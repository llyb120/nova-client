//! 读取系统剪贴板里的文件路径（资源管理器 / Finder 复制的文件）。
//!
//! WebView2 把 Ctrl+Shift+V 映射为「粘贴为纯文本」，JS `paste` 事件通常没有 `File.path`，
//! 也往往不再带 `kind=file` 项。前端需要走原生 CF_HDROP。

pub fn file_paths() -> Vec<String> {
    #[cfg(windows)]
    {
        windows_hdrop_paths()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
fn windows_hdrop_paths() -> Vec<String> {
    use std::ptr;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    const CF_HDROP: u32 = 15;

    unsafe {
        let mut opened = false;
        for _ in 0..8 {
            if OpenClipboard(ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !opened {
            return Vec::new();
        }
        let handle = GetClipboardData(CF_HDROP);
        if handle.is_null() {
            CloseClipboard();
            return Vec::new();
        }
        let count = DragQueryFileW(handle, 0xFFFF_FFFF, ptr::null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let len = DragQueryFileW(handle, index, ptr::null_mut(), 0) as usize;
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            let written = DragQueryFileW(handle, index, buf.as_mut_ptr(), buf.len() as u32) as usize;
            if written == 0 {
                continue;
            }
            if let Ok(path) = String::from_utf16(&buf[..written]) {
                if !path.is_empty() {
                    paths.push(path);
                }
            }
        }
        CloseClipboard();
        paths
    }
}
