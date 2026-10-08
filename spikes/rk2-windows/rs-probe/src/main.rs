// RK2 windows-rs feasibility probe (spike item 3). Standalone; not part of the RightKit workspace.
//
// Creates a GraphicsCaptureItem for an HWND (IGraphicsCaptureItemInterop::CreateForWindow), a D3D11 device wrapped with
// CreateDirect3D11DeviceFromDXGIDevice, a Direct3D11CaptureFramePool::CreateFreeThreaded, sets IsBorderRequired=false and
// IsCursorCaptureEnabled=false, grabs one frame of the fixture, copies it to a staging texture, reads it back, and writes
// frame.bmp (32-bit top-down BMP) plus probe.log to the output directory.
//
// Usage: rk2-rs-probe <hwnd decimal> <out dir>

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{factory, Error, Interface, Result};
use windows::Foundation::Metadata::ApiInformation;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind, GraphicsCaptureItem,
    GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic, IsWindowVisible};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: rk2-rs-probe <hwnd decimal> <out dir>");
        std::process::exit(2);
    }
    let hwnd_val: isize = match args[1].parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bad hwnd: {e}");
            std::process::exit(2);
        }
    };
    let out = Path::new(&args[2]);
    if let Err(e) = fs::create_dir_all(out) {
        eprintln!("create out dir: {e}");
        std::process::exit(1);
    }
    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
    match run(hwnd, out) {
        Ok(()) => println!("ok"),
        Err(e) => {
            let msg = format!("error: {e}\n");
            let _ = fs::write(out.join("probe.log"), &msg);
            eprint!("{msg}");
            std::process::exit(1);
        }
    }
}

fn run(hwnd: HWND, out: &Path) -> Result<()> {
    let mut log = String::new();

    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };

    // Placement snapshot of the target HWND.
    let mut rc = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rc)? };
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let iconic = unsafe { IsIconic(hwnd) }.as_bool();
    log.push_str(&format!(
        "rect={},{},{},{} visible={} iconic={}\n",
        rc.left, rc.top, rc.right, rc.bottom, visible, iconic
    ));

    // Borderless access (Windows 11 build 22000+). Recorded; the probe continues on any status.
    let access = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)?.get()?;
    log.push_str(&format!("graphicsCaptureAccessBorderless={:?}\n", access));

    // GraphicsCaptureItem for the HWND.
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
    let size = item.Size()?;
    log.push_str(&format!("item name={} size={}x{}\n", item.DisplayName()?, size.Width, size.Height));

    // D3D11 device, wrapped as a WinRT IDirect3DDevice.
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
        )?
    };
    let device = device.ok_or_else(|| Error::from(E_FAIL))?;
    let context = context.ok_or_else(|| Error::from(E_FAIL))?;
    let dxgi: IDXGIDevice = device.cast()?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
    let winrt_device: IDirect3DDevice = inspectable.cast()?;

    // Free-threaded frame pool and session.
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )?;
    let session: GraphicsCaptureSession = pool.CreateCaptureSession(&item)?;
    session.SetIsBorderRequired(false)?;
    session.SetIsCursorCaptureEnabled(false)?;
    log.push_str(&format!(
        "IsBorderRequired={} IsCursorCaptureEnabled={}\n",
        session.IsBorderRequired()?,
        session.IsCursorCaptureEnabled()?
    ));
    // Feature presence check (not in the SDK metadata for this build, recorded as a windows-rs gap probe).
    let has_min_interval = ApiInformation::IsPropertyPresent(
        &windows::core::HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &windows::core::HSTRING::from("MinUpdateInterval"),
    )
    .unwrap_or(false);
    log.push_str(&format!("ApiInformation GraphicsCaptureSession.MinUpdateInterval present={}\n", has_min_interval));

    session.StartCapture()?;

    // Skip the first frames so the fixture has painted; keep the last of 10 frames.
    let mut got = 0;
    let mut last = None;
    let deadline = Instant::now() + Duration::from_secs(8);
    while got < 10 {
        if Instant::now() > deadline {
            return Err(Error::from(E_FAIL));
        }
        match pool.TryGetNextFrame() {
            Ok(frame) => {
                got += 1;
                if let Some(prev) = last.replace(frame) {
                    prev.Close()?;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let frame = last.ok_or_else(|| Error::from(E_FAIL))?;
    log.push_str(&format!("frames consumed before grab={}\n", got));

    // Surface -> ID3D11Texture2D.
    let surface = frame.Surface()?;
    let access_iface: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    let texture: ID3D11Texture2D = unsafe { access_iface.GetInterface()? };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };

    // Staging copy and CPU readback.
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: desc.Width,
        Height: desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging))? };
    let staging = staging.ok_or_else(|| Error::from(E_FAIL))?;
    unsafe { context.CopyResource(&staging, &texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };

    let w = desc.Width as usize;
    let h = desc.Height as usize;
    let pitch = mapped.RowPitch as usize;
    let mut bgra = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = unsafe { std::slice::from_raw_parts((mapped.pData as *const u8).add(y * pitch), w * 4) };
        bgra[y * w * 4..(y + 1) * w * 4].copy_from_slice(src);
    }
    unsafe { context.Unmap(&staging, 0) };
    log.push_str(&format!("frame {}x{} rowPitch={} readbackBytes={}\n", w, h, pitch, bgra.len()));

    write_bmp(&out.join("frame.bmp"), w, h, &bgra).map_err(|e| Error::new(E_FAIL, e.to_string()))?;

    frame.Close()?;
    session.Close()?;
    pool.Close()?;
    fs::write(out.join("probe.log"), &log).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
    print!("{log}");
    Ok(())
}

// 32-bit BGRA, top-down (negative height), no compression.
fn write_bmp(path: &Path, w: usize, h: usize, bgra: &[u8]) -> std::io::Result<()> {
    let data_len = (w * h * 4) as u32;
    let file_len = 14 + 40 + data_len;
    let mut out = Vec::with_capacity(file_len as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_len.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(bgra);
    fs::write(path, out)
}
