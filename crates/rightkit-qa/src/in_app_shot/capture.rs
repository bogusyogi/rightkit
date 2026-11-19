//! Window capture for a QA `--shot`: find the window by title, prepare it, and save a PNG of it.
//!
//! Windows only. Elsewhere `capture_window` returns an error. Window titles come in on the
//! `ShotWindowSpec` the options carry; nothing here names a window.

use super::options::ShotOptions;

/// Captures `hwnd` into a top-down RGBA buffer of `width * height * 4` bytes.
///
/// # Safety
///
/// `hwnd` must be a valid window handle that stays alive for the whole call, and `width` and
/// `height` must be positive and match the window's size so the pixel buffer allocation and the
/// `GetDIBits` copy agree.
#[cfg(target_os = "windows")]
unsafe fn print_window(
    hwnd: windows_sys::Win32::Foundation::HWND,
    width: i32,
    height: i32,
) -> Result<Vec<u8>, String> {
    use std::{mem::size_of, ptr::null_mut};

    use windows_sys::Win32::{
        Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
            ReleaseDC, SelectObject, BITMAPINFO, BI_RGB, DIB_RGB_COLORS,
        },
        Storage::Xps::PrintWindow,
        UI::WindowsAndMessaging::PW_RENDERFULLCONTENT,
    };

    // Bound allocation before any Win32 resource acquisition or signed arithmetic.
    let bytes = (width as u32)
        .checked_mul(height as u32)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| width > 0 && height > 0 && *bytes <= 256 * 1024 * 1024)
        .ok_or_else(|| format!("invalid or oversized shot bounds {width}x{height}"))?;
    let screen_dc = GetDC(null_mut());
    if screen_dc.is_null() {
        return Err("GetDC failed".to_string());
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_null() {
        ReleaseDC(null_mut(), screen_dc);
        return Err("CreateCompatibleDC failed".to_string());
    }
    let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
    if bitmap.is_null() {
        DeleteDC(mem_dc);
        ReleaseDC(null_mut(), screen_dc);
        return Err("CreateCompatibleBitmap failed".to_string());
    }
    let old_bitmap = SelectObject(mem_dc, bitmap);
    if old_bitmap.is_null() || old_bitmap as isize == -1 {
        DeleteObject(bitmap);
        DeleteDC(mem_dc);
        ReleaseDC(null_mut(), screen_dc);
        return Err("SelectObject failed".to_string());
    }

    let printed = PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT);
    if printed == 0 {
        SelectObject(mem_dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(mem_dc);
        ReleaseDC(null_mut(), screen_dc);
        return Err("PrintWindow failed".to_string());
    }

    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    info.bmiHeader.biSizeImage = bytes;

    let mut pixels = vec![0u8; bytes as usize];
    // GetDIBits requires this bitmap to be deselected from its device context.
    SelectObject(mem_dc, old_bitmap);
    let rows = GetDIBits(
        mem_dc,
        bitmap,
        0,
        height as u32,
        pixels.as_mut_ptr() as *mut _,
        &mut info,
        DIB_RGB_COLORS,
    );

    SelectObject(mem_dc, old_bitmap);
    DeleteObject(bitmap);
    DeleteDC(mem_dc);
    ReleaseDC(null_mut(), screen_dc);

    if rows != height {
        return Err("GetDIBits failed".to_string());
    }

    for bgra in pixels.as_chunks_mut::<4>().0 {
        bgra.swap(0, 2);
        if bgra[3] == 0 {
            bgra[3] = 255;
        }
    }
    Ok(pixels)
}

#[cfg(not(target_os = "windows"))]
fn capture_window(_options: &ShotOptions) -> Result<(), String> {
    Err("--shot is currently implemented for Windows only".to_string())
}

pub fn capture(options: &ShotOptions) -> Result<(), String> {
    capture_window(options)
}

#[cfg(target_os = "windows")]
fn capture_window(options: &ShotOptions) -> Result<(), String> {
    use std::ptr::null_mut;

    use tiny_skia::{IntSize, Pixmap};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        },
    };

    struct WindowSearch {
        pid: u32,
        title: &'static str,
        hwnd: HWND,
    }

    unsafe extern "system" fn enum_windows(hwnd: HWND, lparam: LPARAM) -> i32 {
        let search = &mut *(lparam as *mut WindowSearch);
        let mut window_pid = 0;
        GetWindowThreadProcessId(hwnd, &mut window_pid);
        if window_pid != search.pid {
            return 1;
        }

        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return 1;
        }
        let mut title = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if copied <= 0 {
            return 1;
        }
        let title = String::from_utf16_lossy(&title[..copied as usize]);
        if title == search.title {
            search.hwnd = hwnd;
            return 0;
        }

        1
    }

    let titles = options.window.titles;
    let first_title = titles
        .first()
        .ok_or_else(|| "shot window titles must not be empty".to_string())?;
    let hwnd = unsafe {
        let mut search = WindowSearch {
            pid: std::process::id(),
            title: first_title,
            hwnd: null_mut(),
        };
        for title in titles {
            search.title = title;
            EnumWindows(Some(enum_windows), &mut search as *mut _ as LPARAM);
            if !search.hwnd.is_null() {
                break;
            }
        }
        search.hwnd
    };
    if hwnd.is_null() {
        return Err(format!("could not find {} window", titles.join(" / ")));
    }

    // SAFETY: `hwnd` is a live, non-null window of this process (checked above).
    unsafe { prepare_window(hwnd, options.foreground)? };
    std::thread::sleep(options.delay);
    // A window can close during the delay; refuse a handle now owned by another process.
    let mut current_pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut current_pid) };
    if current_pid != std::process::id() {
        return Err("shot window no longer belongs to this process".to_string());
    }

    // SAFETY: `hwnd` was just found by `EnumWindows` for this process and is non-null.
    let rect = unsafe { window_rect(hwnd)? };
    let width = rect
        .right
        .checked_sub(rect.left)
        .ok_or("shot width overflow")?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .ok_or("shot height overflow")?;
    if width <= 0 || height <= 0 {
        return Err(format!("invalid shot bounds {width}x{height}"));
    }

    // SAFETY: `hwnd` is live and `width`/`height` are the positive bounds read from it above.
    let pixels = unsafe { print_window(hwnd, width, height)? };
    if let Some(parent) = options
        .path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create shot directory {}: {err}",
                parent.display()
            )
        })?;
    }

    let size = IntSize::from_wh(width as u32, height as u32)
        .ok_or_else(|| format!("invalid pixmap size {width}x{height}"))?;
    let pixmap =
        Pixmap::from_vec(pixels, size).ok_or_else(|| "failed to create shot pixmap".to_string())?;
    pixmap
        .save_png(&options.path)
        .map_err(|err| format!("failed to save screenshot: {err}"))
}

/// Prepares `hwnd` for capture: restores and foregrounds it, or moves it offscreen without
/// activating it.
///
/// # Safety
///
/// `hwnd` must be a valid window handle (it is passed straight to Win32), and the call must be
/// made on a thread allowed to manipulate that window.
#[cfg(target_os = "windows")]
unsafe fn prepare_window(
    hwnd: windows_sys::Win32::Foundation::HWND,
    foreground: bool,
) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOP, SWP_NOACTIVATE, SWP_NOSIZE,
        SWP_NOZORDER, SW_RESTORE, SW_SHOWNOACTIVATE,
    };

    if foreground {
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
        return Ok(());
    }

    let moved = SetWindowPos(
        hwnd,
        HWND_TOP,
        -32000,
        -32000,
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    if moved == 0 {
        return Err("SetWindowPos offscreen failed".to_string());
    }
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    Ok(())
}

/// Reads full window bounds matching PrintWindow's whole-window bitmap.
///
/// # Safety
///
/// `hwnd` must be a valid window handle of this process.
#[cfg(target_os = "windows")]
unsafe fn window_rect(
    hwnd: windows_sys::Win32::Foundation::HWND,
) -> Result<windows_sys::Win32::Foundation::RECT, String> {
    use windows_sys::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetWindowRect};
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect) == 0 {
        return Err("failed to read shot window bounds".to_string());
    }
    Ok(rect)
}
