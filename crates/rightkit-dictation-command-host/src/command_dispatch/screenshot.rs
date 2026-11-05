#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(any(target_os = "windows", target_os = "macos"))]
use super::DispatchOutcome;
use super::{DispatchError, DispatchResult};

#[cfg(target_os = "windows")]
const SCREENSHOT_PNG_NAME_PREFIX: &str = "Screenshot";

#[cfg(target_os = "windows")]
pub(super) fn dispatch_windows_screenshot() -> DispatchResult {
    let raw_destination = crate::settings::screenshot_destination();
    let destination = normalize_screenshot_destination(&raw_destination);
    let dispatch_started = Instant::now();
    screenshot_step(
        raw_destination.as_str(),
        "start",
        dispatch_started,
        true,
        None,
    );
    screenshot_step(
        raw_destination.as_str(),
        "destination_normalized",
        dispatch_started,
        true,
        Some(serde_json::json!({ "destination": destination })),
    );
    let result: DispatchResult = match destination {
        "disk" => {
            let path = save_clipboard_image_to_windows_screenshots(raw_destination.as_str())?;
            Ok(DispatchOutcome::new(
                "screenshot",
                format!("{} + clipboard image", path.to_string_lossy()),
            ))
        }
        _ => {
            capture_full_screen_to_clipboard(raw_destination.as_str())?;
            Ok(DispatchOutcome::new(
                "screenshot",
                "full-screen clipboard image",
            ))
        }
    };
    match &result {
        Ok(outcome) => screenshot_step(
            raw_destination.as_str(),
            "done",
            dispatch_started,
            true,
            Some(serde_json::json!({ "detail": outcome.detail.clone() })),
        ),
        Err(err) => screenshot_step(
            raw_destination.as_str(),
            "failed",
            dispatch_started,
            false,
            Some(serde_json::json!({ "code": err.code, "message": err.message.clone() })),
        ),
    };
    result
}

#[cfg(target_os = "macos")]
pub(super) fn dispatch_macos_screenshot() -> DispatchResult {
    let destination = crate::settings::screenshot_destination();
    if macos_destination_saves(&destination) {
        let dir = macos_screenshots_dir()?;
        let path = dir.join(format!(
            "Screenshot {}.png",
            chrono::Local::now().format("%Y-%m-%d at %H.%M.%S%.3f")
        ));
        capture_macos_screen(Some(&path))?;
        return Ok(DispatchOutcome::new(
            "screenshot",
            format!("{} + clipboard image", path.display()),
        ));
    }

    capture_macos_screen(None)?;
    Ok(DispatchOutcome::new(
        "screenshot",
        "full-screen clipboard image",
    ))
}

#[cfg(target_os = "macos")]
fn macos_destination_saves(destination: &str) -> bool {
    matches!(destination, "disk" | "both")
}

#[cfg(target_os = "macos")]
fn macos_screenshots_dir() -> Result<PathBuf, DispatchError> {
    let dir = crate::user_dirs::account_home_dir()
        .map(|home| home.join("Pictures").join("Screenshots"))
        .unwrap_or_else(|| crate::settings::app_data_root().join("Screenshots"));
    std::fs::create_dir_all(&dir).map_err(|err| {
        DispatchError::new(
            "E_SCREENSHOT_DIR",
            format!("create screenshot directory {}: {err}", dir.display()),
        )
    })?;
    Ok(dir)
}

#[cfg(target_os = "macos")]
fn capture_macos_screen(path: Option<&Path>) -> Result<(), DispatchError> {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        fn rightkit_dictation_capture_screen_excluding_app(
            bundle_id: *const c_char,
            output_path: *const c_char,
            error_buffer: *mut c_char,
            error_capacity: usize,
        ) -> c_int;
    }

    let bundle_id = CString::new(crate::host::identity().bundle_id)
        .map_err(|_| DispatchError::new("E_SCREENSHOT_CONFIG", "host bundle id contains NUL"))?;
    let output_path = path
        .map(|path| CString::new(path.to_string_lossy().as_bytes()))
        .transpose()
        .map_err(|_| DispatchError::new("E_SCREENSHOT_FILE", "screenshot path contains NUL"))?;
    let mut error = [0 as c_char; 1024];
    let result = unsafe {
        rightkit_dictation_capture_screen_excluding_app(
            bundle_id.as_ptr(),
            output_path
                .as_ref()
                .map_or(std::ptr::null(), |path| path.as_ptr()),
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if result == 0 {
        return Ok(());
    }
    let message = unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Err(DispatchError::new(
        "E_SCREENSHOT_CAPTURE",
        if message.is_empty() {
            format!("ScreenCaptureKit failed with code {result}")
        } else {
            message
        },
    ))
}

#[cfg(target_os = "windows")]
fn screenshot_step(
    destination: &str,
    step: &str,
    started: Instant,
    ok: bool,
    detail: Option<serde_json::Value>,
) {
    let elapsed_ms = duration_ms_u64(started.elapsed());
    let mut payload = serde_json::json!({
        "event": "screenshot_step",
        "schema_version": 1,
        "ts_ms": system_time_ms(),
        "destination": destination,
        "step": step,
        "elapsed_ms": elapsed_ms,
        "ok": ok,
    });
    if let Some(detail) = detail {
        payload["detail"] = detail;
    }
    super::entry::append_command_dispatch_event(&payload);
    tracing::info!(target: "command_dispatch", "{}", payload);
}

#[cfg(target_os = "windows")]
fn system_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(duration_ms_u64)
        .unwrap_or(0)
}

#[cfg(target_os = "windows")]
fn duration_ms_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(target_os = "windows")]
fn capture_full_screen_to_clipboard(destination: &str) -> Result<(), DispatchError> {
    let started = Instant::now();
    let before = clipboard_sequence_number();
    let hide_started = Instant::now();
    screenshot_step(destination, "pill_exclude_start", hide_started, true, None);
    let _pill_guard = PillCaptureExclusion::exclude(destination);
    screenshot_step(
        destination,
        "pill_exclude_done",
        hide_started,
        true,
        Some(serde_json::json!({
            "excluded_count": _pill_guard.len(),
            "method": "hide",
        })),
    );
    let chord_started = Instant::now();
    screenshot_step(
        destination,
        "clipboard_chord_start",
        chord_started,
        true,
        Some(serde_json::json!({ "sequence_before": before })),
    );
    super::keys::send_chord("ctrl+printscreen").map_err(|err| {
        let message = err.message;
        screenshot_step(
            destination,
            "clipboard_chord_sent",
            chord_started,
            false,
            Some(serde_json::json!({ "message": message.clone() })),
        );
        DispatchError::new(
            "E_SCREENSHOT_CLIPBOARD",
            format!("Ctrl+PrintScreen failed: {message}"),
        )
    })?;
    screenshot_step(
        destination,
        "clipboard_chord_sent",
        chord_started,
        true,
        None,
    );
    let wait_started = Instant::now();
    match wait_for_clipboard_image_after(before, Duration::from_millis(1800)) {
        Ok(()) => screenshot_step(
            destination,
            "clipboard_image_found",
            wait_started,
            true,
            Some(serde_json::json!({ "sequence_after": clipboard_sequence_number() })),
        ),
        Err(err) => {
            screenshot_step(
                destination,
                "clipboard_image_found",
                wait_started,
                false,
                Some(serde_json::json!({ "message": err.message.clone() })),
            );
            return Err(err);
        }
    }
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis(),
        "Windows screenshot captured pixels to clipboard with Ctrl+PrintScreen"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn normalize_screenshot_destination(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "disk" | "save" | "pictures" | "both" => "disk",
        _ => "clipboard",
    }
}

#[cfg(target_os = "windows")]
fn save_clipboard_image_to_windows_screenshots(
    destination: &str,
) -> Result<PathBuf, DispatchError> {
    let step_started = Instant::now();
    capture_full_screen_to_clipboard(destination)?;
    let read_started = Instant::now();
    let image = read_clipboard_image_as_rgba().inspect_err(|err| {
        screenshot_step(
            destination,
            "clipboard_image_read",
            read_started,
            false,
            Some(serde_json::json!({ "message": err.message.clone() })),
        );
    })?;
    screenshot_step(
        destination,
        "clipboard_image_read",
        read_started,
        true,
        Some(serde_json::json!({
            "width": image.width,
            "height": image.height,
        })),
    );
    let dir = screenshots_dir()?;
    let path = next_screenshot_path(&dir)?;
    let write_started = Instant::now();
    write_rgba_png(&path, &image).inspect_err(|err| {
        screenshot_step(
            destination,
            "clipboard_png_saved",
            write_started,
            false,
            Some(serde_json::json!({ "message": err.message.clone() })),
        );
    })?;
    screenshot_step(
        destination,
        "clipboard_png_saved",
        write_started,
        true,
        Some(serde_json::json!({ "path": path.display().to_string() })),
    );
    tracing::info!(
        destination = "disk",
        path = %path.display(),
        elapsed_ms = step_started.elapsed().as_millis(),
        "Windows screenshot saved from Ctrl+PrintScreen clipboard image"
    );
    Ok(path)
}

#[cfg(target_os = "windows")]
fn clipboard_sequence_number() -> u32 {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
    unsafe { GetClipboardSequenceNumber() }
}

#[cfg(target_os = "windows")]
fn wait_for_clipboard_image_after(before: u32, timeout: Duration) -> Result<(), DispatchError> {
    let deadline = Instant::now() + timeout;
    loop {
        let changed = clipboard_sequence_number() != before;
        if changed && clipboard_has_image() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(DispatchError::new(
                "E_SCREENSHOT_CLIPBOARD",
                "Ctrl+PrintScreen did not place a full-screen image on the clipboard",
            ));
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(target_os = "windows")]
fn clipboard_has_image() -> bool {
    use windows_sys::Win32::System::{
        DataExchange::IsClipboardFormatAvailable,
        Ole::{CF_BITMAP, CF_DIB, CF_DIBV5},
    };
    unsafe {
        IsClipboardFormatAvailable(CF_DIB as u32) != 0
            || IsClipboardFormatAvailable(CF_DIBV5 as u32) != 0
            || IsClipboardFormatAvailable(CF_BITMAP as u32) != 0
    }
}

#[cfg(target_os = "windows")]
struct ClipboardRgba {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[cfg(target_os = "windows")]
fn read_clipboard_image_as_rgba() -> Result<ClipboardRgba, DispatchError> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5};

    unsafe {
        let format = if IsClipboardFormatAvailable(CF_DIBV5 as u32) != 0 {
            CF_DIBV5 as u32
        } else if IsClipboardFormatAvailable(CF_DIB as u32) != 0 {
            CF_DIB as u32
        } else {
            return Err(DispatchError::new(
                "E_SCREENSHOT_CLIPBOARD",
                "clipboard does not contain a DIB screenshot image",
            ));
        };
        if OpenClipboard(std::ptr::null_mut::<std::ffi::c_void>() as HWND) == 0 {
            return Err(DispatchError::new(
                "E_SCREENSHOT_CLIPBOARD",
                "could not open clipboard to read screenshot image",
            ));
        }
        let handle = GetClipboardData(format);
        if handle.is_null() {
            CloseClipboard();
            return Err(DispatchError::new(
                "E_SCREENSHOT_CLIPBOARD",
                "clipboard image handle was empty",
            ));
        }
        let len = GlobalSize(handle) as usize;
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() || len < 40 {
            if !ptr.is_null() {
                GlobalUnlock(handle);
            }
            CloseClipboard();
            return Err(DispatchError::new(
                "E_SCREENSHOT_CLIPBOARD",
                "clipboard image data was unavailable",
            ));
        }
        let bytes = std::slice::from_raw_parts(ptr, len);
        let decoded = decode_dib_rgba(bytes);
        GlobalUnlock(handle);
        CloseClipboard();
        decoded
    }
}

#[cfg(target_os = "windows")]
fn decode_dib_rgba(bytes: &[u8]) -> Result<ClipboardRgba, DispatchError> {
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;

    fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ))
    }
    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }
    fn i32_at(bytes: &[u8], offset: usize) -> Option<i32> {
        Some(i32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    let header_size = u32_at(bytes, 0)
        .ok_or_else(|| DispatchError::new("E_SCREENSHOT_CLIPBOARD", "DIB header is truncated"))?
        as usize;
    if header_size < 40 || bytes.len() < header_size {
        return Err(DispatchError::new(
            "E_SCREENSHOT_CLIPBOARD",
            "unsupported or truncated DIB header",
        ));
    }
    let width_i = i32_at(bytes, 4)
        .ok_or_else(|| DispatchError::new("E_SCREENSHOT_CLIPBOARD", "DIB width is missing"))?;
    let height_i = i32_at(bytes, 8)
        .ok_or_else(|| DispatchError::new("E_SCREENSHOT_CLIPBOARD", "DIB height is missing"))?;
    let planes = u16_at(bytes, 12).unwrap_or(0);
    let bit_count = u16_at(bytes, 14).unwrap_or(0);
    let compression = u32_at(bytes, 16).unwrap_or(BI_RGB);
    let colors_used = u32_at(bytes, 32).unwrap_or(0);
    if planes != 1 || width_i <= 0 || height_i == 0 {
        return Err(DispatchError::new(
            "E_SCREENSHOT_CLIPBOARD",
            "invalid DIB screenshot geometry",
        ));
    }
    if !matches!(
        (bit_count, compression),
        (32, BI_RGB) | (32, BI_BITFIELDS) | (24, BI_RGB)
    ) {
        return Err(DispatchError::new(
            "E_SCREENSHOT_CLIPBOARD",
            format!("unsupported DIB format: {bit_count}bpp compression {compression}"),
        ));
    }

    let width = width_i as usize;
    let height = height_i.unsigned_abs() as usize;
    let color_table_entries = if bit_count <= 8 {
        if colors_used == 0 {
            1usize << bit_count
        } else {
            colors_used as usize
        }
    } else {
        0
    };
    let bitfield_bytes = if header_size == 40 && compression == BI_BITFIELDS {
        12
    } else {
        0
    };
    let pixel_offset = header_size + bitfield_bytes + color_table_entries * 4;
    let stride = (width * bit_count as usize).div_ceil(32) * 4;
    let required = pixel_offset.saturating_add(stride.saturating_mul(height));
    if bytes.len() < required {
        return Err(DispatchError::new(
            "E_SCREENSHOT_CLIPBOARD",
            "DIB pixel buffer is truncated",
        ));
    }

    let mut rgba = vec![0u8; width * height * 4];
    let top_down = height_i < 0;
    for y in 0..height {
        let source_y = if top_down { y } else { height - 1 - y };
        let source_row = pixel_offset + source_y * stride;
        let dest_row = y * width * 4;
        for x in 0..width {
            let dest = dest_row + x * 4;
            if bit_count == 32 {
                let src = source_row + x * 4;
                rgba[dest] = bytes[src + 2];
                rgba[dest + 1] = bytes[src + 1];
                rgba[dest + 2] = bytes[src];
                rgba[dest + 3] = 255;
            } else {
                let src = source_row + x * 3;
                rgba[dest] = bytes[src + 2];
                rgba[dest + 1] = bytes[src + 1];
                rgba[dest + 2] = bytes[src];
                rgba[dest + 3] = 255;
            }
        }
    }

    Ok(ClipboardRgba {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

#[cfg(target_os = "windows")]
fn next_screenshot_path(dir: &Path) -> Result<PathBuf, DispatchError> {
    let mut next = 1u32;
    let entries = std::fs::read_dir(dir).map_err(|e| {
        DispatchError::new(
            "E_SCREENSHOT_DIR",
            format!("read screenshot directory {}: {e}", dir.display()),
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_png_path(&path) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == SCREENSHOT_PNG_NAME_PREFIX {
            next = next.max(2);
            continue;
        }
        if let Some(number) = name
            .strip_prefix("Screenshot (")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|raw| raw.parse::<u32>().ok())
        {
            next = next.max(number.saturating_add(1));
        }
    }
    Ok(dir.join(format!("{SCREENSHOT_PNG_NAME_PREFIX} ({next}).png")))
}

#[cfg(target_os = "windows")]
fn write_rgba_png(path: &Path, image: &ClipboardRgba) -> Result<(), DispatchError> {
    image::save_buffer_with_format(
        path,
        &image.rgba,
        image.width,
        image.height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|e| {
        DispatchError::new(
            "E_SCREENSHOT_FILE",
            format!("write screenshot PNG {}: {e}", path.display()),
        )
    })
}

#[cfg(target_os = "windows")]
fn is_png_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("png"))
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn screenshots_dir() -> Result<PathBuf, DispatchError> {
    let dir = pictures_dir()
        .map(|pictures| pictures.join("Screenshots"))
        .unwrap_or_else(|| {
            crate::user_dirs::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Profile)
                .map(|home| home.join("Pictures").join("Screenshots"))
                .unwrap_or_else(|| crate::settings::app_data_root().join("Screenshots"))
        });
    std::fs::create_dir_all(&dir).map_err(|e| {
        DispatchError::new(
            "E_SCREENSHOT_DIR",
            format!("create screenshot directory {}: {e}", dir.display()),
        )
    })?;
    Ok(dir)
}

#[cfg(target_os = "windows")]
fn pictures_dir() -> Option<PathBuf> {
    crate::user_dirs::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Pictures)
}

#[cfg(target_os = "windows")]
struct PillCaptureExclusion {
    windows: Vec<PillWindowRestore>,
    destination: String,
    restored: bool,
}

#[cfg(target_os = "windows")]
struct PillWindowRestore {
    hwnd: windows_sys::Win32::Foundation::HWND,
}

#[cfg(target_os = "windows")]
impl PillCaptureExclusion {
    fn exclude(destination: &str) -> Self {
        use windows_sys::Win32::Foundation::RECT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowRect, ShowWindow, SW_HIDE,
        };

        unsafe extern "system" fn enum_window(
            hwnd: windows_sys::Win32::Foundation::HWND,
            lparam: windows_sys::Win32::Foundation::LPARAM,
        ) -> i32 {
            let handles =
                unsafe { &mut *(lparam as *mut Vec<windows_sys::Win32::Foundation::HWND>) };
            if is_rightkit_dictation_pill_window(hwnd) {
                handles.push(hwnd);
            }
            1
        }

        let mut handles = Vec::new();
        let mut windows = Vec::new();
        unsafe {
            EnumWindows(Some(enum_window), &mut handles as *mut _ as isize);
            for hwnd in &handles {
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if GetWindowRect(*hwnd, &mut rect) == 0 {
                    continue;
                }
                let hidden = ShowWindow(*hwnd, SW_HIDE) != 0;
                if hidden {
                    windows.push(PillWindowRestore { hwnd: *hwnd });
                }
            }
        }
        if !windows.is_empty() {
            std::thread::sleep(Duration::from_millis(90));
        }
        Self {
            windows,
            destination: destination.to_string(),
            restored: false,
        }
    }

    fn len(&self) -> usize {
        self.windows.len()
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let restore_started = Instant::now();
        use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
        let mut restored_count = 0usize;
        unsafe {
            for window in &self.windows {
                ShowWindow(window.hwnd, SW_SHOWNOACTIVATE);
                if windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(window.hwnd) != 0 {
                    restored_count += 1;
                }
            }
        }
        screenshot_step(
            &self.destination,
            "pill_restore_done",
            restore_started,
            restored_count == self.windows.len(),
            Some(serde_json::json!({
                "restored_count": restored_count,
                "excluded_count": self.windows.len(),
                "method": "hide",
            })),
        );
    }
}

#[cfg(target_os = "windows")]
impl Drop for PillCaptureExclusion {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(target_os = "windows")]
fn is_rightkit_dictation_pill_window(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
    };

    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return false;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 || len > 128 {
            return false;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if copied <= 0 {
            return false;
        }
        let title = String::from_utf16_lossy(&buf[..copied as usize]);
        title.eq_ignore_ascii_case("pill")
            || crate::host::identity()
                .overlay_window_titles
                .iter()
                .any(|candidate| title.eq_ignore_ascii_case(candidate))
    }
}
