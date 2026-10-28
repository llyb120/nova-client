//! Windows input through the Interception keyboard/mouse filter driver (https://github.com/oblitum/Interception).
//! Strokes enter the class-driver stack like a real device's, so they carry no LLMHF_INJECTED/LLKHF_INJECTED flag.
//! Enabled by `NOVA_JIANLAI_INPUT=interception`; needs the driver installed (and a reboot) plus interception.dll
//! next to Nova or on PATH. Strokes go to the first keyboard (1) and first mouse (11) device.
use enigo::{Button, Direction, Key};
use std::{ffi::c_void, sync::{atomic::{AtomicBool, Ordering}, Mutex}, thread::sleep, time::Duration};
use windows_sys::Win32::{
    Foundation::HMODULE,
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    UI::{Input::KeyboardAndMouse::{MapVirtualKeyW, VkKeyScanW}, WindowsAndMessaging::*},
};

type Result<T> = std::result::Result<T, String>;
const KEYBOARD: i32 = 1;
const MOUSE: i32 = 11;
const ABSOLUTE_VIRTUAL_DESKTOP: u16 = 0x001 | 0x002;
const WHEEL: u16 = 0x400;
const HWHEEL: u16 = 0x800;

#[repr(C)]
#[derive(Default)]
struct MouseStroke { state: u16, flags: u16, rolling: i16, x: i32, y: i32, information: u32 }
#[repr(C)]
#[derive(Default)]
struct KeyStroke { code: u16, state: u16, information: u32 }

struct Driver { context: usize, send: unsafe extern "C" fn(*mut c_void, i32, *const c_void, u32) -> i32 }
static DRIVER: Mutex<Option<Driver>> = Mutex::new(None);
/// Set per batch: private background seats must never be driven through the physical input stack.
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub(super) fn set_active(on: bool) {
    ACTIVE.store(on && std::env::var("NOVA_JIANLAI_INPUT").is_ok_and(|v| v.eq_ignore_ascii_case("interception")), Ordering::Relaxed);
}
pub(super) fn enabled() -> bool { ACTIVE.load(Ordering::Relaxed) }

fn send<T>(device: i32, stroke: &T) -> Result<()> {
    let mut guard = DRIVER.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        let name: Vec<u16> = "interception.dll\0".encode_utf16().collect();
        let dll: HMODULE = unsafe { LoadLibraryW(name.as_ptr()) };
        if dll.is_null() { return Err("找不到interception.dll；请安装Interception驱动并把DLL放在Nova旁或PATH中".into()); }
        unsafe {
            let create = GetProcAddress(dll, c"interception_create_context".as_ptr().cast());
            let send = GetProcAddress(dll, c"interception_send".as_ptr().cast());
            let (Some(create), Some(send)) = (create, send) else { return Err("interception.dll缺少导出函数".into()) };
            let context = std::mem::transmute::<_, unsafe extern "C" fn() -> *mut c_void>(create)();
            if context.is_null() { return Err("无法打开Interception驱动；请确认已安装并重启过系统".into()); }
            *guard = Some(Driver { context: context as usize, send: std::mem::transmute(send) });
        }
    }
    let driver = guard.as_ref().unwrap();
    // Strokes are passed as the DLL's fixed-size union; ours are never larger than the mouse stroke.
    let mut buffer = [0u8; size_of::<MouseStroke>()];
    unsafe { std::ptr::copy_nonoverlapping((stroke as *const T).cast::<u8>(), buffer.as_mut_ptr(), size_of::<T>()) };
    if unsafe { (driver.send)(driver.context as *mut c_void, device, buffer.as_ptr().cast(), 1) } != 1 {
        return Err("Interception驱动未接收输入".into());
    }
    Ok(())
}

fn mouse(state: u16, flags: u16, rolling: i16, x: i32, y: i32) -> Result<()> {
    send(MOUSE, &MouseStroke { state, flags, rolling, x, y, information: 0 })
}

/// Absolute move over the virtual desktop, same normalization as SendInput.
pub(super) fn move_to(x: i32, y: i32) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics;
    let n = |p, origin, size| super::virtual_desk_coordinate(p, unsafe { GetSystemMetrics(origin) }, unsafe { GetSystemMetrics(size) });
    mouse(0, ABSOLUTE_VIRTUAL_DESKTOP, 0, n(x, SM_XVIRTUALSCREEN, SM_CXVIRTUALSCREEN), n(y, SM_YVIRTUALSCREEN, SM_CYVIRTUALSCREEN))
}

pub(super) fn button(button: Button, direction: Direction) -> Result<()> {
    let (down, up) = match button { Button::Left => (0x01, 0x02), Button::Right => (0x04, 0x08), Button::Middle => (0x10, 0x20), _ => return Err("驱动输入不支持该鼠标键".into()) };
    if direction != Direction::Release { mouse(down, 0, 0, 0, 0)?; }
    if direction == Direction::Click { sleep(Duration::from_millis(30)); }
    if direction != Direction::Press { mouse(up, 0, 0, 0, 0)?; }
    Ok(())
}

/// One wheel notch; positive = down (vertical) / right (horizontal), like enigo.
pub(super) fn scroll(length: i32, horizontal: bool) -> Result<()> {
    let notch = 120 * length.signum() as i16;
    if horizontal { mouse(HWHEEL, 0, notch, 0, 0) } else { mouse(WHEEL, 0, -notch, 0, 0) }
}

pub(super) fn key(key: Key, direction: Direction) -> Result<()> {
    let vk = virtual_key(key).ok_or_else(|| format!("驱动输入不支持按键{key:?}"))?;
    let scan = unsafe { MapVirtualKeyW(vk as u32, 0) } as u16;
    if scan == 0 { return Err(format!("按键{key:?}没有扫描码")); }
    let state = if extended(vk) { 0x02 } else { 0 };
    let stroke = |up: u16| send(KEYBOARD, &KeyStroke { code: scan, state: state | up, information: 0 });
    if direction != Direction::Release { stroke(0)?; }
    if direction == Direction::Click { sleep(Duration::from_millis(30)); }
    if direction != Direction::Press { stroke(1)?; }
    Ok(())
}

/// Keys whose scan codes carry the E0 prefix: navigation block, arrows, right-hand modifiers, Win.
fn extended(vk: u8) -> bool { matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0xA3 | 0xA5) }

fn virtual_key(key: Key) -> Option<u8> {
    Some(match key {
        Key::Control => 0xA2, Key::Shift => 0xA0, Key::Alt => 0xA4, Key::Meta => 0x5B,
        Key::Return => 0x0D, Key::Escape => 0x1B, Key::Backspace => 0x08, Key::Tab => 0x09, Key::Space => 0x20,
        Key::Delete => 0x2E, Key::Home => 0x24, Key::End => 0x23, Key::PageUp => 0x21, Key::PageDown => 0x22,
        Key::LeftArrow => 0x25, Key::UpArrow => 0x26, Key::RightArrow => 0x27, Key::DownArrow => 0x28,
        Key::F1 => 0x70, Key::F2 => 0x71, Key::F3 => 0x72, Key::F4 => 0x73, Key::F5 => 0x74, Key::F6 => 0x75,
        Key::F7 => 0x76, Key::F8 => 0x77, Key::F9 => 0x78, Key::F10 => 0x79, Key::F11 => 0x7A, Key::F12 => 0x7B,
        Key::Other(vk) => u8::try_from(vk).ok()?,
        // Shift state is ignored: a shifted character is not a single physical key.
        Key::Unicode(c) => match unsafe { VkKeyScanW(u16::try_from(c as u32).ok()?) } { -1 => return None, v => (v & 0xFF) as u8 },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_keys_to_virtual_keys_and_scan_prefixes() {
        assert_eq!(virtual_key(Key::Other(0x56)), Some(0x56));
        assert_eq!(virtual_key(Key::Return), Some(0x0D));
        assert_eq!(virtual_key(Key::Unicode('中')), None);
        assert!(extended(0x25) && extended(0x2E) && !extended(0x0D) && !extended(0xA2));
        assert_eq!(size_of::<MouseStroke>(), 20); // matches InterceptionMouseStroke
    }
}
