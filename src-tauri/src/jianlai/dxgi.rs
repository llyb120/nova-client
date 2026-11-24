//! Windows monitor capture through a persistent DXGI Desktop Duplication pool (GPU-composited frame, no GDI).
//! Reuses `ID3D11Device` + `IDXGIOutputDuplication` + staging texture across turns (~0.05–2ms warm capture)
//! and yields immediately on cold static screens instead of stalling 6s so `xcap` can capture in ~10ms.
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
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

type RectKey = (i32, i32, u32, u32);

struct OutputSession {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<(u32, u32, ID3D11Texture2D)>,
    last_frame: Option<RgbaImage>,
}

struct SendSession(OutputSession);
// Safety: Every COM call on `OutputSession` happens strictly under `POOL` mutex lock (never concurrent).
unsafe impl Send for SendSession {}

static POOL: LazyLock<Mutex<HashMap<RectKey, SendSession>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Fast non-stalling DXGI grab:
/// - `Ok(Some(img))`: fresh GPU frame or valid unchanged frame cached from this active duplication session.
/// - `Ok(None)`: cold static screen (session is now warm in pool; caller grabs once via `xcap` and calls `seed_cache`)
///   or unsupported rotation/HDR format.
/// - `Err(msg)`: unexpected D3D11/DXGI error (caller falls back to `xcap`).
pub(super) fn try_grab(x: i32, y: i32, width: u32, height: u32) -> Result<Option<RgbaImage>, String> {
    try_grab_inner((x, y, width, height), 16).map_err(|e| e.to_string())
}

/// Seeds the warm duplication session's baseline frame when a cold static desktop was captured via `xcap`.
/// Subsequent unchanged calls return in ~0.05ms, while any DWM present immediately replaces `last_frame`.
pub(super) fn seed_cache(x: i32, y: i32, width: u32, height: u32, image: &RgbaImage) {
    if image.dimensions() != (width, height) {
        return;
    }
    if let Ok(mut pool) = POOL.lock() {
        if let Some(entry) = pool.get_mut(&(x, y, width, height)) {
            if entry.0.last_frame.is_none() {
                entry.0.last_frame = Some(image.clone());
            }
        }
    }
}

#[cfg(test)]
pub(super) fn grab(x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage, String> {
    if let Some(img) = try_grab_inner((x, y, width, height), 250).map_err(|e| e.to_string())? {
        return Ok(img);
    }
    Err("没有匹配的DXGI输出或屏幕完全静止".into())
}

fn try_grab_inner(key: RectKey, cold_wait_ms: u32) -> windows::core::Result<Option<RgbaImage>> {
    let mut pool = POOL.lock().unwrap();
    if let Some(entry) = pool.get_mut(&key) {
        match unsafe { acquire_frame(&mut entry.0, key.2, key.3, 0) } {
            Ok(Some(img)) => return Ok(Some(img)),
            Ok(None) => return Ok(entry.0.last_frame.clone()),
            Err(_) => {
                // `DXGI_ERROR_ACCESS_LOST` / mode switch / desktop lock: drop stale session and recreate once.
                pool.remove(&key);
            }
        }
    }

    let Some(mut session) = (unsafe { open_session(key.0, key.1, key.2, key.3)? }) else {
        return Ok(None);
    };
    let mut captured = None;
    for _ in 0..2 {
        match unsafe { acquire_frame(&mut session, key.2, key.3, cold_wait_ms) } {
            Ok(Some(img)) => {
                captured = Some(img);
                break;
            }
            Ok(None) => continue,
            Err(_) => return Ok(None),
        }
    }
    pool.insert(key, SendSession(session));
    Ok(captured)
}

unsafe fn open_session(x: i32, y: i32, width: u32, height: u32) -> windows::core::Result<Option<OutputSession>> {
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
                return Ok(None);
            }
            let base_adapter = adapter.cast::<IDXGIAdapter>()?;
            let output1 = output.cast::<IDXGIOutput1>()?;
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                &base_adapter,
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
            let duplication = output1.DuplicateOutput(&device)?;
            return Ok(Some(OutputSession {
                device,
                context,
                duplication,
                staging: None,
                last_frame: None,
            }));
        }
    }
    Ok(None)
}

unsafe fn acquire_frame(
    session: &mut OutputSession,
    width: u32,
    height: u32,
    timeout_ms: u32,
) -> windows::core::Result<Option<RgbaImage>> {
    let (mut info, mut resource) = (DXGI_OUTDUPL_FRAME_INFO::default(), None);
    match session.duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) {
        Ok(()) => {}
        Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
        Err(err) => return Err(err),
    }

    if info.LastPresentTime == 0 {
        let _ = session.duplication.ReleaseFrame();
        return Ok(None);
    }

    let copy_res = (|| -> windows::core::Result<Option<RgbaImage>> {
        let texture: ID3D11Texture2D = resource.unwrap().cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM || (desc.Width, desc.Height) != (width, height) {
            return Ok(None);
        }
        let needs_staging = session
            .staging
            .as_ref()
            .is_none_or(|(sw, sh, _)| (*sw, *sh) != (width, height));
        if needs_staging {
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            session.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            session.staging = Some((width, height, staging.unwrap()));
        }
        let staging = &session.staging.as_ref().unwrap().2;
        session.context.CopyResource(staging, &texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        session.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let row = width as usize * 4;
        let mut pixels = vec![0u8; row * height as usize];
        for y in 0..height as usize {
            let src = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
            pixels[y * row..(y + 1) * row].copy_from_slice(src);
        }
        session.context.Unmap(staging, 0);
        // Contiguous in-place BGRA -> RGBA swizzle (auto-vectorizes to SIMD `pshufb`).
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        Ok(RgbaImage::from_raw(width, height, pixels))
    })();

    let _ = session.duplication.ReleaseFrame();
    let img = copy_res?;
    if let Some(ref valid) = img {
        session.last_frame = Some(valid.clone());
    }
    Ok(img)
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
