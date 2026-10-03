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
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, OpenClipboard,
    };
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
            let written =
                DragQueryFileW(handle, index, buf.as_mut_ptr(), buf.len() as u32) as usize;
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

/// 系统剪贴板变更序号。自动化批次前后比较，只回传本批动作（Ctrl+C/右键复制）新产生的文本，
/// 不读取用户原有剪贴板。
pub fn sequence() -> u32 {
    #[cfg(windows)]
    {
        unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
    }
    // ponytail: 非 Windows 没有无依赖的剪贴板读取，复制结果不回传；需要时引入 arboard 并按内容比对。
    #[cfg(not(windows))]
    {
        0
    }
}

/// `before` 之后复制出的文本（Canvas 表格为 TSV），附在 act 结果的 `clipboard` 字段；未变化/非文本时为 None。
pub fn copied_since(before: u32) -> Option<serde_json::Value> {
    if sequence() == before {
        return None;
    }
    let text = text()?;
    const LIMIT: usize = 100_000;
    let truncated = text.chars().count() > LIMIT;
    let text: String = text.chars().take(LIMIT).collect();
    Some(serde_json::json!({
        "text": text, "truncated": truncated, "rows": text.lines().count(),
        "note": "本批动作复制出的剪贴板文本；表格选区为制表符分隔(TSV)，按行列解析，比看图识别更准"
    }))
}

#[cfg(windows)]
fn text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        let mut opened = false;
        for _ in 0..8 {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !opened {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        let mut text = None;
        if !handle.is_null() {
            let ptr = GlobalLock(handle) as *const u16;
            if !ptr.is_null() {
                let wide = std::slice::from_raw_parts(ptr, GlobalSize(handle) / 2);
                let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
                text = Some(String::from_utf16_lossy(&wide[..end]));
                GlobalUnlock(handle);
            }
        }
        CloseClipboard();
        text
    }
}
#[cfg(not(windows))]
fn text() -> Option<String> {
    None
}
