//! Windows monitor capture through the DXGI Desktop Duplication API (GPU-composited frame, no GDI).
//! Any failure (rotated output, HDR format, secure desktop, session limits) is reported to the caller,
//! which falls back to xcap.
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

/// Grabs the output whose desktop rectangle is exactly (x, y, width, height).
pub(super) fn grab(x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage, String> {
    unsafe { find(x, y, width, height) }.map_err(|e| e.to_string())?.ok_or_else(|| "没有匹配的DXGI输出（旋转/HDR/布局不符）".into())
}

unsafe fn find(x: i32, y: i32, width: u32, height: u32) -> windows::core::Result<Option<RgbaImage>> {
    let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
    let mut a = 0;
    while let Ok(adapter) = factory.EnumAdapters1(a) {
        a += 1;
        let mut o = 0;
        while let Ok(output) = adapter.EnumOutputs(o) {
            o += 1;
            let r = output.GetDesc()?;
            let c = r.DesktopCoordinates;
            if (c.left, c.top) != (x, y) || (c.right - c.left, c.bottom - c.top) != (width as i32, height as i32) { continue; }
            if !matches!(r.Rotation, DXGI_MODE_ROTATION_UNSPECIFIED | DXGI_MODE_ROTATION_IDENTITY) { return Ok(None); }
            return duplicate(&adapter.cast::<IDXGIAdapter>()?, &output.cast::<IDXGIOutput1>()?, width, height);
        }
    }
    Ok(None)
}

unsafe fn duplicate(adapter: &IDXGIAdapter, output: &IDXGIOutput1, width: u32, height: u32) -> windows::core::Result<Option<RgbaImage>> {
    let (mut device, mut context) = (None, None);
    D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0), None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))?;
    let (device, context) = (device.unwrap(), context.unwrap());
    let duplication = output.DuplicateOutput(&device)?;
    // The first frames of a fresh duplication carry no desktop image (LastPresentTime == 0, black texture);
    // a static screen therefore times out here and the caller falls back to xcap.
    let (mut info, mut resource) = (DXGI_OUTDUPL_FRAME_INFO::default(), None);
    for attempt in 0..20 {
        duplication.AcquireNextFrame(300, &mut info, &mut resource)?;
        if info.LastPresentTime != 0 { break; }
        if attempt == 19 { return Ok(None); }
        resource = None;
        duplication.ReleaseFrame()?;
    }
    let image = (|| {
        let texture: ID3D11Texture2D = resource.unwrap().cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM || (desc.Width, desc.Height) != (width, height) { return Ok(None); }
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.unwrap();
        context.CopyResource(&staging, &texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let row = width as usize * 4;
        let mut pixels = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            let src = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
            pixels.extend(src.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]));
        }
        context.Unmap(&staging, 0);
        Ok(RgbaImage::from_raw(width, height, pixels))
    })();
    let _ = duplication.ReleaseFrame();
    image
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
