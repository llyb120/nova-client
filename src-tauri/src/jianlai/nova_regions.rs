//! Ignore Nova's own UI refreshes only where it is visibly in front of other windows.
use crate::taskbar_status::Rect;

// Only the comparison copy is masked; screenshots and stale-target guards keep real pixels.
pub(super) fn comparison_pixels(image: &xcap::image::RgbaImage, regions: &[crate::visual_guard::PixelRect]) -> xcap::image::RgbaImage {
    let mut pixels = image.clone();
    let (w, h) = pixels.dimensions();
    let raw: &mut [u8] = pixels.as_mut();
    for r in regions {
        for y in r.top.min(h)..r.bottom.min(h) {
            let start = (y as usize * w as usize + r.left.min(w) as usize) * 4;
            let end = (y as usize * w as usize + r.right.min(w) as usize) * 4;
            if start < end { raw[start..end].fill(0); }
        }
    }
    pixels
}

#[cfg(any(windows, test))]
fn subtract(regions: Vec<Rect>, cover: Rect) -> Vec<Rect> {
    let mut visible = Vec::new();
    for r in regions {
        let (left, top, right, bottom) = (r.left.max(cover.left), r.top.max(cover.top), r.right.min(cover.right), r.bottom.min(cover.bottom));
        if left >= right || top >= bottom { visible.push(r); continue; }
        visible.extend([
            Rect { bottom: top, ..r }, Rect { top: bottom, ..r },
            Rect { top, bottom, right: left, ..r }, Rect { top, bottom, left: right, ..r },
        ].into_iter().filter(|r| r.left < r.right && r.top < r.bottom));
    }
    visible
}

#[cfg(not(windows))]
pub(super) fn visible() -> Vec<Rect> { Vec::new() }

#[cfg(windows)]
pub(super) fn visible() -> Vec<Rect> {
    use windows_sys::Win32::{
        Foundation::{POINT, RECT},
        Graphics::{Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED}, Gdi::ClientToScreen},
        UI::{HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2}, WindowsAndMessaging::*},
    };
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if previous.is_null() { return Vec::new(); }
        let regions = (|| {
            let mut pid = 0;
            if GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) == 0 || pid == std::process::id() {
                return None; // Nova itself may be the target: observe its changes normally.
            }
            let mut above = Vec::new();
            let mut ignored = Vec::new();
            let mut hwnd = GetTopWindow(std::ptr::null_mut());
            // ponytail: inspect at most 256 Z-order entries; lower, unvisited windows stay unmasked.
            // Already collected regions are safe: lower windows cannot occlude them. Cache per-process windows if needed.
            for _ in 0..256 {
                if hwnd.is_null() { return Some(ignored); }
                if IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 {
                    let mut cloaked = 0u32;
                    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as _, 4) < 0 { return None; }
                    if cloaked == 0 {
                        let mut bounds = RECT::default();
                        if GetWindowRect(hwnd, &mut bounds) == 0 || GetWindowThreadProcessId(hwnd, &mut pid) == 0 { return None; }
                        let mut affinity = 0;
                        let capture_excluded = GetWindowDisplayAffinity(hwnd, &mut affinity) != 0 && affinity == WDA_EXCLUDEFROMCAPTURE;
                        // Nova's full-screen glow is absent from capture, so it must not occlude the comparison mask.
                        if !capture_excluded && bounds.left < bounds.right && bounds.top < bounds.bottom {
                            // Transparent/layered overlays block exclusions, but are never themselves ignored.
                            let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                            if pid == std::process::id() && style & (WS_EX_LAYERED | WS_EX_TRANSPARENT) == 0 {
                                let mut client = RECT::default();
                                if GetClientRect(hwnd, &mut client) == 0 { return None; }
                                let mut start = POINT { x: client.left, y: client.top };
                                let mut end = POINT { x: client.right, y: client.bottom };
                                if ClientToScreen(hwnd, &mut start) == 0 || ClientToScreen(hwnd, &mut end) == 0 { return None; }
                                if start.x < end.x && start.y < end.y {
                                    let mut parts = vec![Rect { left: start.x, top: start.y, right: end.x, bottom: end.y }];
                                    for cover in &above {
                                        parts = subtract(parts, *cover);
                                        if parts.len() + ignored.len() > 1024 { return None; }
                                        if parts.is_empty() { break; }
                                    }
                                    ignored.extend(parts);
                                }
                            }
                            above.push(Rect { left: bounds.left, top: bounds.top, right: bounds.right, bottom: bounds.bottom });
                        }
                    }
                }
                hwnd = GetWindow(hwnd, GW_HWNDNEXT);
            }
            Some(ignored)
        })();
        SetThreadDpiAwarenessContext(previous);
        regions.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_target_is_never_ignored() {
        let nova = Rect { left: -200, top: -100, right: 100, bottom: 200 };
        let target = Rect { left: -100, top: 0, right: 50, bottom: 100 };
        let visible = subtract(vec![nova], target);
        assert_eq!(visible.len(), 4);
        assert_eq!(visible.iter().map(|r| (r.right-r.left) * (r.bottom-r.top)).sum::<i32>(), 75_000);
        for r in visible {
            assert!(r.right <= target.left || r.left >= target.right || r.bottom <= target.top || r.top >= target.bottom);
        }
        assert!(subtract(vec![nova], nova).is_empty());
        let outside = Rect { left: 200, top: 200, right: 300, bottom: 300 };
        assert_eq!(subtract(vec![nova], outside), vec![nova]);
    }
}
