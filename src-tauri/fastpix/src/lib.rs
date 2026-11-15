/// In-place X11 ZPixmap (BGRA/BGRX, LSB first) to opaque RGBA.
/// Kept in its own crate so dev builds can compile just this loop at opt-level 3.
pub fn bgra_to_rgba(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 255;
    }
}

/// RGB triples on a `step`-pixel grid, read straight from BGRA.
pub fn grid_rgb(bgra: &[u8], width: u32, height: u32, step: u32) -> Vec<u8> {
    let step = step.max(1) as usize;
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity((w / step + 1) * (h / step + 1) * 3);
    let mut y = 0;
    while y < h {
        let row = y.saturating_mul(w).saturating_mul(4);
        let mut x = 0;
        while x < w {
            let i = row + x * 4;
            if i + 2 < bgra.len() {
                out.push(bgra[i + 2]);
                out.push(bgra[i + 1]);
                out.push(bgra[i]);
            }
            x += step;
        }
        y += step;
    }
    out
}

/// How many grid samples differ by more than 16 in any RGB channel.
pub fn rgb_diff_count(a: &[u8], b: &[u8]) -> Option<u32> {
    if a.len() != b.len() || a.len() % 3 != 0 {
        return None;
    }
    Some(
        a.chunks_exact(3)
            .zip(b.chunks_exact(3))
            .filter(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > 16))
            .count() as u32,
    )
}
