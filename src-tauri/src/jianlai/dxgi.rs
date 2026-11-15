//! Windows monitor capture through the DXGI Desktop Duplication API.
//!
//! A single thread owns the duplication sessions. Recreating a device and output
//! duplication for every shot costs tens of milliseconds, and a static desktop
//! never presents, so a fresh `AcquireNextFrame(300)` used to time out and fall
//! back to xcap on every unchanged screen. The session stays open: a queued frame
//! is copied, and a timeout returns the last good frame (the desktop has not changed).
//!
//! Any hard failure (rotated output, HDR, secure desktop, access lost) is reported
//! to the caller, which falls back to xcap.
use std::{
    sync::{mpsc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::Interface,
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
};
use xcap::image::RgbaImage;

type Key = (i32, i32, u32, u32);

#[derive(Debug)]
pub(super) enum GrabError {
    /// No frame yet (static desktop on a new duplication). Caller uses xcap quietly.
    Quiet,
    Loud(String),
}

enum Request {
    Grab(Key, mpsc::Sender<Result<RgbaImage, GrabError>>),
    Settle(Option<Key>, Duration, mpsc::Sender<bool>),
}

enum Next {
    Timeout,
    /// Pointer-only present, or a black warmup frame while a good cache exists.
    Same,
    /// Black warmup frame and nothing cached yet.
    Black,
    Frame(RgbaImage),
}

struct Session {
    key: Key,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    cache: Option<RgbaImage>,
}

struct State {
    sessions: Vec<Session>,
}

pub(super) fn grab(x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage, GrabError> {
    let (tx, rx) = mpsc::channel();
    try_send(Request::Grab((x, y, width, height), tx)).map_err(|_| GrabError::Loud("DXGI捕获线程不可用".into()))?;
    rx.recv().unwrap_or_else(|_| Err(GrabError::Loud("DXGI捕获线程已退出".into())))
}

/// Wait until `budget` elapses, or until a substantial desktop change is followed by a quiet frame.
/// Returns false when no reusable session exists, so the caller keeps the full wait.
pub(super) fn settle(target: Option<Key>, budget: Duration) -> bool {
    if budget.is_zero() {
        return true;
    }
    let (tx, rx) = mpsc::channel();
    if try_send(Request::Settle(target, budget, tx)).is_err() {
        return false;
    }
    rx.recv().unwrap_or(false)
}

fn sender() -> &'static Mutex<mpsc::Sender<Request>> {
    static TX: OnceLock<Mutex<mpsc::Sender<Request>>> = OnceLock::new();
    TX.get_or_init(|| Mutex::new(spawn()))
}

fn spawn() -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("jianlai-dxgi".into())
        .spawn(move || worker(rx))
        .expect("jianlai dxgi thread");
    tx
}

fn try_send(request: Request) -> Result<(), Request> {
    let mut guard = sender().lock().unwrap_or_else(|e| e.into_inner());
    match guard.send(request) {
        Ok(()) => Ok(()),
        Err(mpsc::SendError(request)) => {
            *guard = spawn();
            guard.send(request).map_err(|e| e.0)
        }
    }
}

fn worker(rx: mpsc::Receiver<Request>) {
    let mut state = State { sessions: Vec::new() };
    while let Ok(request) = rx.recv() {
        match request {
            Request::Grab(key, reply) => {
                let _ = reply.send(state.grab(key));
            }
            Request::Settle(key, budget, reply) => {
                let _ = reply.send(state.settle(key, budget));
            }
        }
    }
}

fn timeout(error: &windows::core::Error) -> bool {
    error.code() == DXGI_ERROR_WAIT_TIMEOUT
}
fn lost(error: &windows::core::Error) -> bool {
    error.code() == DXGI_ERROR_ACCESS_LOST
}

impl State {
    fn index(&self, target: Option<Key>) -> Option<usize> {
        match target {
            Some(key) => self.sessions.iter().position(|s| s.key == key),
            None if self.sessions.len() == 1 => Some(0),
            None => None,
        }
    }

    fn grab(&mut self, key: Key) -> Result<RgbaImage, GrabError> {
        if let Some(i) = self.sessions.iter().position(|s| s.key == key) {
            match self.sessions[i].pull(0) {
                Ok(Some(image)) => return Ok(image),
                Ok(None) => {
                    if let Some(cache) = self.sessions[i].cache.clone() {
                        return Ok(cache);
                    }
                    return Err(GrabError::Quiet);
                }
                Err(error) => {
                    self.sessions.remove(i);
                    if !lost(&error) {
                        return Err(GrabError::Loud(error.to_string()));
                    }
                }
            }
        }
        self.open_and_pull(key, true)
    }

    fn open_and_pull(&mut self, key: Key, retry: bool) -> Result<RgbaImage, GrabError> {
        match Session::open(key) {
            Ok(mut session) => match session.pull(40) {
                Ok(Some(image)) => {
                    if self.sessions.len() >= 8 {
                        self.sessions.remove(0);
                    }
                    self.sessions.push(session);
                    Ok(image)
                }
                Ok(None) => {
                    if self.sessions.len() >= 8 {
                        self.sessions.remove(0);
                    }
                    self.sessions.push(session);
                    Err(GrabError::Quiet)
                }
                Err(error) => Err(GrabError::Loud(error.to_string())),
            },
            // A stale duplication can make DuplicateOutput fail for a live output. Drop ours once and retry.
            Err(error) if retry && !self.sessions.is_empty() => {
                self.sessions.clear();
                self.open_and_pull(key, false)
            }
            Err(error) => Err(GrabError::Loud(error)),
        }
    }

    fn settle(&mut self, target: Option<Key>, budget: Duration) -> bool {
        let Some(index) = self.index(target) else { return false };
        let deadline = Instant::now() + budget;
        let mut seen = false;
        let mut previous = self.sessions[index].cache.clone();
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let slice = if seen { left.min(Duration::from_millis(32)) } else { left.min(Duration::from_millis(16)) };
            let wait_ms = slice.as_millis().clamp(1, 60_000) as u32;
            match self.sessions[index].next_frame(wait_ms) {
                // A dead session must not look like "the UI already settled". The caller
                // still has the full budget to sleep; this thread goes back to captures.
                Err(_) => {
                    self.sessions.remove(index);
                    return false;
                }
                Ok(Next::Timeout) => {
                    let (_, stop) = super::settle_step(seen, crate::visual_guard::DesktopPulse::Quiet);
                    if stop {
                        return true;
                    }
                }
                Ok(Next::Same | Next::Black) => {
                    let (_, stop) = super::settle_step(seen, crate::visual_guard::DesktopPulse::Pointer);
                    if stop {
                        return true;
                    }
                }
                Ok(Next::Frame(image)) => {
                    let pulse = crate::visual_guard::frame_pulse(previous.as_ref(), &image);
                    previous = Some(image);
                    let (next_seen, stop) = super::settle_step(seen, pulse);
                    seen = next_seen;
                    if stop {
                        return true;
                    }
                }
            }
        }
        true
    }
}

impl Session {
    fn open(key: Key) -> Result<Self, String> {
        unsafe { open_session(key) }.map_err(|e| e.to_string())
    }

    fn pull(&mut self, first_wait: u32) -> windows::core::Result<Option<RgbaImage>> {
        let mut latest = None;
        let mut wait = first_wait;
        let mut copies = 0u32;
        let mut blacks = 0u32;
        for _ in 0..8 {
            match self.next_frame(wait)? {
                Next::Timeout => break,
                Next::Same => wait = 0,
                Next::Black => {
                    blacks += 1;
                    if self.cache.is_some() || blacks >= 3 {
                        break;
                    }
                    wait = first_wait.max(16);
                }
                Next::Frame(image) => {
                    latest = Some(image);
                    copies += 1;
                    if copies >= 2 {
                        break;
                    }
                    wait = 0;
                }
            }
        }
        Ok(latest)
    }

    fn next_frame(&mut self, wait_ms: u32) -> windows::core::Result<Next> {
        unsafe { self.next_frame_raw(wait_ms) }
    }

    unsafe fn next_frame_raw(&mut self, wait_ms: u32) -> windows::core::Result<Next> {
        let (mut info, mut resource) = (DXGI_OUTDUPL_FRAME_INFO::default(), None);
        match self.duplication.AcquireNextFrame(wait_ms, &mut info, &mut resource) {
            Err(error) if timeout(&error) => return Ok(Next::Timeout),
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        let read = (|| {
            // The first presents after DuplicateOutput are often black (LastPresentTime == 0).
            if info.LastPresentTime == 0 {
                return Ok(if self.cache.is_some() { Next::Same } else { Next::Black });
            }
            // Pointer motion wakes duplication without changing the desktop image.
            if info.TotalMetadataBufferSize == 0 && self.cache.is_some() {
                return Ok(Next::Same);
            }
            let Some(resource) = resource.take() else {
                return Ok(Next::Same);
            };
            let image = self.read_mapped(resource)?;
            self.cache = Some(image.clone());
            Ok(Next::Frame(image))
        })();
        let _ = self.duplication.ReleaseFrame();
        read
    }

    unsafe fn read_mapped(&mut self, resource: IDXGIResource) -> windows::core::Result<RgbaImage> {
        let texture: ID3D11Texture2D = resource.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let (width, height) = (self.key.2, self.key.3);
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM || (desc.Width, desc.Height) != (width, height) {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x80004005u32 as i32),
                "DXGI桌面格式不是BGRA8",
            ));
        }
        if self.staging.is_none() {
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            self.staging = staging;
        }
        // Clone the COM pointer so the context call does not borrow the session.
        let staging = self.staging.clone().unwrap();
        self.context.CopyResource(&staging, &texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        self.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let row = width as usize * 4;
        let mut pixels = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            let src = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
            pixels.extend(src.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]));
        }
        self.context.Unmap(&staging, 0);
        RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
            windows::core::Error::new(windows::core::HRESULT(0x80004005u32 as i32), "无法组装DXGI图像")
        })
    }
}

unsafe fn open_session(key: Key) -> windows::core::Result<Session> {
    let (x, y, width, height) = key;
    let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
    let mut a = 0;
    while let Ok(adapter) = factory.EnumAdapters1(a) {
        a += 1;
        let mut o = 0;
        while let Ok(output) = adapter.EnumOutputs(o) {
            o += 1;
            let r = output.GetDesc()?;
            let c = r.DesktopCoordinates;
            if (c.left, c.top) != (x, y) || (c.right - c.left, c.bottom - c.top) != (width as i32, height as i32) {
                continue;
            }
            if !matches!(r.Rotation, DXGI_MODE_ROTATION_UNSPECIFIED | DXGI_MODE_ROTATION_IDENTITY) {
                return Err(windows::core::Error::new(
                    windows::core::HRESULT(0x80004005u32 as i32),
                    "没有匹配的DXGI输出（旋转/HDR/布局不符）",
                ));
            }
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                &adapter.cast::<IDXGIAdapter>()?,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let (device, context) = (device.unwrap(), context.unwrap());
            let duplication = output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)?;
            return Ok(Session { key, device, context, duplication, staging: None, cache: None });
        }
    }
    Err(windows::core::Error::new(
        windows::core::HRESULT(0x80004005u32 as i32),
        "没有匹配的DXGI输出（旋转/HDR/布局不符）",
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "needs a real desktop: cargo test dxgi_matches_xcap -- --ignored"]
    fn dxgi_matches_xcap() {
        for m in xcap::Monitor::all().unwrap() {
            let (x, y, w, h) = (m.x().unwrap(), m.y().unwrap(), m.width().unwrap(), m.height().unwrap());
            let image = super::grab(x, y, w, h).unwrap();
            assert_eq!(image.dimensions(), m.capture_image().unwrap().dimensions());
            assert!(image.pixels().any(|p| p.0[..3] != [0, 0, 0]), "black frame");
        }
    }
}
