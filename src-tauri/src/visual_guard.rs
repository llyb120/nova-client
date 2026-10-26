//! Shared screenshot delivery and bounded pixel-only stale-target checks. No OCR or object recognition.
use xcap::image::RgbaImage;

/// Half-open rectangle in original image pixels, never display/cropped-image coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct PixelRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}
impl PixelRect {
    pub(crate) fn contains(self, x: f64, y: f64) -> bool {
        x >= self.left as f64 && x < self.right as f64
            && y >= self.top as f64 && y < self.bottom as f64
    }
}

#[derive(Clone)]
pub(crate) struct VisualGuard {
    width: u32,
    height: u32,
    source: (u32, u32),
    step: u32,
    pixels: std::sync::Arc<Vec<[u8; 3]>>,
    unobserved: Vec<PixelRect>,
}
impl VisualGuard {
    // At most 1600^2 RGB samples (~7.7 MB); clones share the backing storage.
    pub(crate) fn new(image: &RgbaImage) -> Self {
        let step = image.width().max(image.height()).div_ceil(1600).max(1);
        let width = image.width().div_ceil(step);
        let height = image.height().div_ceil(step);
        let pixels = (0..height).flat_map(|y| (0..width).map(move |x| {
            let p = image.get_pixel(x * step, y * step);
            [p[0], p[1], p[2]]
        })).collect();
        Self { width, height, source: image.dimensions(), step, pixels: std::sync::Arc::new(pixels), unobserved: Vec::new() }
    }

    /// Delivery exclusions may have refreshed without reaching the model; require a fresh crop before input there.
    pub(crate) fn mark_unobserved(&mut self, regions: Vec<PixelRect>) { self.unobserved = regions; }

    /// `core_only`: the batch's own earlier input legitimately changed the neighbourhood
    /// (typed text, ticked checkbox); only an overlay or shift right under the target counts.
    pub(crate) fn changed_near(&self, current: &RgbaImage, x: f64, y: f64, core_only: bool) -> bool {
        self.changed_near_excluding(current, x, y, core_only, None)
    }

    /// The caller must verify that the excluded overlay's geometry has not changed.
    pub(crate) fn changed_near_excluding(&self, current: &RgbaImage, x: f64, y: f64, core_only: bool, exclude: Option<PixelRect>) -> bool {
        if current.dimensions() != self.source || !x.is_finite() || !y.is_finite()
            || x < 0. || y < 0. || x >= self.source.0 as f64 || y >= self.source.1 as f64
            || self.unobserved.iter().any(|r| r.contains(x, y))
            || exclude.is_some_and(|r| r.contains(x, y)) {
            return true;
        }
        let cx = (x / self.step as f64) as i64;
        let cy = (y / self.step as f64) as i64;
        let (mut total, mut changed, mut core, mut core_changed) = (0, 0, 0, 0);
        for dy in -24i64..=24 {
            for dx in -24i64..=24 {
                let (px, py) = (cx + dx, cy + dy);
                if px < 0 || py < 0 || px >= self.width as i64 || py >= self.height as i64 { continue; }
                let (sx, sy) = (px as u32 * self.step, py as u32 * self.step);
                if exclude.is_some_and(|r| r.contains(sx as f64, sy as f64)) { continue; }
                let old = self.pixels[(py * self.width as i64 + px) as usize];
                let new = current.get_pixel(sx, sy);
                let different = (0..3).any(|c| old[c].abs_diff(new[c]) > 24);
                total += 1;
                changed += u32::from(different);
                if dx.abs() <= 4 && dy.abs() <= 4 { core += 1; core_changed += u32::from(different); }
            }
        }
        // Ignore small anti-aliasing/caret noise and unrelated animation elsewhere.
        // A changed centre or substantially changed neighbourhood requires a new observation.
        total == 0 || core == 0 || (!core_only && changed * 100 > total * 12) || core_changed * 100 > core * 35
    }
}

/// 截图统一存无损 WebP，保留 RGBA 像素，避免压缩噪声影响落点校验。
pub(crate) fn encode_webp(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    xcap::image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
        .encode(image.as_raw(), image.width(), image.height(), xcap::image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Delivered comparison pixels before cursor annotation; ignored self-UI regions may be masked.
#[derive(Clone)]
pub(crate) struct Delivered {
    pub stream: String,
    pub group: String,
    pub keyframe_id: String,
    pub basis: serde_json::Value,
    pub keyframe_path: std::path::PathBuf,
    pub seen: std::sync::Arc<RgbaImage>,
    /// Saved patches in delivery order; partially overwritten patches remain for reconstruction only.
    pub tiles: Vec<DeliveredTile>,
    pub deltas: u8,
}

#[derive(Clone)]
pub(crate) struct DeliveredTile {
    pub rect: PixelRect,
    pub path: std::path::PathBuf,
    pub revision: u8,
    pub read_only: bool,
}

pub(crate) const MAX_DELTAS: u8 = 8;
const TILE: u32 = 32;
pub(crate) enum Delivery { Unchanged, Delta(Vec<PixelRect>), Keyframe }

/// Send only grid tiles changed since their last delivery. Earlier tiles remain valid independently.
#[cfg(test)]
pub(crate) fn plan_delivery(prior: Option<&Delivered>, pixels: &RgbaImage) -> Delivery {
    plan_grid_delivery(prior, pixels, (TILE, TILE))
}

fn plan_grid_delivery(prior: Option<&Delivered>, pixels: &RgbaImage, tile: (u32, u32)) -> Delivery {
    let Some(p) = prior.filter(|p| p.seen.dimensions() == pixels.dimensions()) else { return Delivery::Keyframe };
    let Some(changed) = changed_grid(&p.seen, pixels, None, tile) else { return Delivery::Keyframe };
    if !changed.contains(&true) { return Delivery::Unchanged; }
    if p.deltas >= MAX_DELTAS { return Delivery::Keyframe; }
    Delivery::Delta(merge_grid(&changed, pixels.dimensions(), tile))
}

/// Merge horizontal runs, then vertically extend matching runs. Never include an unchanged grid cell.
#[cfg(test)]
fn merge_changed_tiles(changed: &[bool], size: (u32, u32)) -> Vec<PixelRect> {
    merge_grid(changed, size, (TILE, TILE))
}

fn merge_grid(changed: &[bool], (w, h): (u32, u32), (tw, th): (u32, u32)) -> Vec<PixelRect> {
    let cols = w.div_ceil(tw) as usize;
    let mut blocks: Vec<PixelRect> = Vec::new();
    let mut previous: std::collections::HashMap<(usize, usize), usize> = std::collections::HashMap::new();
    for (y, row) in changed.chunks(cols).enumerate() {
        let mut active = std::collections::HashMap::new();
        let mut x = 0;
        while x < row.len() {
            if !row[x] { x += 1; continue; }
            let start = x;
            while x < row.len() && row[x] { x += 1; }
            let bottom = ((y as u32 + 1) * th).min(h);
            let index = if let Some(&index) = previous.get(&(start, x)) {
                blocks[index].bottom = bottom;
                index
            } else {
                blocks.push(PixelRect { left: start as u32 * tw, top: y as u32 * th,
                    right: (x as u32 * tw).min(w), bottom });
                blocks.len() - 1
            };
            active.insert((start, x), index);
        }
        previous = active;
    }
    blocks
}

/// Which 32×32 blocks visibly changed (any RGB channel > 16).
#[cfg(test)]
pub(crate) fn changed_tiles(a: &RgbaImage, b: &RgbaImage, exclude: Option<PixelRect>) -> Option<Vec<bool>> {
    changed_grid(a, b, exclude, (TILE, TILE))
}

fn changed_grid(a: &RgbaImage, b: &RgbaImage, exclude: Option<PixelRect>, (tw, th): (u32, u32)) -> Option<Vec<bool>> {
    let (w, h) = a.dimensions();
    if b.dimensions() != (w, h) || w == 0 || h == 0 { return None; }
    let cols = w.div_ceil(tw) as usize;
    let mut tiles = vec![false; cols * h.div_ceil(th) as usize];
    let (ra, rb) = (a.as_raw(), b.as_raw());
    // memcmp before per-pixel scans keeps unoptimized NovaDev builds fast.
    if ra == rb { return Some(tiles); }
    for y in 0..h as usize { for col in 0..cols {
        let t = y / th as usize * cols + col;
        let x0 = col * tw as usize;
        let span = (y * w as usize + x0) * 4..(y * w as usize + (x0 + tw as usize).min(w as usize)) * 4;
        if tiles[t] || ra[span.clone()] == rb[span.clone()] { continue; }
        tiles[t] = ra[span.clone()].chunks_exact(4).zip(rb[span].chunks_exact(4)).enumerate().any(|(i, (p, q))| {
            (0..3).any(|c| p[c].abs_diff(q[c]) > 16)
                && !exclude.is_some_and(|r| r.contains((x0 + i) as f64, y as f64))
        });
    } }
    Some(tiles)
}

/// Shared desktop/browser delivery. No `path` means no repeated attachment. The base entry describes
/// the complete image; callers may register delta IDs that map local crop coordinates automatically.
pub(crate) fn deliver_frame(entry: &mut serde_json::Value, prior: Option<&Delivered>, stream: Option<String>,
    plain: RgbaImage, annotated: Option<&RgbaImage>, path: std::path::PathBuf)
    -> Result<(Option<Delivered>, Vec<serde_json::Value>), String> {
    use serde_json::json;
    let path = path.with_extension("webp");
    let mark = |entry: &mut serde_json::Value, p: &Delivered| {
        entry["frameStream"] = json!(p.stream);
        entry["frameGroup"] = json!(p.group);
        entry["keyframeImageId"] = json!(p.keyframe_id);
        entry["keyframePath"] = json!(p.keyframe_path);
    };
    let base_id = entry["imageId"].clone();
    let delta_entry = |p: &Delivered, tile: &DeliveredTile| {
        let b = tile.rect;
        let mut delta = json!({"deltaOf":base_id,"path":tile.path,"x":b.left,"y":b.top,"tileRevision":tile.revision,
            "width":b.right-b.left,"height":b.bottom-b.top,"pixelWidth":b.right-b.left,"pixelHeight":b.bottom-b.top});
        if tile.read_only { delta["readOnly"] = json!(true); }
        mark(&mut delta, p);
        delta
    };
    // Equal resized dimensions alone do not prove the same monitor geometry or source scale.
    // Changed exclusion geometry must reveal newly exposed areas with a fresh, unmasked keyframe.
    let basis = json!([entry["desktopBounds"], entry["originalWidth"], entry["originalHeight"], entry["ignoredRegions"], entry["deltaPolicy"]]);
    let prior = prior.filter(|p| Some(&p.stream) == stream.as_ref() && p.basis == basis);
    let image = annotated.unwrap_or(&plain);
    // Desktop screens and browser viewport/document streams opt into at most 3×3 cells.
    let (tw, th) = if entry["deltaPolicy"] == "nine-grid" {
        (plain.width().div_ceil(3).max(1), plain.height().div_ceil(3).max(1))
    } else { (TILE, TILE) };
    match plan_grid_delivery(prior, &plain, (tw, th)) {
        Delivery::Unchanged => {
            let p = prior.unwrap();
            entry["unchanged"] = json!(true);
            mark(entry, p);
            entry["latestDeltas"] = json!(p.tiles.iter().map(|tile| delta_entry(p, tile)).collect::<Vec<_>>());
            Ok((Some(p.clone()), Vec::new()))
        }
        Delivery::Delta(blocks) => {
            let p = prior.unwrap();
            let mut next = p.clone();
            next.deltas += 1;
            // Merged shapes may change across frames. Keep surviving old portions in order, but
            // never authorize coordinates from a patch partially overwritten by newer patches.
            let cols = plain.width().div_ceil(tw) as usize;
            let mut revisions = vec![0; cols * plain.height().div_ceil(th) as usize];
            for (r, revision) in next.tiles.iter().map(|t| (t.rect, t.revision))
                .chain(blocks.iter().map(|r| (*r, next.deltas))) {
                for y in r.top / th..r.bottom.div_ceil(th) {
                    for x in r.left / tw..r.right.div_ceil(tw) {
                        revisions[y as usize * cols + x as usize] = revision;
                    }
                }
            }
            next.tiles.retain_mut(|tile| {
                let r = tile.rect;
                let (mut current, mut overwritten) = (false, false);
                for y in r.top / th..r.bottom.div_ceil(th) {
                    for x in r.left / tw..r.right.div_ceil(tw) {
                        let newer = revisions[y as usize * cols + x as usize] > tile.revision;
                        current |= !newer;
                        overwritten |= newer;
                    }
                }
                tile.read_only = overwritten;
                current
            });
            let mut seen = (*p.seen).clone();
            let mut deltas = Vec::new();
            for b in blocks {
                let crop = xcap::image::imageops::crop_imm(image, b.left, b.top, b.right-b.left, b.bottom-b.top).to_image();
                let path = path.with_file_name(format!("{}-delta-{}-{}.webp", path.file_stem().unwrap_or_default().to_string_lossy(), b.left, b.top));
                std::fs::write(&path, encode_webp(&crop)?).map_err(|e| e.to_string())?;
                let tile = DeliveredTile { rect: b, path, revision: next.deltas, read_only: false };
                let mut delta = delta_entry(&next, &tile);
                if let (Some(x), Some(y)) = (entry["cursor"]["x"].as_u64(), entry["cursor"]["y"].as_u64()) {
                    if b.contains(x as f64, y as f64) {
                        delta["cursor"] = entry["cursor"].clone();
                        delta["cursor"]["x"] = json!(x - b.left as u64);
                        delta["cursor"]["y"] = json!(y - b.top as u64);
                    }
                }
                deltas.push(delta);
                next.tiles.push(tile);
                // Preserve undelivered pixels so small differences cannot drift forever below the threshold.
                let w = plain.width() as usize;
                let raw: &mut [u8] = seen.as_mut();
                for y in b.top..b.bottom {
                    let range = (y as usize * w + b.left as usize) * 4..(y as usize * w + b.right as usize) * 4;
                    raw[range.clone()].copy_from_slice(&plain.as_raw()[range]);
                }
            }
            next.seen = seen.into();
            mark(entry, &next);
            // Complete tile state permits recovery after any prefix of this group is compacted.
            entry["latestDeltas"] = json!(next.tiles.iter().map(|tile| delta_entry(&next, tile)).collect::<Vec<_>>());
            Ok((Some(next), deltas))
        }
        Delivery::Keyframe => {
            std::fs::write(&path, encode_webp(image)?).map_err(|e| e.to_string())?;
            entry["path"] = json!(path);
            let delivered = stream.map(|stream| {
                let plain = std::sync::Arc::new(plain);
                let p = Delivered { stream, group: entry["snapshotId"].as_str().unwrap_or_default().into(),
                    keyframe_id: entry["imageId"].as_str().unwrap_or_default().into(), basis,
                    keyframe_path: path, seen: plain, tiles: Vec::new(), deltas: 0 };
                entry["keyframe"] = json!(true);
                mark(entry, &p);
                p
            });
            Ok((delivered, Vec::new()))
        }
    }
}

pub(crate) fn patch_changed(before: &RgbaImage, after: &RgbaImage) -> bool {
    if before.width() == 0 || before.height() == 0 || after.width() == 0 || after.height() == 0 { return true; }
    let after = if before.dimensions() == after.dimensions() { after.clone() } else {
        xcap::image::imageops::resize(after, before.width(), before.height(), xcap::image::imageops::FilterType::Triangle)
    };
    VisualGuard::new(before).changed_near(&after, before.width() as f64 / 2., before.height() as f64 / 2., false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcap::image::Rgba;
    #[test]
    fn delivery_compares_plain_pixels_and_keeps_cursor_in_crop_coordinates() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(320, 320, Rgba([255, 255, 255, 255]));
        let entry = |id| json!({"imageId":"window-1","snapshotId":id,"cursor":{"x":150,"y":150,"marker":"sword-tip"}});
        let mut first = entry("key");
        let (prior, _) = deliver_frame(&mut first, None, Some("window".into()), page.clone(), None, dir.path().join("key.webp")).unwrap();
        let path = std::path::Path::new(first["path"].as_str().unwrap());
        assert_eq!(path.extension().and_then(|s| s.to_str()), Some("webp"));
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(xcap::image::guess_format(&bytes).unwrap(), xcap::image::ImageFormat::WebP);
        assert_eq!(xcap::image::load_from_memory(&bytes).unwrap().to_rgba8(), page);
        let mut annotated = page.clone();
        annotated.put_pixel(150, 150, Rgba([0, 100, 255, 255]));
        let mut same = entry("same");
        let (_, delta) = deliver_frame(&mut same, prior.as_ref(), Some("window".into()), page.clone(), Some(&annotated), dir.path().join("same.webp")).unwrap();
        assert!(same["unchanged"] == true && delta.is_empty(), "cursor annotation alone is not a change");
        let mut changed = page;
        changed.put_pixel(150, 150, Rgba([0, 0, 0, 255]));
        let mut next = entry("delta");
        let (seen, delta) = deliver_frame(&mut next, prior.as_ref(), Some("window".into()), changed.clone(), Some(&annotated), dir.path().join("delta.webp")).unwrap();
        assert_eq!(delta.len(), 1);
        let path = std::path::Path::new(delta[0]["path"].as_str().unwrap());
        assert_eq!(path.extension().and_then(|s| s.to_str()), Some("webp"));
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(xcap::image::guess_format(&bytes).unwrap(), xcap::image::ImageFormat::WebP);
        assert_eq!(xcap::image::load_from_memory(&bytes).unwrap().to_rgba8(),
            xcap::image::imageops::crop_imm(&annotated, 128, 128, 32, 32).to_image());
        assert_eq!((delta[0]["cursor"]["x"].as_u64(), delta[0]["cursor"]["y"].as_u64()), (Some(22), Some(22)));
        assert_eq!(seen.unwrap().seen.as_ref(), &changed, "guard pixels never contain the cursor marker");
    }

    #[test]
    fn tile_delivery_sends_only_new_blocks_and_restores_reverts() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(800, 600, Rgba([255,255,255,255]));
        let deliver = |id: &str, prior: Option<&Delivered>, pixels: RgbaImage| {
            let mut entry = json!({"imageId":id,"snapshotId":id});
            let (frame, blocks) = deliver_frame(&mut entry, prior, Some("screen".into()), pixels, None, dir.path().join(format!("{id}.webp"))).unwrap();
            (entry, frame.unwrap(), blocks)
        };
        let (_, key, _) = deliver("key", None, page.clone());
        let mut pixels = page.clone();
        pixels.put_pixel(16,16,Rgba([0,0,0,255]));
        let (_, first, a) = deliver("a", Some(&key), pixels.clone());
        pixels.put_pixel(799,599,Rgba([0,0,0,255]));
        let (entry, second, b) = deliver("b", Some(&first), pixels.clone());
        assert_eq!((a.len(),b.len(),entry["latestDeltas"].as_array().unwrap().len()),(1,1,2));
        assert_eq!((b[0]["x"].as_u64(),b[0]["y"].as_u64(),b[0]["width"].as_u64(),b[0]["height"].as_u64()),(Some(768),Some(576),Some(32),Some(24)));
        assert_eq!(entry["latestDeltas"][0]["path"],a[0]["path"]);
        assert_eq!(entry["latestDeltas"][0]["deltaOf"],"b");
        let (same, _, blocks) = deliver("same", Some(&second), pixels.clone());
        assert!(blocks.is_empty() && same["unchanged"] == true);
        pixels.put_pixel(16,16,*page.get_pixel(16,16));
        let (entry, reverted, blocks) = deliver("revert", Some(&second), pixels.clone());
        assert_eq!(blocks.len(),1);
        assert_eq!((blocks[0]["x"].as_u64(),blocks[0]["y"].as_u64()),(Some(0),Some(0)));
        assert_eq!(entry["latestDeltas"][0]["path"],b[0]["path"]);
        assert_eq!(entry["latestDeltas"][1]["path"],blocks[0]["path"]);
        assert_ne!(entry["latestDeltas"][1]["path"],a[0]["path"]);
        assert_eq!(reverted.seen.as_ref(),&pixels);
        let saved = xcap::image::open(blocks[0]["path"].as_str().unwrap()).unwrap().to_rgba8();
        assert_eq!(saved.get_pixel(16,16), page.get_pixel(16,16));
        // Reconstruct with the latest saved tiles only: no distant unchanged gap was sent.
        let area: u32 = reverted.tiles.iter().map(|t|(t.rect.right-t.rect.left)*(t.rect.bottom-t.rect.top)).sum();
        assert_eq!(area,32*32+32*24);
    }

    #[test]
    fn nine_grid_covers_edges_merges_in_order_and_retains_partial_patches() {
        use serde_json::json;
        let size = (11, 8);
        let tile = (4, 3);
        for mask in 0..512 {
            let changed: Vec<_> = (0..9).map(|i| mask & (1 << i) != 0).collect();
            let blocks = merge_grid(&changed, size, tile);
            assert!(blocks.len() <= 9);
            for y in 0..size.1 { for x in 0..size.0 {
                assert_eq!(blocks.iter().filter(|r| r.contains(x as f64, y as f64)).count(),
                    usize::from(changed[(y / tile.1 * 3 + x / tile.0) as usize]));
            } }
        }
        let dir = tempfile::tempdir().unwrap();
        let deliver = |id: &str, prior: Option<&Delivered>, image: RgbaImage| {
            let mut entry = json!({"imageId":"monitor-1","snapshotId":id,"deltaPolicy":"nine-grid"});
            let (frame, blocks) = deliver_frame(&mut entry, prior, Some("monitor-1".into()), image, None, dir.path().join(format!("{id}.webp"))).unwrap();
            (entry, frame.unwrap(), blocks)
        };
        let mut page = RgbaImage::from_pixel(size.0, size.1, Rgba([255,255,255,255]));
        let (_, base, _) = deliver("base", None, page.clone());
        for (x,y) in [(0,0),(4,0),(0,3),(4,3),(10,7)] { page.put_pixel(x,y,Rgba([0,0,0,255])); }
        let (_, first, blocks) = deliver("first", Some(&base), page.clone());
        assert_eq!(blocks.len(), 2, "horizontal runs merge vertically only when widths match");
        assert_eq!(first.tiles[0].rect, PixelRect { left:0, top:0, right:8, bottom:6 });
        assert_eq!(first.tiles[1].rect, PixelRect { left:8, top:6, right:11, bottom:8 });
        page.put_pixel(4,0,Rgba([255,255,255,255]));
        let (_, second, blocks) = deliver("second", Some(&first), page.clone());
        assert_eq!(blocks.len(), 1);
        assert!(second.tiles[0].read_only, "partially overwritten patches are reconstruction-only");
        assert!(!second.tiles[1].read_only, "an unrelated corner keeps its revision");
        assert_eq!(second.seen.as_ref(), &page);
        let (same, _, blocks) = deliver("same", Some(&second), page.clone());
        assert!(same["unchanged"] == true && blocks.is_empty());
        let black = RgbaImage::from_pixel(size.0, size.1, Rgba([0,0,0,255]));
        let (_, full, blocks) = deliver("full", Some(&second), black);
        assert_eq!(blocks.len(), 1);
        assert_eq!(full.tiles.len(), 1, "fully overwritten old patches are removed");
        for size in [(1,1),(2,2),(641,479)] {
            let blank = RgbaImage::new(size.0,size.1);
            let mut edge = blank.clone();
            edge.put_pixel(size.0-1,size.1-1,Rgba([255,255,255,255]));
            let grid = (size.0.div_ceil(3).max(1),size.1.div_ceil(3).max(1));
            let cells = changed_grid(&blank,&edge,None,grid).unwrap();
            assert!(cells.len() <= 9 && cells.last() == Some(&true));
        }
    }

    #[test]
    fn merged_tiles_cover_exactly_the_changed_cells() {
        let size = (70,65); // clipped tiles at the right and bottom edges
        assert_eq!(merge_changed_tiles(&[true;9],size),vec![PixelRect{left:0,top:0,right:70,bottom:65}]);
        assert_eq!(merge_changed_tiles(&[true,true,false,true,true,false,false,false,false],size),
            vec![PixelRect{left:0,top:0,right:64,bottom:64}]);
        for mask in 0..512 {
            let changed: Vec<bool> = (0..9).map(|i| mask & (1<<i) != 0).collect();
            let blocks = merge_changed_tiles(&changed,size);
            for r in &blocks {
                assert!(r.left < r.right && r.top < r.bottom && r.right <= size.0 && r.bottom <= size.1);
            }
            for (i, &changed) in changed.iter().enumerate() {
                let (x,y)=((i%3) as f64*32.,(i/3) as f64*32.);
                assert_eq!(blocks.iter().filter(|r|r.contains(x,y)).count(),usize::from(changed),
                    "mask={mask}: gaps/diagonal neighbours must not be filled and patches must not overlap");
            }
        }
    }

    #[test]
    fn merged_patches_preserve_partial_updates_in_delivery_order() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let blank = RgbaImage::from_pixel(128,64,Rgba([255,255,255,255]));
        let mut pixels = RgbaImage::from_fn(128,64,|x,y| if x<96 && y<32 { Rgba([0,0,0,255]) } else { Rgba([255,255,255,255]) });
        let deliver = |id: &str, prior: Option<&Delivered>, image: &RgbaImage| {
            let mut entry=json!({"imageId":"monitor-1","snapshotId":id});
            let (frame,blocks)=deliver_frame(&mut entry,prior,Some("screen".into()),image.clone(),None,dir.path().join(format!("{id}.webp"))).unwrap();
            (entry,frame.unwrap(),blocks)
        };
        let (_,key,_) = deliver("key",None,&blank);
        let (_,first,blocks) = deliver("merged",Some(&key),&pixels);
        assert_eq!(blocks.len(),1);
        assert_eq!((blocks[0]["width"].as_u64(),blocks[0]["height"].as_u64()),(Some(96),Some(32)));
        for y in 0..32 { for x in 32..64 { pixels.put_pixel(x,y,Rgba([255,255,255,255])); } }
        let (entry,second,blocks) = deliver("middle",Some(&first),&pixels);
        assert_eq!(blocks.len(),1);
        assert_eq!(entry["latestDeltas"][0]["readOnly"],true);
        assert_eq!(second.tiles.len(),2);
        let mut restored=blank.clone();
        for tile in &second.tiles {
            let patch=xcap::image::open(&tile.path).unwrap().to_rgba8();
            xcap::image::imageops::replace(&mut restored,&patch,tile.rect.left as i64,tile.rect.top as i64);
        }
        assert_eq!(pixels,restored,"restoration must apply newer overlap last without pixel loss");
        let (entry,third,blocks) = deliver("sides",Some(&second),&blank);
        assert_eq!(blocks.len(),2,"unchanged middle must not be included");
        assert_eq!(third.tiles.len(),3);
        assert!(third.tiles.iter().all(|t|t.revision>1 && !t.read_only),"fully covered original patch must be discarded");
        assert!(entry["latestDeltas"].as_array().unwrap().windows(2).all(|pair|pair[0]["tileRevision"].as_u64()<=pair[1]["tileRevision"].as_u64()));
        let (entry,_,blocks) = deliver("same",Some(&third),&blank);
        assert!(blocks.is_empty() && entry["unchanged"]==true);
    }

    #[test]
    fn webp_roundtrip_preserves_pixels_and_detects_visual_changes() {
        // 密集细字、彩色控件及透明度均须逐像素保真。
        let source = RgbaImage::from_fn(240, 140, |x, y| {
            if (y / 12) % 2 == 0 && (x % 5 == 0 || (x + y) % 7 == 0) { Rgba([20, 20, 20, 255]) }
            else if (100..180).contains(&x) && (90..120).contains(&y) { Rgba([30, 110, 230, 128]) }
            else { Rgba([250, 250, 250, ((x + y) % 256) as u8]) }
        });
        let decoded = xcap::image::load_from_memory(&encode_webp(&source).unwrap()).unwrap().to_rgba8();
        assert_eq!(decoded, source);
        assert!(!patch_changed(&decoded, &source));
        let mut moved = source.clone();
        for y in 0..140 { for x in 0..240 { moved.put_pixel(x, y, *source.get_pixel((x + 3) % 240, y)); } }
        assert!(patch_changed(&decoded, &moved), "real content shifts must still be detected");
    }

    #[test]
    fn stale_targets_not_background_animation_or_caret_noise() {
        let original = RgbaImage::from_pixel(240, 140, Rgba([80, 80, 80, 255]));
        let guard = VisualGuard::new(&original);
        let mut current = original.clone();
        for y in 0..40 { for x in 180..240 { current.put_pixel(x,y,Rgba([255,0,0,255])); } }
        assert!(!guard.changed_near(&current, 30., 60., false));
        for y in 52..68 { current.put_pixel(30,y,Rgba([255,255,255,255])); }
        assert!(!guard.changed_near(&current, 30., 60., false));
        // Typed text beside the target: stale for a fresh batch, fine after the batch's own input.
        let mut typed = original.clone();
        for y in 40..56 { for x in 10..60 { typed.put_pixel(x,y,Rgba([0,0,0,255])); } }
        assert!(guard.changed_near(&typed, 30., 60., false));
        assert!(!guard.changed_near(&typed, 30., 60., true));
        for y in 56..65 { for x in 26..35 { current.put_pixel(x,y,Rgba([0,255,0,255])); } }
        assert!(guard.changed_near(&current, 30., 60., false));
        assert!(guard.changed_near(&current, 30., 60., true));
        assert!(guard.changed_near(&current, f64::NAN, 60., false));
        assert!(guard.changed_near(&RgbaImage::new(240,141),30.,60.,false));
    }
    #[test]
    fn explicit_exclusion_keeps_nearby_targets_guarded() {
        let original = RgbaImage::from_pixel(100, 100, Rgba([20,20,20,255]));
        let guard = VisualGuard::new(&original);
        let rect = PixelRect { left:50, top:20, right:80, bottom:80 };
        let mut current = original.clone();
        for y in rect.top..rect.bottom { for x in rect.left..rect.right {
            current.put_pixel(x, y, Rgba([220,220,220,255]));
        } }
        assert!(guard.changed_near(&current, 48., 50., false));
        for core_only in [false, true] {
            assert!(!guard.changed_near_excluding(&current, 48., 50., core_only, Some(rect)));
            assert!(guard.changed_near_excluding(&current, 50., 50., core_only, Some(rect)));
        }
        for y in 46..55 { for x in 44..50 { current.put_pixel(x, y, Rgba([200,0,0,255])); } }
        assert!(guard.changed_near_excluding(&current, 48., 50., false, Some(rect)));
        assert!(guard.changed_near_excluding(&current, 48., 50., true, Some(rect)));
        // No exemptions for the browser caller or an image whose dimensions changed.
        assert!(patch_changed(&original, &current));
        assert!(guard.changed_near_excluding(&RgbaImage::new(101,100), 48., 50., false, Some(rect)));
    }
    #[test]
    fn high_dpi_and_image_boundaries_are_bounded() {
        let original=RgbaImage::from_pixel(3840,2160,Rgba([15,15,15,255]));
        let guard=VisualGuard::new(&original);
        assert!(guard.pixels.len() <= 1600 * 1600);
        assert!(!guard.changed_near(&original,3839.,2159.,false));
        assert!(guard.changed_near(&original,3840.,2159.,false));
        let a=RgbaImage::from_pixel(48,48,Rgba([15,15,15,255]));
        let b=RgbaImage::from_pixel(96,96,Rgba([15,15,15,255]));
        assert!(!patch_changed(&a,&b));
        assert!(patch_changed(&a,&RgbaImage::from_pixel(48,48,Rgba([255,255,255,255]))));
    }
}
