//! D3D11 device, WinRT device wrapper, and the staging-texture readback ring.
//! The FrameArrived callback copies into a slot. The worker maps the slot and
//! reads it back. Both share one immediate context, guarded by a mutex and by
//! ID3D11Multithread protection.

use ::windows::core::Interface;
use ::windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use ::windows::Win32::Foundation::HMODULE;
use ::windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use ::windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use ::windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use ::windows::Win32::Graphics::Dxgi::IDXGIDevice;
use ::windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

use crate::error::{Code, RecordError};

pub struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
}

impl Device {
    pub fn create() -> Result<Self, RecordError> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| RecordError::new(Code::CaptureUnsupported, format!("D3D11CreateDevice failed: {e}")))?;
        let (Some(device), Some(context)) = (device, context) else {
            return Err(RecordError::new(Code::CaptureUnsupported, "D3D11CreateDevice returned no device"));
        };
        let multithread: ID3D11Multithread = device
            .cast()
            .map_err(|e| RecordError::new(Code::CaptureUnsupported, format!("ID3D11Multithread: {e}")))?;
        let _ = unsafe { multithread.SetMultithreadProtected(true) };

        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|e| RecordError::new(Code::CaptureUnsupported, format!("IDXGIDevice: {e}")))?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .map_err(|e| RecordError::new(Code::CaptureUnsupported, format!("CreateDirect3D11DeviceFromDXGIDevice: {e}")))?;
        let winrt: IDirect3DDevice = inspectable
            .cast()
            .map_err(|e| RecordError::new(Code::CaptureUnsupported, format!("IDirect3DDevice: {e}")))?;
        Ok(Self { device, context, winrt })
    }

    pub fn winrt(&self) -> &IDirect3DDevice {
        &self.winrt
    }

    /// Creates `slots` staging textures of `width` x `height` BGRA.
    pub fn readback(&self, width: u32, height: u32, slots: usize) -> Result<Readback, RecordError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = Vec::with_capacity(slots);
        for _ in 0..slots {
            let mut texture: Option<ID3D11Texture2D> = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
                .map_err(|e| RecordError::new(Code::CaptureFailed, format!("staging texture: {e}")))?;
            let texture = texture
                .ok_or_else(|| RecordError::new(Code::CaptureFailed, "staging texture: no texture returned"))?;
            staging.push(texture);
        }
        Ok(Readback { context: self.context.clone(), staging, width, height })
    }
}

/// Staging ring. Slots are addressed by index; ownership of a slot is handed
/// between the callback and the worker through channels.
pub struct Readback {
    context: ID3D11DeviceContext,
    staging: Vec<ID3D11Texture2D>,
    width: u32,
    height: u32,
}

// The COM pointers are only touched while the owning Mutex is held, and the
// device is multithread-protected.
unsafe impl Send for Readback {}

impl Readback {
    pub fn copy_in(&self, slot: usize, source: &ID3D11Texture2D) {
        unsafe { self.context.CopyResource(&self.staging[slot], source) };
    }

    /// Maps a slot and copies its rows, without padding, into `out` as packed
    /// top-down BGRA (`width * 4` bytes per row). `out` is reused between frames.
    pub fn read_packed(&self, slot: usize, out: &mut Vec<u8>) -> Result<(), RecordError> {
        let texture = &self.staging[slot];
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { self.context.Map(texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .map_err(|e| RecordError::new(Code::CaptureFailed, format!("Map staging slot: {e}")))?;
        let width = self.width as usize;
        let height = self.height as usize;
        let row = width * 4;
        let pitch = mapped.RowPitch as usize;
        out.clear();
        out.reserve(row * height);
        for y in 0..height {
            // Safety: Map returned pitch-strided rows of at least `row` bytes each.
            let src = unsafe { std::slice::from_raw_parts((mapped.pData as *const u8).add(y * pitch), row) };
            out.extend_from_slice(src);
        }
        unsafe { self.context.Unmap(texture, 0) };
        Ok(())
    }
}
