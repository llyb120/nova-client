//! Windows text transport: one paste, no Unicode-key replay and no lossy clipboard backup.
use enigo::{Direction, Enigo, Key, Keyboard};
use std::{ptr::{null, null_mut}, thread::sleep, time::Duration};
use windows_sys::Win32::{
    Foundation::{GetLastError, GlobalFree, SetLastError, HGLOBAL, HWND},
    System::{DataExchange::*, Memory::*},
    UI::{Input::KeyboardAndMouse::GetAsyncKeyState, WindowsAndMessaging::*},
};

type Result<T> = std::result::Result<T, String>;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const CF_UNICODETEXT: u32 = 13;

fn supported(format: u32, name: &str) -> bool {
    matches!(format, 1 | 7 | 8 | 13 | 15 | 16 | 17)
        || (format >= 0xC000 && matches!(name, "PNG" | "HTML Format" | "Rich Text Format"
            | "CanIncludeInClipboardHistory" | "CanUploadToCloudClipboard"))
}
fn payload(text: &str) -> Result<Vec<u8>> {
    if text.contains('\0') || text.chars().count() > 10_000 { return Err("文本过长或含NUL".into()); }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n");
    Ok(normalized.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect())
}
fn os_error(context: &str) -> String { format!("{context}（Windows {}）", unsafe { GetLastError() }) }

struct Owner(HWND);
impl Owner {
    fn new() -> Result<Self> {
        let class = [83u16, 84, 65, 84, 73, 67, 0]; // Built-in STATIC; message-only, never shown/focused.
        let hwnd = unsafe { CreateWindowExW(0, class.as_ptr(), null(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), null_mut(), null()) };
        if hwnd.is_null() { Err(os_error("无法创建剪贴板暂存窗口")) } else { Ok(Self(hwnd)) }
    }
}
impl Drop for Owner { fn drop(&mut self) { unsafe { DestroyWindow(self.0); } } }
struct Open;
impl Open {
    fn new(owner: HWND) -> Result<Self> {
        for attempt in 0..8 {
            if unsafe { OpenClipboard(owner) } != 0 { return Ok(Self); }
            if attempt < 7 { sleep(Duration::from_millis(10)); }
        }
        Err(os_error("剪贴板忙，无法安全暂存/恢复"))
    }
}
impl Drop for Open { fn drop(&mut self) { unsafe { CloseClipboard(); } } }
struct Memory(HGLOBAL);
impl Memory {
    fn copy(bytes: &[u8]) -> Result<Self> {
        let memory = Self(unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) });
        if memory.0.is_null() { return Err(os_error("无法分配剪贴板备份")); }
        let target = unsafe { GlobalLock(memory.0) };
        if target.is_null() { return Err(os_error("无法锁定剪贴板备份")); }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
            GlobalUnlock(memory.0);
        }
        Ok(memory)
    }
    fn transfer(&mut self, format: u32) -> Result<()> {
        if unsafe { SetClipboardData(format, self.0) }.is_null() { return Err(os_error("设置剪贴板格式失败")); }
        self.0 = null_mut(); // The system owns this handle now.
        Ok(())
    }
}
impl Drop for Memory { fn drop(&mut self) { if !self.0.is_null() { unsafe { GlobalFree(self.0); } } } }

struct Swap {
    owner: HWND,
    backup: Vec<(u32, Memory)>,
    sequence: Option<u32>,
    restoring: bool,
}
impl Swap {
    fn stage(owner: HWND, bytes: &[u8]) -> Result<Self> {
        let mut text = Memory::copy(bytes)?;
        // Temporary input (including credentials) must not enter Windows clipboard history/cloud sync.
        let mut privacy = Vec::new();
        for name in ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"] {
            let wide = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let format = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
            if format == 0 { return Err(os_error("无法关闭临时输入的剪贴板历史同步")); }
            privacy.push((format, Memory::copy(&0u32.to_le_bytes())?));
        }
        let locked = Open::new(owner)?;
        let mut formats = Vec::new();
        let mut format = 0;
        loop {
            unsafe { SetLastError(0); }
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                if unsafe { GetLastError() } != 0 { return Err(os_error("无法枚举完整剪贴板")); }
                break;
            }
            if formats.len() == 64 { return Err("无法无损暂存：剪贴板格式超过64项".into()); }
            let mut name = [0u16; 256];
            let count = unsafe { GetClipboardFormatNameW(format, name.as_mut_ptr(), name.len() as i32) }.max(0) as usize;
            if !supported(format, &String::from_utf16_lossy(&name[..count])) {
                return Err(format!("无法无损暂存剪贴板格式 {format}；保留原剪贴板，未改用逐字输入"));
            }
            formats.push(format);
        }
        let mut swap = Self { owner, backup: Vec::new(), sequence: None, restoring: false };
        let mut total = bytes.len();
        for format in formats {
            let source = unsafe { GetClipboardData(format) };
            if source.is_null() { return Err(os_error("无法无损暂存：剪贴板延迟数据读取失败")); }
            let size = unsafe { GlobalSize(source) };
            if size == 0 || size > MAX_BYTES.saturating_sub(total) { return Err("无法无损暂存：剪贴板数据无效或超过64MiB".into()); }
            total += size;
            let source_ptr = unsafe { GlobalLock(source) };
            if source_ptr.is_null() { return Err(os_error("无法锁定原剪贴板数据")); }
            let copied = Memory::copy(unsafe { std::slice::from_raw_parts(source_ptr.cast::<u8>(), size) });
            unsafe { GlobalUnlock(source); }
            swap.backup.push((format, copied?));
        }
        // Every original format and all replacement HGLOBALs exist before the destructive call.
        if unsafe { EmptyClipboard() } == 0 { return Err(os_error("无法暂存剪贴板")); }
        swap.sequence = Some(unsafe { GetClipboardSequenceNumber() });
        let written = privacy.iter_mut().try_for_each(|(format, memory)| memory.transfer(*format))
            .and_then(|_| text.transfer(CF_UNICODETEXT));
        swap.sequence = Some(unsafe { GetClipboardSequenceNumber() });
        drop(locked);
        if let Err(error) = written {
            let restored = swap.restore();
            return Err(match restored { Ok(()) => error, Err(restore) => format!("{error}；{restore}") });
        }
        Ok(swap)
    }
    fn restore(&mut self) -> Result<()> {
        let Some(sequence) = self.sequence else { return Ok(()); };
        let _locked = Open::new(self.owner)?;
        // Check under the clipboard lock: never overwrite content copied by the user/another app.
        if unsafe { GetClipboardSequenceNumber() } != sequence { self.sequence = None; return Ok(()); }
        if !self.restoring {
            if unsafe { EmptyClipboard() } == 0 { return Err(os_error("剪贴板恢复失败，无法清理临时内容")); }
            self.restoring = true;
            self.sequence = Some(unsafe { GetClipboardSequenceNumber() });
        }
        for (format, memory) in &mut self.backup {
            if memory.0.is_null() { continue; }
            let restored = memory.transfer(*format);
            self.sequence = Some(unsafe { GetClipboardSequenceNumber() });
            restored.map_err(|error| format!("剪贴板恢复失败：{error}"))?;
        }
        self.sequence = None;
        Ok(())
    }
}
impl Drop for Swap {
    fn drop(&mut self) {
        // One bounded recovery attempt, including remaining formats after partial restoration.
        // Never empty again after some restored handles have transferred to the system.
        if let Err(error) = self.restore() { eprintln!("[jianlai] {error}"); }
    }
}

fn modifiers_released() -> Result<()> {
    if [0x11, 0x10, 0x12, 0x5B, 0x5C].iter().any(|key| unsafe { GetAsyncKeyState(*key) } < 0) {
        Err("Ctrl/Shift/Alt/Win仍被按下，请松开后再输入".into())
    } else { Ok(()) }
}
fn focus(expected: Option<(u32, u32)>) -> Result<HWND> {
    if expected.is_none() || super::foreground()? != expected { return Err("前台窗口已改变，停止输入".into()); }
    let thread = unsafe { GetWindowThreadProcessId(GetForegroundWindow(), null_mut()) };
    let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
    if thread == 0 || unsafe { GetGUIThreadInfo(thread, &mut info) } == 0 || info.hwndFocus.is_null() {
        return Err("无法确认输入焦点，停止输入".into());
    }
    Ok(info.hwndFocus)
}

pub(super) fn paste(enigo: &mut Enigo, text: &str, expected_foreground: Option<(u32, u32)>) -> Result<()> {
    if text.is_empty() { return Ok(()); }
    let not_sent = |error| format!("未输入：{error}");
    let bytes = payload(text).map_err(not_sent)?;
    modifiers_released().map_err(not_sent)?;
    let target = focus(expected_foreground).map_err(not_sent)?;
    let owner = Owner::new().map_err(not_sent)?;
    let mut swap = Swap::stage(owner.0, &bytes).map_err(not_sent)?;
    let mut attempted = false;
    let sent = (|| -> Result<()> {
        if focus(expected_foreground)? != target { return Err("输入焦点已改变，停止粘贴".into()); }
        modifiers_released()?;
        if Some(unsafe { GetClipboardSequenceNumber() }) != swap.sequence { return Err("剪贴板已改变，停止粘贴".into()); }
        attempted = true; // Any key error may still mean partial input. Never retry or fall back.
        let keys = [Key::Control, Key::Other(0x56)];
        let pressed = keys.iter().try_for_each(|key| enigo.key(*key, Direction::Press).map_err(|e| e.to_string()));
        let mut released = Ok(());
        for key in keys.iter().rev() {
            if let Err(error) = enigo.key(*key, Direction::Release) { released = Err(error.to_string()); }
        }
        // ponytail: Ctrl+V has no universal paste acknowledgement. Allow 80ms plus a bounded
        // target-message fence; apps that defer reading longer need an application-specific acknowledgement.
        sleep(Duration::from_millis(80));
        let mut result = 0usize;
        let responsive = unsafe { SendMessageTimeoutW(target, WM_NULL, 0, 0, SMTO_ABORTIFHUNG | SMTO_BLOCK, 200, &mut result) };
        pressed?;
        released?;
        if responsive == 0 { return Err("目标应用未及时处理粘贴，请核对当前输入".into()); }
        if focus(expected_foreground)? != target { return Err("粘贴期间输入焦点改变".into()); }
        if Some(unsafe { GetClipboardSequenceNumber() }) != swap.sequence { return Err("粘贴期间剪贴板改变，已保留新的剪贴板".into()); }
        Ok(())
    })();
    let restored = swap.restore();
    let error = match (sent, restored) {
        (Ok(()), Ok(())) => return Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => error,
        (Err(error), Err(restore)) => format!("{error}；{restore}"),
    };
    Err(format!("{}：{error}", if attempted { "可能已输入，请核验，勿重放" } else { "未输入" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_self_contained_clipboard_formats_are_accepted() {
        for format in [1, 7, 8, 13, 15, 16, 17] { assert!(supported(format, "")); }
        for name in ["PNG", "HTML Format", "Rich Text Format", "CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"] { assert!(supported(0xC001, name)); }
        for format in [2, 3, 9, 14, 0x80, 0x200, 0xC001] { assert!(!supported(format, "unknown")); }
        assert!(!supported(2, "PNG"));
    }
    #[test]
    fn text_is_one_utf16_clipboard_payload_not_enter_or_tab_events() {
        let bytes = payload("中文😀\nA\r\nB\rC\tD").unwrap();
        let wide = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect::<Vec<_>>();
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(String::from_utf16(&wide[..wide.len() - 1]).unwrap(), "中文😀\r\nA\r\nB\r\nC\tD");
        assert!(payload("a\0b").is_err());
        assert!(payload(&"界".repeat(10_001)).is_err());
        assert!(payload(&"😀".repeat(10_000)).is_ok());
    }
}
