//! Windows text transport: paste the whole text once, never replay it as per-character keys.
//! Key-based input is rewritten/swallowed by active CJK IMEs (dropped 我/回, ，→ ,), so there is
//! deliberately no typing fallback. The text is offered by delayed rendering: the target asking
//! for it is a real paste acknowledgement, and the user's clipboard is restored only afterwards.
use enigo::{Direction, Enigo, Key, Keyboard};
use std::{cell::{Cell, RefCell}, ptr::{null, null_mut}, thread::sleep, time::{Duration, Instant}};
use windows_sys::Win32::{
    Foundation::{GetLastError, GlobalFree, SetLastError, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{CopyEnhMetaFileW, DeleteEnhMetaFile},
    System::{DataExchange::*, LibraryLoader::GetModuleHandleW, Memory::*},
    UI::{Input::KeyboardAndMouse::GetAsyncKeyState, WindowsAndMessaging::*},
};

type Result<T> = std::result::Result<T, String>;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const CF_UNICODETEXT: u32 = 13;
const CF_ENHMETAFILE: u32 = 14;
const PASTE_ACK: Duration = Duration::from_millis(3000);
// Some targets (企业微信) ack with one read but insert from a later async read; restoring right after
// the first read left their input empty. The rendered text stays on the clipboard for that read.
// ponytail: fixed grace; a target reading again even later still gets the restored clipboard.
const PASTE_GRACE: Duration = Duration::from_millis(400);

thread_local! {
    // Text handed out on WM_RENDERFORMAT, and whether something has read it.
    static PENDING: RefCell<Option<Memory>> = const { RefCell::new(None) };
    static RENDERED: Cell<bool> = const { Cell::new(false) };
    // Until Ctrl+V is pressed every reader is a clipboard listener, never the target. Refused
    // requests leave the format delayed, so the target's later read is still our acknowledgement.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static LAST_REFUSED: Cell<Option<Instant>> = const { Cell::new(None) };
}

// Bitmap/palette/metafile-picture are GDI handles that Windows re-synthesizes from the DIB/EMF we keep.
// ponytail: owner-display and private GDI formats cannot be copied and are dropped on restore (rare).
fn skipped(format: u32) -> bool { matches!(format, 2 | 3 | 9 | 0x80..=0x8F | 0x300..=0x3FF) }
fn payload(text: &str) -> Result<Vec<u8>> {
    if text.contains('\0') || text.chars().count() > 10_000 { return Err("文本过长或含NUL".into()); }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n");
    Ok(normalized.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect())
}
fn os_error(context: &str) -> String { format!("{context}（Windows {}）", unsafe { GetLastError() }) }
/// Delivers sent messages (WM_RENDERFORMAT) to our owner window while we wait.
fn pump() {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        unsafe { TranslateMessage(&message); DispatchMessageW(&message); }
    }
}

unsafe extern "system" fn owner_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_RENDERFORMAT && wparam as u32 == CF_UNICODETEXT {
        if !ARMED.with(Cell::get) { LAST_REFUSED.with(|last| last.set(Some(Instant::now()))); return 0; }
        // After the call ends PENDING is empty: a late reader gets nothing instead of stale text.
        if let Some(mut text) = PENDING.with(|pending| pending.borrow_mut().take()) {
            if text.transfer(CF_UNICODETEXT).is_ok() {
                RENDERED.with(|rendered| rendered.set(true));
            }
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct Owner(HWND);
impl Owner {
    fn new() -> Result<Self> {
        let class = "NovaJianlaiClipboardOwner\0".encode_utf16().collect::<Vec<_>>();
        let instance = unsafe { GetModuleHandleW(null()) };
        let mut window_class: WNDCLASSW = unsafe { std::mem::zeroed() };
        window_class.lpfnWndProc = Some(owner_proc);
        window_class.hInstance = instance;
        window_class.lpszClassName = class.as_ptr();
        // 1410 = ERROR_CLASS_ALREADY_EXISTS on every call after the first.
        if unsafe { RegisterClassW(&window_class) } == 0 && unsafe { GetLastError() } != 1410 {
            return Err(os_error("无法注册剪贴板暂存窗口"));
        }
        // Message-only: never shown or focused.
        let hwnd = unsafe { CreateWindowExW(0, class.as_ptr(), null(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), instance, null()) };
        if hwnd.is_null() { Err(os_error("无法创建剪贴板暂存窗口")) } else { Ok(Self(hwnd)) }
    }
}
impl Drop for Owner { fn drop(&mut self) { unsafe { DestroyWindow(self.0); } } }
struct Open;
impl Open {
    fn new(owner: HWND) -> Result<Self> {
        // The target may still hold the clipboard while reading the pasted text.
        for attempt in 0..50 {
            if unsafe { OpenClipboard(owner) } != 0 { return Ok(Self); }
            if attempt < 49 { pump(); sleep(Duration::from_millis(10)); }
        }
        Err(os_error("剪贴板忙，无法暂存/恢复"))
    }
}
impl Drop for Open { fn drop(&mut self) { unsafe { CloseClipboard(); } } }
struct Memory { handle: HGLOBAL, emf: bool }
impl Memory {
    fn copy(bytes: &[u8]) -> Result<Self> {
        let memory = Self { handle: unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }, emf: false };
        if memory.handle.is_null() { return Err(os_error("无法分配剪贴板备份")); }
        let target = unsafe { GlobalLock(memory.handle) };
        if target.is_null() { return Err(os_error("无法锁定剪贴板备份")); }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
            GlobalUnlock(memory.handle);
        }
        Ok(memory)
    }
    fn emf(source: HGLOBAL) -> Result<Self> {
        let handle = unsafe { CopyEnhMetaFileW(source, null()) };
        if handle.is_null() { Err(os_error("无法备份剪贴板图元")) } else { Ok(Self { handle, emf: true }) }
    }
    fn transfer(&mut self, format: u32) -> Result<()> {
        if unsafe { SetClipboardData(format, self.handle) }.is_null() { return Err(os_error("设置剪贴板格式失败")); }
        self.handle = null_mut(); // The system owns this handle now.
        Ok(())
    }
}
impl Drop for Memory {
    fn drop(&mut self) {
        if self.handle.is_null() { return; }
        unsafe { if self.emf { DeleteEnhMetaFile(self.handle); } else { GlobalFree(self.handle); } }
    }
}

struct Swap {
    owner: HWND,
    backup: Vec<(u32, Memory)>,
    staged: bool,
    restoring: bool,
}
impl Swap {
    fn stage(owner: HWND, bytes: &[u8]) -> Result<Self> {
        let text = Memory::copy(bytes)?;
        // Temporary input (including credentials) must not enter clipboard history/cloud sync or
        // be pulled early by clipboard managers (which would trigger the render before Ctrl+V).
        let mut privacy = Vec::new();
        for name in ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard", "ExcludeClipboardContentFromMonitorProcessing"] {
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
                if unsafe { GetLastError() } != 0 { return Err(os_error("无法枚举剪贴板")); }
                break;
            }
            if formats.len() == 256 { return Err("剪贴板格式过多，无法暂存".into()); }
            if !skipped(format) { formats.push(format); }
        }
        let mut swap = Self { owner, backup: Vec::new(), staged: false, restoring: false };
        let mut total = bytes.len();
        for format in formats {
            let source = unsafe { GetClipboardData(format) };
            if source.is_null() { continue; } // Delayed render failed (source app gone): nothing to keep.
            if format == CF_ENHMETAFILE { swap.backup.push((format, Memory::emf(source)?)); continue; }
            let size = unsafe { GlobalSize(source) };
            if size == 0 { continue; } // Not an HGLOBAL; cannot be copied.
            if size > MAX_BYTES.saturating_sub(total) { return Err("剪贴板内容超过256MiB，无法暂存".into()); }
            total += size;
            let source_ptr = unsafe { GlobalLock(source) };
            if source_ptr.is_null() { continue; }
            let copied = Memory::copy(unsafe { std::slice::from_raw_parts(source_ptr.cast::<u8>(), size) });
            unsafe { GlobalUnlock(source); }
            swap.backup.push((format, copied?));
        }
        // Every original format and all replacement handles exist before the destructive call.
        if unsafe { EmptyClipboard() } == 0 { return Err(os_error("无法暂存剪贴板")); }
        swap.staged = true;
        RENDERED.with(|rendered| rendered.set(false));
        ARMED.with(|armed| armed.set(false));
        LAST_REFUSED.with(|last| last.set(None));
        PENDING.with(|pending| *pending.borrow_mut() = Some(text));
        let written = privacy.iter_mut().try_for_each(|(format, memory)| memory.transfer(*format)).and_then(|_| {
            unsafe { SetLastError(0); SetClipboardData(CF_UNICODETEXT, null_mut()); } // Delayed rendering.
            if unsafe { GetLastError() } != 0 { Err(os_error("设置剪贴板格式失败")) } else { Ok(()) }
        });
        drop(locked);
        if let Err(error) = written {
            let restored = swap.restore();
            return Err(match restored { Ok(()) => error, Err(restore) => format!("{error}；{restore}") });
        }
        Ok(swap)
    }
    /// Anyone else writing the clipboard must EmptyClipboard and thereby takes ownership. Unlike the
    /// sequence number, ownership is unaffected by readers, delayed rendering and listener activity.
    fn owned(&self) -> bool { (unsafe { GetClipboardOwner() }) == self.owner }
    fn restore(&mut self) -> Result<()> {
        PENDING.with(|pending| pending.borrow_mut().take()); // Unrendered text never outlives the call.
        if !self.staged { return Ok(()); }
        let _locked = Open::new(self.owner)?;
        // Check under the clipboard lock: never overwrite content copied by the user/another app.
        if !self.owned() { self.staged = false; return Ok(()); }
        if !self.restoring {
            if unsafe { EmptyClipboard() } == 0 { return Err(os_error("剪贴板恢复失败，无法清理临时内容")); }
            self.restoring = true;
        }
        for (format, memory) in &mut self.backup {
            if memory.handle.is_null() { continue; }
            memory.transfer(*format).map_err(|error| format!("剪贴板恢复失败：{error}"))?;
        }
        self.staged = false;
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
    if expected.is_none() { return Err("无法确认前台窗口，停止输入".into()); }
    super::regain(expected)?;
    let thread = unsafe { GetWindowThreadProcessId(GetForegroundWindow(), null_mut()) };
    let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
    if thread == 0 || unsafe { GetGUIThreadInfo(thread, &mut info) } == 0 || info.hwndFocus.is_null() {
        return Err("无法确认输入焦点，停止输入".into());
    }
    Ok(info.hwndFocus)
}
/// Lets eager clipboard listeners (they read right after every change, here before Ctrl+V) get their
/// refusal and go quiet, so they cannot take the acknowledgement meant for the target.
// ponytail: a listener that keeps polling past 500ms could still steal the ack and restore too early.
fn settle_listeners() {
    let start = Instant::now();
    loop {
        pump();
        let last = LAST_REFUSED.with(Cell::get).map_or(start, |last| last.max(start));
        if last.elapsed() >= Duration::from_millis(60) || start.elapsed() >= Duration::from_millis(500) { return; }
        sleep(Duration::from_millis(5));
    }
}
fn wait_for_render(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        pump();
        if RENDERED.with(Cell::get) { return true; }
        if Instant::now() >= deadline { return false; }
        sleep(Duration::from_millis(5));
    }
}

pub(super) fn type_text(enigo: &mut Enigo, text: &str, expected_foreground: Option<(u32, u32)>) -> Result<()> {
    if text.is_empty() { return Ok(()); }
    let not_sent = |error| format!("未输入：{error}");
    let bytes = payload(text).map_err(not_sent)?;
    modifiers_released().map_err(not_sent)?;
    let target = focus(expected_foreground).map_err(not_sent)?;
    let owner = Owner::new().map_err(not_sent)?;
    let mut swap = Swap::stage(owner.0, &bytes).map_err(not_sent)?;
    let mut attempted = false;
    let mut unread = false;
    let sent = (|| -> Result<()> {
        if focus(expected_foreground)? != target { return Err("输入焦点已改变，停止粘贴".into()); }
        modifiers_released()?;
        settle_listeners();
        if !swap.owned() { return Err("剪贴板已被其他程序改写，停止粘贴".into()); }
        attempted = true; // Any key error may still mean partial input. Never retry or fall back.
        ARMED.with(|armed| armed.set(true));
        let keys = [Key::Control, Key::Other(0x56)];
        let pressed = keys.iter().try_for_each(|key| enigo.key(*key, Direction::Press).map_err(|e| e.to_string()));
        let mut released = Ok(());
        for key in keys.iter().rev() {
            if let Err(error) = enigo.key(*key, Direction::Release) { released = Err(error.to_string()); }
        }
        // The clipboard is restored only after the target actually read our text, however slow it is.
        let acknowledged = wait_for_render(PASTE_ACK);
        pressed?;
        released?;
        if !acknowledged {
            unread = true;
            return Err("目标应用3秒内没有读取粘贴内容，焦点可能不在可粘贴的输入框".into());
        }
        let grace = Instant::now() + PASTE_GRACE;
        while Instant::now() < grace { pump(); sleep(Duration::from_millis(5)); }
        if focus(expected_foreground)? != target { return Err("粘贴期间输入焦点改变".into()); }
        if !swap.owned() { return Err("粘贴期间剪贴板被其他程序改写，已保留新的剪贴板".into()); }
        Ok(())
    })();
    let restored = swap.restore();
    let error = match (sent, restored) {
        (Ok(()), Ok(())) => return Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => error,
        (Err(error), Err(restore)) => format!("{error}；{restore}"),
    };
    // Unread: the text was never handed out, so nothing of ours can have been inserted.
    Err(format!("{}：{error}", if attempted && !unread { "可能已输入，请核验，勿重放" } else { "未输入" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_gdi_and_owner_drawn_formats_are_skipped() {
        for format in [1, 7, 8, 13, 14, 15, 16, 17, 0x200, 0xC001] { assert!(!skipped(format), "{format}"); }
        for format in [2, 3, 9, 0x80, 0x82, 0x8E, 0x300] { assert!(skipped(format), "{format}"); }
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
    #[test]
    #[ignore = "uses the real Windows clipboard: cargo test delayed_render -- --ignored"]
    fn delayed_render_acknowledges_the_read_and_restores_the_original_clipboard() {
        let read = || {
            let _open = Open::new(null_mut()).unwrap();
            let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
            if handle.is_null() { return None; }
            let wide = unsafe { GlobalLock(handle) }.cast::<u16>();
            let len = (0..).take_while(|&i| unsafe { *wide.add(i) } != 0).count();
            let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) });
            unsafe { GlobalUnlock(handle); }
            Some(text)
        };
        let owner = Owner::new().unwrap();
        let before = read();
        let mut swap = Swap::stage(owner.0, &payload("哈哈，我这边现在有点事在忙，回头再聊～").unwrap()).unwrap();
        // Before Ctrl+V, listeners (and this read) are refused and the text stays delayed.
        settle_listeners();
        assert_eq!(read(), None);
        assert_eq!(read(), None, "a refused render must not consume the delayed format");
        assert!(!wait_for_render(Duration::ZERO));
        ARMED.with(|armed| armed.set(true));
        // Same-thread read renders directly; reading must not look like a foreign write.
        assert_eq!(read().as_deref(), Some("哈哈，我这边现在有点事在忙，回头再聊～"));
        assert!(wait_for_render(Duration::ZERO));
        assert!(swap.owned(), "render/read must not count as the clipboard being changed");
        swap.restore().unwrap();
        assert_eq!(read(), before, "the user's clipboard is restored");
        // A foreign write takes ownership; restore must keep it instead of clobbering it.
        let outer = Swap::stage(owner.0, &payload("外层").unwrap()).unwrap(); // Backs up the user's clipboard.
        let mut inner = Swap::stage(owner.0, &payload("内层").unwrap()).unwrap();
        let foreign = Owner::new().unwrap();
        {
            let _open = Open::new(foreign.0).unwrap();
            unsafe { EmptyClipboard(); }
            Memory::copy(&payload("外部").unwrap()).unwrap().transfer(CF_UNICODETEXT).unwrap();
        }
        assert!(!inner.owned());
        inner.restore().unwrap();
        assert_eq!(read().as_deref(), Some("外部"));
        { let _open = Open::new(owner.0).unwrap(); unsafe { EmptyClipboard(); } } // Hand back to outer.
        drop(outer);
        assert_eq!(read(), before, "the user's clipboard is restored");
    }
}
