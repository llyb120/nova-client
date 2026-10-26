//! Ignore visible Nova/Butterfly refreshes and the native taskbar clock, never foreign windows.
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

/// Keep only areas excluded both before and after capture. Changes on another screen
/// or a different enumeration order must not discard this screen's stable masks.
pub(super) fn stable(before: &[Rect], after: &[Rect]) -> Vec<Rect> {
    let mut regions = Vec::new();
    for a in before { for b in after {
        let r = Rect { left:a.left.max(b.left), top:a.top.max(b.top), right:a.right.min(b.right), bottom:a.bottom.min(b.bottom) };
        if r.left < r.right && r.top < r.bottom { regions.push(r); }
        // ponytail: bounded mask fragmentation; excess regions fall back to observed pixels.
        if regions.len() > 1024 { return Vec::new(); }
    } }
    regions.sort_unstable_by_key(|r| (r.left,r.top,r.right,r.bottom));
    regions.dedup();
    regions
}

#[cfg(not(windows))]
pub(super) fn visible() -> Vec<Rect> { Vec::new() }

#[cfg(windows)]
pub(super) fn visible() -> Vec<Rect> {
    use windows_sys::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS},
        UI::{HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2}, WindowsAndMessaging::*},
    };
    unsafe fn children(parent: HWND, bounds: Rect, above: &[Rect], nova_pids: &[u32], shell_pid: u32, remaining: &mut usize) -> Option<Vec<Rect>> {
        let mut covers = above.to_vec();
        let mut ignored = Vec::new();
        let mut child = GetTopWindow(parent);
        while !child.is_null() && *remaining > 0 {
            *remaining -= 1;
            if IsWindowVisible(child) != 0 {
                let mut r = RECT::default();
                if GetWindowRect(child, &mut r) == 0 { return None; }
                let mut class = [0u16; 64];
                let len = GetClassNameW(child, class.as_mut_ptr(), class.len() as i32);
                let mut pid = 0;
                if len <= 0 || GetWindowThreadProcessId(child, &mut pid) == 0 { return None; }
                let class = String::from_utf16_lossy(&class[..len as usize]);
                let rect = Rect { left:r.left.max(bounds.left), top:r.top.max(bounds.top), right:r.right.min(bounds.right), bottom:r.bottom.min(bounds.bottom) };
                if rect.left < rect.right && rect.top < rect.bottom {
                    if (nova_pids.contains(&pid) && class == "Nova.TaskbarStatus") || (pid == shell_pid && class == "TrayClockWClass") {
                        let mut parts = vec![rect];
                        for cover in &covers {
                            parts = subtract(parts, *cover);
                            if parts.len() + ignored.len() > 1024 { return None; }
                            if parts.is_empty() { break; }
                        }
                        ignored.extend(parts);
                    } else if pid == shell_pid && class == "TrayNotifyWnd" {
                        // The native clock is nested here; never mask the whole notification area.
                        ignored.extend(children(child, rect, &covers, nova_pids, shell_pid, remaining)?);
                    }
                    covers.push(rect);
                }
            }
            child = GetWindow(child, GW_HWNDNEXT);
        }
        Some(ignored)
    }
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if previous.is_null() { return Vec::new(); }
        let regions = (|| {
            let mut pid = 0;
            // Butterfly is a native Explorer child. Its PID also identifies peer Nova/Dev windows.
            let taskbar = FindWindowW(windows_sys::core::w!("Shell_TrayWnd"), std::ptr::null());
            let mut shell_pid = 0;
            GetWindowThreadProcessId(taskbar, &mut shell_pid);
            let mut nova_pids = vec![std::process::id()];
            let mut chip = std::ptr::null_mut();
            if !taskbar.is_null() {
                // ponytail: at most 16 simultaneous Nova instances; use a process registry if exceeded.
                for _ in 0..16 {
                    chip = FindWindowExW(taskbar, chip, windows_sys::core::w!("Nova.TaskbarStatus"), std::ptr::null());
                    if chip.is_null() { break; }
                    if GetWindowThreadProcessId(chip, &mut pid) != 0 && !nova_pids.contains(&pid) { nova_pids.push(pid); }
                }
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
                        // Invisible resize borders must not hide the visible taskbar below maximized apps.
                        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut bounds as *mut _ as _, std::mem::size_of::<RECT>() as u32) < 0
                            && GetWindowRect(hwnd, &mut bounds) == 0 { return None; }
                        if GetWindowThreadProcessId(hwnd, &mut pid) == 0 { return None; }
                        let mut affinity = 0;
                        let capture_excluded = GetWindowDisplayAffinity(hwnd, &mut affinity) != 0 && affinity == WDA_EXCLUDEFROMCAPTURE;
                        // Nova's full-screen glow is absent from capture, so it must not occlude the comparison mask.
                        if !capture_excluded && bounds.left < bounds.right && bounds.top < bounds.bottom {
                            // Transparent/layered overlays block exclusions, but are never themselves ignored.
                            let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                            if nova_pids.contains(&pid) && style & (WS_EX_LAYERED | WS_EX_TRANSPARENT) == 0 {
                                let mut parts = vec![Rect { left:bounds.left, top:bounds.top, right:bounds.right, bottom:bounds.bottom }];
                                for cover in &above {
                                    parts = subtract(parts, *cover);
                                    if parts.len() + ignored.len() > 1024 { return None; }
                                    if parts.is_empty() { break; }
                                }
                                ignored.extend(parts);
                            }
                            if hwnd == taskbar {
                                // ponytail: 256 native taskbar descendants; unvisited UI stays observed.
                                let rect = Rect { left:bounds.left, top:bounds.top, right:bounds.right, bottom:bounds.bottom };
                                ignored.extend(children(hwnd, rect, &above, &nova_pids, shell_pid, &mut 256)?);
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
    fn stable_masks_do_not_depend_on_other_screens_or_enumeration_order() {
        let chip = Rect { left:1290,top:1046,right:1590,bottom:1074 };
        let clock = Rect { left:1794,top:1040,right:1867,bottom:1080 };
        let nova = Rect { left:-1600,top:100,right:-200,bottom:900 };
        assert_eq!(stable(&[nova,chip,clock],&[clock,chip]),vec![chip,clock]);
        assert_eq!(stable(&[chip,clock],&[clock,chip]),vec![chip,clock]);
        let menu = Rect { left:1400,top:1000,right:1500,bottom:1080 };
        let visible = subtract(vec![chip],menu);
        assert_eq!(stable(&[chip],&visible).iter().map(|r|(r.right-r.left)*(r.bottom-r.top)).sum::<i32>(),200*28);
        assert!(stable(&[chip],&[]).is_empty(),"a disappeared mask must expose new content");
    }

    #[test]
    fn native_mask_ignores_self_refresh_without_hiding_neighbouring_pixels() {
        use xcap::image::{imageops::{resize, FilterType}, Rgba, RgbaImage};
        use crate::visual_guard::PixelRect;
        let page = RgbaImage::from_pixel(1920,1080,Rgba([255,255,255,255]));
        let chip = PixelRect { left:1290,top:1046,right:1590,bottom:1074 };
        let clock = PixelRect { left:1794,top:1040,right:1867,bottom:1080 };
        let mut refresh = page.clone();
        for r in [chip,clock] { for y in r.top..r.bottom { for x in r.left..r.right { refresh.put_pixel(x,y,Rgba([0,0,0,255])); } } }
        let compare = |image: &RgbaImage| resize(&comparison_pixels(image,&[chip,clock]),1600,900,FilterType::Triangle);
        assert_eq!(compare(&page),compare(&refresh),"mask before resizing so animation cannot bleed out");
        refresh.put_pixel(chip.left-1,chip.top,Rgba([0,0,0,255]));
        assert_ne!(compare(&page),compare(&refresh));
        assert_eq!(page.get_pixel(chip.left,chip.top)[0],255,"real screenshots remain unmasked");
        let slot = Rect { left:1290,top:1046,right:1590,bottom:1074 };
        let menu = Rect { left:1400,top:1000,right:1500,bottom:1080 };
        assert_eq!(subtract(vec![slot],menu).iter().map(|r|(r.right-r.left)*(r.bottom-r.top)).sum::<i32>(),200*28);
        assert!(subtract(subtract(vec![slot],menu),slot).is_empty(),"overlapping peer chips share one mask");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads the current desktop's window geometry; no input or focus changes"]
    fn live_visible_masks_are_stable() {
        let before = visible();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let after = visible();
        let masks = stable(&before,&after);
        println!("visible masks before={before:?} after={after:?} stable={masks:?}");
        assert!(!masks.is_empty(),"requires a visible Nova/Butterfly or native taskbar clock");
    }

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
