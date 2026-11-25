//! Delivery of a finished screenshot on Windows: where it goes (the Desktop known folder, a name
//! that does not overwrite), a PNG file through WIC, and a `CF_DIB` copy on the clipboard. Pixels
//! are `0xAARRGGBB` `u32`s, row-major, top row first (BGRA in memory on little endian). The
//! application captures, names the file's stem and does its own logging and presentation.

use ::windows::core::{Error, IUnknown, Interface, PCWSTR};
use ::windows::Win32::Foundation::{GlobalFree, E_FAIL, HANDLE, HWND};
use ::windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
use ::windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
};
use ::windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
use ::windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, IStream, CLSCTX_INPROC_SERVER,
};
use ::windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use ::windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use ::windows::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CF_DIB: u32 = 8;
const GENERIC_WRITE: u32 = 0x4000_0000;
/// Attempts to open the clipboard (another program may hold it for a moment), 20 ms apart.
const CLIPBOARD_TRIES: u32 = 10;
const CLIPBOARD_STEP_MS: u64 = 20;
/// Names tried after `stem` before giving up on a free one (`stem (2)` to `stem (999)`).
const MAX_SUFFIX: u32 = 1000;

/// Why a delivery step failed: the step, and the OS error when there was one.
#[derive(Debug)]
pub struct StillError {
    /// `"open"` (the clipboard stayed busy), `"empty"`, `"alloc"`, `"lock"` or `"set"`.
    pub stage: &'static str,
    pub error: Option<Error>,
}

/// The user's Desktop known folder; `%USERPROFILE%\Desktop` when the shell cannot name it.
pub fn desktop_dir() -> Option<PathBuf> {
    // SAFETY: the returned buffer is copied and freed with CoTaskMemFree.
    unsafe {
        if let Ok(path) = SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), None) {
            let text = path.to_string();
            CoTaskMemFree(Some(path.0 as *const c_void));
            if let Ok(text) = text {
                return Some(PathBuf::from(text));
            }
        }
    }
    std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Desktop"))
}

/// `dir\<stem>.<extension>`, or `dir\<stem> (2).<extension>` and on while that name is taken, so a
/// new file never replaces an old one.
pub fn unique_path(dir: &Path, stem: &str, extension: &str) -> PathBuf {
    let mut path = dir.join(format!("{stem}.{extension}"));
    let mut n = 2;
    while path.exists() && n < MAX_SUFFIX {
        path = dir.join(format!("{stem} ({n}).{extension}"));
        n += 1;
    }
    path
}

/// Writes `pixels` (`width * height`) as a PNG at `path` through WIC. COM must be initialised on
/// the calling thread. On failure a partly written file may remain; the caller removes it.
pub fn encode_png(width: u32, height: u32, pixels: &[u32], path: &Path) -> Result<(), Error> {
    if pixels.len() < width as usize * height as usize {
        return Err(Error::from(E_FAIL));
    }
    // SAFETY: a u32 slice viewed as its own bytes (BGRA in memory order on little endian).
    let bytes =
        unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 4) };
    let name: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: COM is initialised on this thread; every interface is released on drop and
    // `name` / `bytes` outlive the calls.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(
            &CLSID_WICImagingFactory,
            None::<&IUnknown>,
            CLSCTX_INPROC_SERVER,
        )?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(name.as_ptr()), GENERIC_WRITE)?;
        let istream: IStream = stream.cast()?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&istream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
        frame.Initialize(None::<&IPropertyBag2>)?;
        frame.SetSize(width, height)?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WritePixels(height, width * 4, bytes)?;
        frame.Commit()?;
        encoder.Commit()?;
    }
    Ok(())
}

fn open_clipboard(owner: HWND) -> bool {
    for _ in 0..CLIPBOARD_TRIES {
        // SAFETY: a plain call; the owner handle may be null or stale, which the API tolerates.
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(CLIPBOARD_STEP_MS));
    }
    false
}

/// Replaces the clipboard's content with `pixels` (`width * height`, top row first) as a `CF_DIB`
/// bitmap, with every pixel as given (the caller makes alpha opaque when it must be). `owner` is an
/// `HWND` kept as an integer, or zero. The clipboard is opened with a few retries and always
/// closed again.
pub fn copy_dib_to_clipboard(
    owner: isize,
    width: usize,
    height: usize,
    pixels: &[u32],
) -> Result<(), StillError> {
    if width == 0 || height == 0 || pixels.len() < width * height {
        return Err(StillError {
            stage: "alloc",
            error: None,
        });
    }
    let owner = HWND(owner as *mut c_void);
    if !open_clipboard(owner) {
        return Err(StillError {
            stage: "open",
            error: None,
        });
    }
    let header_len = size_of::<BITMAPINFOHEADER>();
    // SAFETY: the clipboard is open; the HGLOBAL is filled while locked and either handed to
    // the system by SetClipboardData (which then owns it) or freed here.
    let copied = unsafe {
        'fill: {
            if let Err(error) = EmptyClipboard() {
                break 'fill Err(StillError {
                    stage: "empty",
                    error: Some(error),
                });
            }
            let memory = match GlobalAlloc(GMEM_MOVEABLE, header_len + width * height * 4) {
                Ok(memory) => memory,
                Err(error) => {
                    break 'fill Err(StillError {
                        stage: "alloc",
                        error: Some(error),
                    });
                }
            };
            let destination = GlobalLock(memory).cast::<u8>();
            if destination.is_null() {
                let _ = GlobalFree(Some(memory));
                break 'fill Err(StillError {
                    stage: "lock",
                    error: None,
                });
            }
            let header = BITMAPINFOHEADER {
                biSize: header_len as u32,
                biWidth: width as i32,
                biHeight: height as i32, // positive: bottom-up rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (width * height * 4) as u32,
                ..Default::default()
            };
            std::ptr::copy_nonoverlapping(
                (&header as *const BITMAPINFOHEADER).cast::<u8>(),
                destination,
                header_len,
            );
            let body = destination.add(header_len).cast::<u32>();
            for row in 0..height {
                let source = &pixels[(height - 1 - row) * width..(height - row) * width];
                std::ptr::copy_nonoverlapping(source.as_ptr(), body.add(row * width), width);
            }
            let _ = GlobalUnlock(memory);
            match SetClipboardData(CF_DIB, Some(HANDLE(memory.0))) {
                Ok(_) => Ok(()),
                Err(error) => {
                    let _ = GlobalFree(Some(memory));
                    Err(StillError {
                        stage: "set",
                        error: Some(error),
                    })
                }
            }
        }
    };
    // SAFETY: closes the clipboard this function opened.
    let _ = unsafe { CloseClipboard() };
    copied
}
