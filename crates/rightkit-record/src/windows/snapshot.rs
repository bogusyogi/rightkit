//! One still frame of a window through Windows.Graphics.Capture, as plain pixels.
//!
//! A screen copy of a window's rectangle (`BitBlt`) puts anything lying over the window in the
//! picture (another window, a toast, an overlay). This captures the window itself: a
//! `GraphicsCaptureItem` for its handle, one frame from a free-threaded frame pool, copied through
//! a staging texture. It is meant for a thread that is a COM apartment; nothing waits on a message
//! loop, the frame is polled for. A failure at any step is an `Err`, so a caller can fall back to a
//! screen copy. (The recording backend of this module is the continuous counterpart.)

use ::windows::core::{factory, Error, Interface, Result};
use ::windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use ::windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use ::windows::Graphics::DirectX::DirectXPixelFormat;
use ::windows::Win32::Foundation::{E_FAIL, HMODULE, HWND};
use ::windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use ::windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use ::windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use ::windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use ::windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use std::time::Duration;

/// How many times a first frame is polled for, `FRAME_STEP_MS` apart. A window that is not
/// changing still delivers one frame when the session starts.
const FRAME_WAIT_STEPS: u32 = 100;
const FRAME_STEP_MS: u64 = 15;
/// Frames in the pool; one is taken, so two keeps the next free.
const POOL_FRAMES: i32 = 2;
/// The largest side accepted.
const MAX_SIDE: u32 = 8192;

/// One captured window: `0xAARRGGBB` pixels, row-major, `width * height` of them.
pub struct WindowImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

/// The window behind `window` (an `HWND` kept as an integer), as it is drawn, with or without the
/// pointer. Errs when capture is unsupported, refused for that window, or no frame came.
pub fn capture_window_pixels(window: isize, cursor: bool) -> Result<WindowImage> {
    capture(HWND(window as *mut std::ffi::c_void), cursor)
}

fn capture(hwnd: HWND, cursor: bool) -> Result<WindowImage> {
    // SAFETY: plain COM and WinRT calls on this thread. Every interface is a smart pointer
    // released on drop; the mapped texture is unmapped before it is released and its memory is
    // only read inside the mapping, within the row pitch and height the texture reports.
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device = device.ok_or_else(|| Error::from(E_FAIL))?;
        let context = device.GetImmediateContext()?;
        let dxgi: IDXGIDevice = device.cast()?;
        let winrt_device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;

        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd)?;
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            POOL_FRAMES,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Older Windows has neither setting: the pointer is then left out (the default) and the
        // yellow capture border may show for a moment.
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture()?;

        let mut frame = None;
        for _ in 0..FRAME_WAIT_STEPS {
            match pool.TryGetNextFrame() {
                Ok(next) => {
                    frame = Some(next);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(FRAME_STEP_MS)),
            }
        }
        let result = match frame {
            Some(frame) => {
                let copied = read_frame(&device, &context, &frame);
                let _ = frame.Close();
                copied
            }
            None => Err(Error::from(E_FAIL)),
        };
        let _ = session.Close();
        let _ = pool.Close();
        result
    }
}

/// The frame's texture copied to a CPU-readable one and read out, cut to the window's own size
/// (the pool's texture can be larger than what the window drew).
///
/// # Safety
/// `device`, `context` and `frame` must belong to the calling thread's capture in progress.
unsafe fn read_frame(
    device: &ID3D11Device,
    context: &::windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    frame: &::windows::Graphics::Capture::Direct3D11CaptureFrame,
) -> Result<WindowImage> {
    // SAFETY: see `capture`; the caller upholds this function's contract.
    unsafe {
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let texture: ID3D11Texture2D = access.GetInterface()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let content = frame.ContentSize()?;
        let width = (content.Width.max(0) as u32).min(desc.Width);
        let height = (content.Height.max(0) as u32).min(desc.Height);
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(Error::from(E_FAIL));
        }
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.ok_or_else(|| Error::from(E_FAIL))?;
        context.CopyResource(&staging, &texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let (width, height) = (width as usize, height as usize);
        let mut pixels = vec![0u32; width * height];
        let rows = if mapped.pData.is_null() { 0 } else { height };
        for row in 0..rows {
            // One row of the mapped texture: `width` 32-bit pixels at the row pitch.
            let source = std::slice::from_raw_parts(
                mapped
                    .pData
                    .cast::<u8>()
                    .add(row * mapped.RowPitch as usize)
                    .cast::<u32>(),
                width,
            );
            pixels[row * width..(row + 1) * width].copy_from_slice(source);
        }
        context.Unmap(&staging, 0);
        if rows == 0 {
            return Err(Error::from(E_FAIL));
        }
        Ok(WindowImage {
            width,
            height,
            pixels,
        })
    }
}
