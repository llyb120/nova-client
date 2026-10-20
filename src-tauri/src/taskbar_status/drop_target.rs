//! OLE drop target owned entirely by the Butterfly's STA/message thread.
//! Never send IDataObject, STGMEDIUM or an HWND to the capture worker.

use std::{cell::{Cell, RefCell}, marker::PhantomData, rc::Rc,
    sync::{atomic::{AtomicU64, Ordering}, Mutex}};
use tauri::Emitter;
use windows::{
    core::{implement, w, Ref, Result},
    Win32::{
        Foundation::{E_POINTER, HWND, POINTL},
        Globalization::{MultiByteToWideChar, CP_ACP, MB_ERR_INVALID_CHARS},
        System::{
            Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL},
            DataExchange::RegisterClipboardFormatW,
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::{IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize,
                RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, CF_UNICODETEXT,
                DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE},
            SystemServices::MODIFIERKEYS_FLAGS,
        },
    },
};
use windows_sys::Win32::UI::Shell::DROPFILES;

// Match capsules::capture's limits in UTF-8 bytes, not UTF-16 characters. One UTF-16 unit
// needs at least one UTF-8 byte: these read caps admit even an all-ASCII maximum-size input.
const MAX_FILES: usize = 16;
const MAX_PATH_BYTES: usize = 8192;
const MAX_PATH_UNITS: usize = MAX_PATH_BYTES;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_TEXT_UNITS: usize = MAX_TEXT_BYTES;
const MAX_FILE_BYTES: usize = size_of::<DROPFILES>() + (MAX_FILES * (MAX_PATH_UNITS + 1) + 1) * 2;
const INVALID_DATA: &str = "拖入内容为空、格式无效或无法读取；星尘未保存。";
const FILE_LIMIT: &str = "一次最多拖入 16 个文件；星尘未保存，未截断文件列表。";
const PATH_LIMIT: &str = "文件路径无效或超过 8192 字节（UTF-8）；星尘未保存。";
const TEXT_LIMIT: &str = "拖入文本上限为 256 KiB（UTF-8）；星尘未保存，未截断文本。";

static HOVER_REVISION: AtomicU64 = AtomicU64::new(0);
// ponytail: serialize this one chip's UI workers so a slow Enter cannot reopen a panel after
// Drop. Never lock on the STA or during capture I/O; use a UI queue if multiple chips are added.
static UI_WORK: Mutex<()> = Mutex::new(());

thread_local! {
    static REGISTERED: RefCell<Option<Registration>> = const { RefCell::new(None) };
}

// !Send/!Sync: including early returns, teardown must run on the initializing thread.
pub(super) struct Ole(PhantomData<Rc<()>>);

impl Ole {
    pub(super) fn initialize() -> Result<Self> {
        unsafe { OleInitialize(None)?; }
        Ok(Self(PhantomData))
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        revoke();
        unsafe { OleUninitialize(); }
    }
}

struct Registration {
    hwnd: HWND,
    _target: IDropTarget,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Err(error) = unsafe { RevokeDragDrop(self.hwnd) } {
            eprintln!("[taskbar-status] cannot revoke drop target: {error}");
        }
    }
}

/// Call only on the OLE thread, after the final (possibly non-layered) child is created.
pub(super) fn register(hwnd: HWND) -> Result<()> {
    revoke();
    let target: IDropTarget = Target {
        hwnd,
        supported: Cell::new(false),
        urls: unsafe { [RegisterClipboardFormatW(w!("UniformResourceLocatorW")) as u16,
            RegisterClipboardFormatW(w!("UniformResourceLocator")) as u16] },
    }.into();
    unsafe { RegisterDragDrop(hwnd, &target)?; }
    REGISTERED.with(|slot| *slot.borrow_mut() = Some(Registration { hwnd, _target: target }));
    Ok(())
}

pub(super) fn revoke() {
    // Drop outside the RefCell borrow: COM teardown can reenter the window procedure.
    let registration = REGISTERED.with(|slot| slot.borrow_mut().take());
    if registration.is_some() { hover(false); }
    drop(registration);
}

/// Enter/Leave only, never DragOver. Drop cancels stale hover jobs and shares their UI lock.
/// A drag never opens the panel; the chip's border lights up instead (see windows::glow).
fn hover(dragging: bool) {
    let revision = HOVER_REVISION.fetch_add(1, Ordering::AcqRel) + 1;
    super::windows::on_app(move |app| {
        let _ui = UI_WORK.lock().unwrap_or_else(|e| e.into_inner());
        if HOVER_REVISION.load(Ordering::Acquire) == revision {
            let _ = app.emit_to("taskbar-bubble", "capsules:dragging", dragging);
        }
    });
}

#[implement(IDropTarget, Agile = false)]
struct Target {
    hwnd: HWND,
    supported: Cell<bool>,
    urls: [u16; 2],
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let effect = unsafe { effect.as_mut() }.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        self.supported.set(data.as_ref().is_some_and(|data| {
            [CF_HDROP.0, CF_UNICODETEXT.0, self.urls[0], self.urls[1]].into_iter()
                .any(|format| format != 0 && unsafe { data.QueryGetData(&format_etc(format)).is_ok() })
        }));
        *effect = copy_effect(*effect, self.supported.get());
        unsafe { super::windows::glow(self.hwnd.0, *effect == DROPEFFECT_COPY); }
        hover(*effect == DROPEFFECT_COPY);
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let effect = unsafe { effect.as_mut() }.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        *effect = copy_effect(*effect, self.supported.get());
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.supported.set(false);
        unsafe { super::windows::glow(self.hwnd.0, false); }
        hover(false);
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        self.supported.set(false);
        unsafe { super::windows::glow(self.hwnd.0, false); }
        HOVER_REVISION.fetch_add(1, Ordering::AcqRel);
        let effect = unsafe { effect.as_mut() }.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        let allowed = copy_effect(*effect, true);
        *effect = DROPEFFECT_NONE;
        let copied = if allowed == DROPEFFECT_NONE {
            Err("拖动来源不允许复制；星尘未保存。")
        } else {
            data.as_ref().ok_or(INVALID_DATA).and_then(|data| copy_data(data, self.urls))
        };
        let accepted = copied.is_ok();
        let anchor = unsafe { super::windows::anchor(self.hwnd.0) };
        // All media have already been unlocked/released on the STA. Only owned data crosses.
        if super::windows::on_app(move |app| {
            let _ui = UI_WORK.lock().unwrap_or_else(|e| e.into_inner());
            let _ = app.emit_to("taskbar-bubble", "capsules:dragging", false);
            // Capture silently in the background; only a failure surfaces the panel (on its
            // current tab). The persisted error also works for a cold page.
            match copied {
                Ok((text, paths)) => super::capsules::capture_drop(app.clone(), text, paths, None),
                Err(error) => {
                    super::bubble::capture_error(app, error);
                    if let Some(anchor) = anchor { super::bubble::open_panel(app, anchor); }
                }
            }
        }) && accepted {
            // COPY means accepted for background capture, not persisted. capture_drop reports
            // its eventual success/failure; never tell the source to delete/move its data.
            *effect = DROPEFFECT_COPY;
        }
        Ok(())
    }
}

fn copy_effect(offered: DROPEFFECT, supported: bool) -> DROPEFFECT {
    if supported && offered.0 & DROPEFFECT_COPY.0 != 0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE }
}

fn format_etc(format: u16) -> FORMATETC {
    FORMATETC { cfFormat: format, dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() }
}

struct Medium(STGMEDIUM);
impl Drop for Medium {
    fn drop(&mut self) { unsafe { ReleaseStgMedium(&mut self.0); } }
}

/// None = format unavailable (try fallback); Err = malformed/oversized (reject, no truncation).
fn bytes(data: &IDataObject, format: u16, limit: usize, oversized: &'static str) -> std::result::Result<Option<Vec<u8>>, &'static str> {
    if format == 0 { return Ok(None); }
    unsafe {
        let request = format_etc(format);
        // Only absent formats fall back. An advertised file list failing to render must not
        // silently become its text representation and look like a successful file capture.
        let medium = match data.GetData(&request) {
            Ok(medium) => Medium(medium),
            Err(_) if data.QueryGetData(&request).is_err() => return Ok(None),
            Err(_) => return Err(INVALID_DATA),
        };
        if medium.0.tymed != TYMED_HGLOBAL.0 as u32 { return Err(INVALID_DATA); }
        let handle = medium.0.u.hGlobal;
        if handle.0.is_null() { return Err(INVALID_DATA); }
        let size = GlobalSize(handle);
        if size == 0 { return Err(INVALID_DATA); }
        if size > limit { return Err(oversized); }
        let pointer = GlobalLock(handle);
        if pointer.is_null() { return Err(INVALID_DATA); }
        let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), size).to_vec();
        let _ = GlobalUnlock(handle);
        Ok(Some(bytes))
    }
}

fn copy_data(data: &IDataObject, urls: [u16; 2]) -> std::result::Result<(String, Vec<String>), &'static str> {
    // Explorer's text representation is redundant: prefer its actual file list.
    if let Some(bytes) = bytes(data, CF_HDROP.0, MAX_FILE_BYTES, "文件列表数据超过原生读取上限；星尘未保存。")? {
        return Ok((String::new(), file_paths(&bytes)?));
    }
    for (format, unicode) in [(CF_UNICODETEXT.0, true), (urls[0], true), (urls[1], false)] {
        if let Some(bytes) = bytes(data, format, (MAX_TEXT_UNITS + 1) * 2, TEXT_LIMIT)? {
            let units = if unicode { wide(&bytes).ok_or(INVALID_DATA)? } else {
                // HGLOBAL may contain unused allocation padding after the terminator.
                let end = bytes.iter().position(|&byte| byte == 0).ok_or(INVALID_DATA)?;
                ansi(&bytes[..=end]).ok_or(INVALID_DATA)?
            };
            let text = text_payload(&units)?;
            if !text.is_empty() { return Ok((text, Vec::new())); }
        }
    }
    Err(INVALID_DATA)
}

fn text_payload(units: &[u16]) -> std::result::Result<String, &'static str> {
    if units.len() > MAX_TEXT_UNITS && !units[..=MAX_TEXT_UNITS].contains(&0) { return Err(TEXT_LIMIT); }
    let text = terminated_text(units, MAX_TEXT_UNITS).ok_or(INVALID_DATA)?;
    if text.len() > MAX_TEXT_BYTES { return Err(TEXT_LIMIT); }
    Ok(text)
}

fn wide(bytes: &[u8]) -> Option<Vec<u16>> {
    if bytes.len() % 2 != 0 { return None; }
    Some(bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect())
}

fn ansi(bytes: &[u8]) -> Option<Vec<u16>> {
    unsafe {
        let len = MultiByteToWideChar(CP_ACP, MB_ERR_INVALID_CHARS, bytes, None);
        if len <= 0 { return None; }
        let mut units = vec![0; len as usize];
        (MultiByteToWideChar(CP_ACP, MB_ERR_INVALID_CHARS, bytes, Some(&mut units)) == len).then_some(units)
    }
}

fn terminated_text(units: &[u16], limit: usize) -> Option<String> {
    let end = units.iter().take(limit + 1).position(|&unit| unit == 0)?;
    String::from_utf16(&units[..end]).ok()
}

fn file_paths(bytes: &[u8]) -> std::result::Result<Vec<String>, &'static str> {
    if bytes.len() < size_of::<DROPFILES>() { return Err(INVALID_DATA); }
    // Validate before interpreting the list; unlike blindly calling DragQueryFileW this also
    // bounds unterminated/untrusted lists to GlobalSize. DROPFILES contains only integer fields.
    let header = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<DROPFILES>()) };
    let offset = header.pFiles as usize;
    if offset < size_of::<DROPFILES>() || offset >= bytes.len() { return Err(INVALID_DATA); }
    let units = if header.fWide != 0 {
        if offset % 2 != 0 { return Err(INVALID_DATA); }
        wide(&bytes[offset..]).ok_or(INVALID_DATA)?
    } else {
        let list = &bytes[offset..];
        let end = list.windows(2).position(|pair| pair == [0, 0]).ok_or(INVALID_DATA)? + 2;
        ansi(&list[..end]).ok_or(INVALID_DATA)?
    };
    wide_paths(&units)
}

fn wide_paths(mut units: &[u16]) -> std::result::Result<Vec<String>, &'static str> {
    let mut paths = Vec::new();
    loop {
        let path = terminated_text(units, MAX_PATH_UNITS).ok_or(PATH_LIMIT)?;
        if path.is_empty() { return if paths.is_empty() { Err(INVALID_DATA) } else { Ok(paths) }; }
        if paths.len() == MAX_FILES { return Err(FILE_LIMIT); }
        if path.len() > MAX_PATH_BYTES { return Err(PATH_LIMIT); }
        let used = path.encode_utf16().count() + 1;
        paths.push(path);
        units = &units[used..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_file_limits_never_truncate() {
        let units: Vec<u16> = "中文🦋\0".encode_utf16().collect();
        assert_eq!(terminated_text(&units, 4).as_deref(), Some("中文🦋"));
        assert!(terminated_text(&units, 3).is_none());
        assert!(terminated_text(&[0xd800, 0], 4).is_none());
        assert!(terminated_text(&[65], 4).is_none());
        let mut text = vec![65; MAX_TEXT_UNITS];
        text.push(0);
        assert!(terminated_text(&text, MAX_TEXT_UNITS).is_some());
        text.insert(0, 65);
        assert!(terminated_text(&text, MAX_TEXT_UNITS).is_none());
        let paths: Vec<u16> = "C:\\中文🦋.txt\0D:\\b.txt\0\0".encode_utf16().collect();
        assert_eq!(wide_paths(&paths).unwrap(), ["C:\\中文🦋.txt", "D:\\b.txt"]);
        assert!(wide_paths(&paths[..paths.len() - 1]).is_err());
        assert!(wide_paths(&[0, 0]).is_err());
        let mut many = vec![65, 0].repeat(MAX_FILES);
        many.push(0);
        assert_eq!(wide_paths(&many).unwrap().len(), MAX_FILES);
        many.splice(0..0, [65, 0]);
        assert_eq!(wide_paths(&many), Err(FILE_LIMIT));
        let mut long = vec![65; MAX_PATH_UNITS + 1];
        long.extend([0, 0]);
        assert_eq!(wide_paths(&long), Err(PATH_LIMIT));
    }

    #[test]
    fn utf8_limits_match_backend_and_errors_say_not_saved() {
        let ascii: Vec<u16> = "a".repeat(MAX_TEXT_BYTES).encode_utf16().chain([0]).collect();
        assert_eq!(text_payload(&ascii).unwrap().len(), MAX_TEXT_BYTES);
        let mut oversized = ascii.clone();
        oversized.insert(0, 65);
        assert_eq!(text_payload(&oversized), Err(TEXT_LIMIT));
        let chinese: Vec<u16> = "中".repeat(MAX_TEXT_BYTES / 3 + 1).encode_utf16().chain([0]).collect();
        assert_eq!(text_payload(&chinese), Err(TEXT_LIMIT));
        let path: Vec<u16> = "中".repeat(MAX_PATH_BYTES / 3 + 1).encode_utf16().chain([0, 0]).collect();
        assert_eq!(wide_paths(&path), Err(PATH_LIMIT));
        assert_eq!(text_payload(&[0xd800, 0]), Err(INVALID_DATA));
        for error in [INVALID_DATA, FILE_LIMIT, PATH_LIMIT, TEXT_LIMIT] {
            assert!(error.contains("星尘未保存"));
        }
    }

    #[test]
    fn hdrop_requires_a_bounded_header_and_double_terminator() {
        let mut bytes = vec![0; size_of::<DROPFILES>()];
        bytes[..4].copy_from_slice(&(size_of::<DROPFILES>() as u32).to_le_bytes());
        bytes[16..20].copy_from_slice(&1i32.to_le_bytes());
        bytes.extend("C:\\a.txt\0\0".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(file_paths(&bytes).unwrap(), ["C:\\a.txt"]);
        let mut ansi = bytes[..size_of::<DROPFILES>()].to_vec();
        ansi[16..20].fill(0);
        ansi.extend(b"C:\\a.txt\0\0");
        ansi.extend([0xff, 0x81]); // Allocation padding is not part of the encoded list.
        assert_eq!(file_paths(&ansi).unwrap(), ["C:\\a.txt"]);
        bytes[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(file_paths(&bytes).is_err());
        assert!(file_paths(&bytes[..10]).is_err());
        assert!(wide(&[1]).is_none());
    }

    #[test]
    fn missing_com_data_rejects_without_copy() {
        let target: IDropTarget = Target { hwnd: HWND::default(), supported: Cell::new(true), urls: [0; 2] }.into();
        let mut effect = DROPEFFECT_COPY;
        unsafe { target.Drop(None, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap(); }
        assert_eq!(effect, DROPEFFECT_NONE);
        let error = unsafe { target.Drop(None, MODIFIERKEYS_FLAGS(0), POINTL::default(), std::ptr::null_mut()) }.unwrap_err();
        assert_eq!(error.code(), E_POINTER);
    }

    #[test]
    fn only_copy_is_ever_negotiated() {
        for mask in 0..8 {
            assert_eq!(copy_effect(DROPEFFECT(mask), true), if mask & 1 != 0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE });
            assert_eq!(copy_effect(DROPEFFECT(mask), false), DROPEFFECT_NONE);
        }
    }
}
