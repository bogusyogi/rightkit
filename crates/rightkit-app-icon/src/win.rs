//! Windows extraction: shell icon through GDI, running processes, `App Paths`, Start-menu shortcuts, packaged (MSIX / AppX) logos.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;
use std::time::Instant;

use crate::limits::{self, Budget, Sink};

use windows_sys::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows_sys::Win32::System::ProcessStatus::EnumProcesses;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ,
};
use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, GetSystemMetrics, HICON, ICONINFO, SM_CXICON, SM_CYICON,
};

pub(crate) fn extract(
    config: &crate::IconConfig,
    process_name: &str,
    exe_path: Option<&str>,
) -> Option<Vec<u8>> {
    let max_edge = config.edge_limit();
    let sink = config.event_sink;
    if let Some(path) = exe_path {
        return from_exe_path(path, max_edge, sink);
    }
    // (a) running process.
    if let Some(path) = resolve_running_image_path(process_name, config.process_path_for_pid, sink)
    {
        if let Some(png) = from_exe_path(&path, max_edge, sink) {
            return Some(png);
        }
    }
    // (b) App Paths registry.
    if let Some(path) = resolve_app_paths(process_name) {
        if let Some(png) = from_exe_path(&path, max_edge, sink) {
            return Some(png);
        }
    }
    // (c) Start-menu shortcuts.
    if let Some(path) = with_com(|| find_shortcut_target(process_name, sink)) {
        if let Some(png) = from_exe_path(&path, max_edge, sink) {
            return Some(png);
        }
    }
    // (d) packaged (MSIX / AppX) apps.
    resolve_packaged_logo(process_name, max_edge, sink)
}

/// Icon for a concrete executable. Packaged apps (under `WindowsApps`) prefer
/// the manifest logo: shell extraction is generic or fails there.
fn from_exe_path(path: &str, max_edge: u32, sink: Sink) -> Option<Vec<u8>> {
    // The path may come from the host (a process lookup) or a registry value: bound it before it
    // reaches a native call.
    if !limits::input_str_ok(path) {
        limits::report(sink, "exe_path", limits::Reject::Oversize);
        return None;
    }
    let packaged = path.to_lowercase().contains("\\windowsapps\\");
    if packaged {
        if let Some(png) = logo_for_packaged_exe(path, max_edge, sink) {
            return Some(png);
        }
    }
    // Existence, and a size bound on a file the shell loader will map (a directory is a folder
    // icon). An unreadable or over-size source is "no icon".
    if !limits::source_acceptable(std::path::Path::new(path), limits::MAX_ICON_SOURCE_BYTES) {
        return None;
    }
    with_com(|| icon_png_for_path(path, max_edge, sink))
}

fn logo_for_packaged_exe(path: &str, max_edge: u32, sink: Sink) -> Option<Vec<u8>> {
    let exe = crate::manifest::file_name_of(path);
    let root = std::path::Path::new(path)
        .ancestors()
        .skip(1)
        .take(6)
        .find(|dir| dir.join("AppxManifest.xml").is_file())?;
    let mut budget = Budget::start(&limits::LOGO_PROBE);
    crate::manifest::packaged_logo_png(root, exe, true, max_edge, sink, &mut budget)
}

// -- registry helpers ----------------------------------------------------

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

unsafe fn open_key(root: HKEY, subkey: &str) -> Option<HKEY> {
    let mut key: HKEY = null_mut();
    let w = wide(subkey);
    (RegOpenKeyExW(root, w.as_ptr(), 0, KEY_READ, &mut key) == 0).then_some(key)
}

/// Read a REG_SZ / REG_EXPAND_SZ value (`""` = default value). A value of another type, or one
/// larger than `limits::MAX_REG_VALUE_BYTES`, is `None`: the size is checked before the buffer
/// is allocated.
unsafe fn read_string(key: HKEY, value: &str) -> Option<String> {
    let w = wide(value);
    let mut kind = 0u32;
    let mut size = 0u32;
    if RegQueryValueExW(
        key,
        w.as_ptr(),
        null_mut(),
        &mut kind,
        null_mut(),
        &mut size,
    ) != 0
        || !limits::registry_value_ok(kind, size)
    {
        return None;
    }
    let mut buf = vec![0u16; size as usize / 2 + 1];
    let mut size2 = (buf.len() * 2) as u32;
    if RegQueryValueExW(
        key,
        w.as_ptr(),
        null_mut(),
        &mut kind,
        buf.as_mut_ptr() as *mut u8,
        &mut size2,
    ) != 0
    {
        return None;
    }
    let len = (size2 as usize / 2).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..len]);
    Some(text.trim_end_matches('\0').to_string())
}

fn resolve_app_paths(process_name: &str) -> Option<String> {
    let subkey = crate::manifest::app_paths_subkey(process_name)?;
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        unsafe {
            let Some(key) = open_key(root, &subkey) else {
                continue;
            };
            let value = read_string(key, "");
            RegCloseKey(key);
            if let Some(value) = value {
                let path = value.trim().trim_matches('"').to_string();
                if !path.is_empty() && std::path::Path::new(&path).exists() {
                    return Some(path);
                }
            }
        }
    }
    None
}

// -- Start-menu shortcuts --------------------------------------------------

/// Per-user and all-users Start-menu Programs folders, asked of the shell
/// (environment variables like APPDATA can be absent or redirected).
fn start_menu_roots() -> Vec<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_CommonPrograms, FOLDERID_Programs, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG,
    };
    let mut roots = Vec::new();
    for id in [&FOLDERID_Programs, &FOLDERID_CommonPrograms] {
        if let Ok(p) = unsafe { SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None) } {
            if let Ok(s) = unsafe { p.to_string() } {
                roots.push(std::path::PathBuf::from(s));
            }
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
        }
    }
    roots
}

/// Walk the Start-menu folders within `limits::START_MENU` (entries of every kind, depth and
/// time; reparse points are neither followed nor visited) and load at most
/// `limits::MAX_SHORTCUTS` shortcuts of at most `limits::MAX_LNK_BYTES` each.
///
/// Each shortcut is read by this crate, not by the shell: `Roots::open_confined` under the Start-menu
/// folders (no link followed at open time), at most `MAX_LNK_BYTES` bytes, and the bytes are handed
/// to the shell link object through an in-memory stream (`IPersistStream::Load`). So the byte bound
/// holds even if the file grows or is swapped after the directory walk saw it. Hard links are
/// allowed here (`limits::shortcut_read_options`): the folders are user-writable, so a hostile
/// shortcut could be planted directly and a hard link gives nothing more.
fn find_shortcut_target(wanted: &str, sink: Sink) -> Option<String> {
    use windows::core::Interface;
    use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
    use windows::Win32::System::Com::{CoCreateInstance, IPersistStream, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{IShellLinkW, SHCreateMemStream, ShellLink};

    let roots = start_menu_roots();
    // The folders approved for reads. A folder that cannot be resolved is left out.
    let mut confined = rightkit_fs::Roots::new();
    for root in &roots {
        let _ = confined.add(root);
    }
    let read_options = limits::shortcut_read_options();
    let link: IShellLinkW =
        unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()? };
    let persist: IPersistStream = link.cast().ok()?;
    let mut budget = Budget::start(&limits::START_MENU);
    let mut seen = 0usize;
    let mut found: Option<String> = None;
    let walked = limits::walk_files(roots, &mut budget, |path, meta| {
        let is_lnk = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
        if !is_lnk || meta.len() > limits::MAX_LNK_BYTES {
            return false;
        }
        seen += 1;
        if seen > limits::MAX_SHORTCUTS {
            return true;
        }
        let Ok(bytes) =
            limits::read_confined(&confined, path, &read_options, limits::MAX_LNK_BYTES)
        else {
            return false;
        };
        let Some(stream) = (unsafe { SHCreateMemStream(Some(&bytes)) }) else {
            return false;
        };
        if unsafe { persist.Load(&stream) }.is_err() {
            return false;
        }
        let mut buf = [0u16; 1024];
        let got = unsafe { link.GetPath(&mut buf, std::ptr::null_mut::<WIN32_FIND_DATAW>(), 0) };
        if got.is_err() {
            return false;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let target = String::from_utf16_lossy(&buf[..len]);
        if !target.is_empty()
            && crate::manifest::target_matches_name(&target, wanted)
            && std::path::Path::new(&target).exists()
        {
            found = Some(target);
            return true;
        }
        false
    });
    if let Err(stop) = walked {
        limits::report(sink, "start_menu", stop.reject());
    }
    found
}

// -- packaged (MSIX / AppX) ------------------------------------------------

const PACKAGE_REPO: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";

/// Install roots of the current user's packages (registry repository),
/// package-name hits for `hint` first.
///
/// One registry level only (depth 1), at most `budget` keys; a `PackageRootFolder` that is not a
/// local drive-letter path without `..` is dropped (the key is user-writable). When the budget
/// runs out the roots found so far are used.
fn package_roots(hint: &str, budget: &mut Budget, sink: Sink) -> Vec<std::path::PathBuf> {
    let mut named: Vec<(String, String)> = Vec::new();
    unsafe {
        let Some(repo) = open_key(HKEY_CURRENT_USER, PACKAGE_REPO) else {
            return Vec::new();
        };
        let mut index = 0u32;
        loop {
            if let Err(stop) = budget.charge(Instant::now()) {
                limits::report(sink, "registry", stop.reject());
                break;
            }
            let mut name = [0u16; 512];
            let mut len = name.len() as u32;
            let rc = RegEnumKeyExW(
                repo,
                index,
                name.as_mut_ptr(),
                &mut len,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            const ERROR_MORE_DATA: u32 = 234;
            index = index.saturating_add(1);
            if rc == ERROR_MORE_DATA {
                continue; // a name longer than the buffer: skip that key, keep enumerating
            }
            if rc != 0 {
                break;
            }
            let full = String::from_utf16_lossy(&name[..len as usize]);
            if let Some(sub) = open_key(repo, &full) {
                let root = read_string(sub, "PackageRootFolder");
                RegCloseKey(sub);
                if let Some(root) = root.filter(|r| limits::package_root_acceptable(r)) {
                    named.push((full, root));
                }
            }
        }
        RegCloseKey(repo);
    }
    let hint = hint.to_lowercase();
    named.sort_by_key(|(full, _)| !(full.to_lowercase().contains(&hint)));
    named
        .into_iter()
        .map(|(_, root)| std::path::PathBuf::from(root))
        .collect()
}

fn resolve_packaged_logo(process_name: &str, max_edge: u32, sink: Sink) -> Option<Vec<u8>> {
    let exe = crate::manifest::file_name_of(process_name);
    let hint = exe.to_lowercase();
    let hint = hint.strip_suffix(".exe").unwrap_or(&hint).to_string();
    if hint.is_empty() {
        return None;
    }
    let mut budget = Budget::start(&limits::PACKAGES);
    for root in package_roots(&hint, &mut budget, sink) {
        if budget.exhausted(Instant::now()) {
            break;
        }
        if let Some(png) =
            crate::manifest::packaged_logo_png(&root, exe, false, max_edge, sink, &mut budget)
        {
            return Some(png);
        }
    }
    None
}

/// The shell icon APIs expect an initialized COM apartment on the calling
/// thread. Blocking-pool threads have none, so create (and release) one.
fn with_com<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    unsafe {
        let did_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let out = f();
        if did_init {
            CoUninitialize();
        }
        out
    }
}

/// Find a running process whose image name matches `process_name` and
/// return its full executable path.
fn resolve_running_image_path(
    process_name: &str,
    process_path_for_pid: fn(u32) -> Option<String>,
    sink: Sink,
) -> Option<String> {
    if process_name.trim().is_empty() {
        return None;
    }
    let mut pids = vec![0u32; 4096];
    let mut needed = 0u32;
    let ok = unsafe {
        EnumProcesses(
            pids.as_mut_ptr(),
            (pids.len() * std::mem::size_of::<u32>()) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return None;
    }
    let count = (needed as usize / std::mem::size_of::<u32>()).min(pids.len());
    let mut budget = Budget::start(&limits::PROCESSES);
    for &pid in &pids[..count] {
        if let Err(stop) = budget.charge(Instant::now()) {
            limits::report(sink, "processes", stop.reject());
            return None;
        }
        // Host callback: its string is bounded before it is compared or used.
        let Some(path) = process_path_for_pid(pid).filter(|p| limits::input_str_ok(p)) else {
            continue;
        };
        let image = std::path::Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if crate::manifest::process_names_match(image, process_name) {
            return Some(path);
        }
    }
    None
}

/// Shell icon of `path`, PNG-encoded.
///
/// What each native call allocates and what bounds it:
/// - `GetSystemMetrics(SM_CXICON / SM_CYICON)`: no allocation. The size `SHGFI_LARGEICON` will
///   produce is this metric, so it is checked with `limits::check_dimensions` BEFORE the shell is
///   asked for any icon; a configuration whose large-icon size exceeds `max_edge` or the pixel cap
///   is "no icon".
/// - `SHGetFileInfoW(.., SHGFI_ICON | SHGFI_LARGEICON)`: the shell allocates the `HICON` (two bitmaps
///   of the metric size, plus its own work reading the file's resources, which this crate cannot
///   bound beyond refusing a source file over `MAX_ICON_SOURCE_BYTES`, done by the caller).
/// - `GetIconInfo`: the system hands back handles to bitmaps of that same size; no buffer of ours.
/// - `GetObjectW`: fills a `BITMAP` header only; the colour bitmap's real size is checked again
///   here and the mask must be the same size or it is ignored.
/// - `GetDIBits`: writes into a `Vec` this crate sized from the checked width and height
///   (`rgba_len`, at most `MAX_PIXELS * 4` bytes) and is told exactly that width and height.
fn icon_png_for_path(path: &str, max_edge: u32, sink: Sink) -> Option<Vec<u8>> {
    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let (metric_w, metric_h) = (GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
        if let Err(reject) =
            limits::check_dimensions(i64::from(metric_w), i64::from(metric_h), max_edge)
        {
            limits::report(sink, "shell_icon_size", reject);
            return None;
        }
        let mut info: SHFILEINFOW = std::mem::zeroed();
        let ok = SHGetFileInfoW(
            wide.as_ptr(),
            FILE_ATTRIBUTE_NORMAL,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok == 0 || info.hIcon.is_null() {
            return None;
        }
        let png = hicon_to_png(info.hIcon, max_edge, sink);
        DestroyIcon(info.hIcon);
        png
    }
}

/// Read a 32-bit top-down DIB of `bitmap` (`width` x `height`) via GDI.
unsafe fn read_bitmap_bgra(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    bitmap: HBITMAP,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    // Dimensions were checked by `limits::check_dimensions`; the length is still computed with
    // checked arithmetic and re-bounded before the buffer exists.
    let len = limits::rgba_len(width, height)?;
    if len as u64 > limits::MAX_PIXELS * 4 {
        return None;
    }
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize =
        std::mem::size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = i32::try_from(width).ok()?;
    bmi.bmiHeader.biHeight = -i32::try_from(height).ok()?; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    let mut pixels = vec![0u8; len];
    let lines = GetDIBits(
        dc,
        bitmap,
        0,
        height,
        pixels.as_mut_ptr() as *mut _,
        &mut bmi,
        DIB_RGB_COLORS,
    );
    (lines as u32 == height).then_some(pixels)
}

/// Width and height of a GDI bitmap from its `BITMAP` header (`GetObjectW` copies no pixels).
unsafe fn bitmap_size(bitmap: HBITMAP) -> Option<(i64, i64)> {
    let mut bm: BITMAP = std::mem::zeroed();
    if GetObjectW(
        bitmap,
        std::mem::size_of::<BITMAP>() as i32,
        &mut bm as *mut BITMAP as *mut _,
    ) == 0
    {
        return None;
    }
    Some((i64::from(bm.bmWidth), i64::from(bm.bmHeight)))
}

unsafe fn hicon_to_png(hicon: HICON, max_edge: u32, sink: Sink) -> Option<Vec<u8>> {
    let mut icon: ICONINFO = std::mem::zeroed();
    if GetIconInfo(hicon, &mut icon) == 0 {
        return None;
    }
    let result = (|| {
        if icon.hbmColor.is_null() {
            return None; // monochrome icon: not worth rendering
        }
        let (bm_width, bm_height) = bitmap_size(icon.hbmColor)?;
        // Bounded before any device context or pixel buffer exists.
        let (width, height) = match limits::check_dimensions(bm_width, bm_height, max_edge) {
            Ok(dims) => dims,
            Err(reject) => {
                limits::report(sink, "shell_icon", reject);
                return None;
            }
        };
        // The mask is read with the colour bitmap's dimensions, so it must have exactly them;
        // otherwise it is not used (the icon then keeps its own alpha or is opaque).
        let mask_bitmap = if icon.hbmMask.is_null()
            || bitmap_size(icon.hbmMask) != Some((i64::from(width), i64::from(height)))
        {
            None
        } else {
            Some(icon.hbmMask)
        };
        let dc = GetDC(null_mut());
        if dc.is_null() {
            return None;
        }
        let mut bgra = read_bitmap_bgra(dc, icon.hbmColor, width, height);
        // Legacy (non-alpha) icons carry transparency in the AND mask only.
        if let Some(pixels) = bgra.as_mut() {
            if pixels.as_chunks::<4>().0.iter().all(|px| px[3] == 0) {
                let mask = mask_bitmap.and_then(|m| read_bitmap_bgra(dc, m, width, height));
                for (index, px) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let transparent = mask.as_ref().map(|m| m[index * 4] != 0).unwrap_or(false);
                    px[3] = if transparent { 0 } else { 255 };
                }
            }
        }
        ReleaseDC(null_mut(), dc);
        let mut rgba = bgra?;
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2); // BGRA -> RGBA
        }
        crate::png::encode_png_rgba(width, height, &rgba)
    })();
    DeleteObject(icon.hbmColor);
    if !icon.hbmMask.is_null() {
        DeleteObject(icon.hbmMask);
    }
    result
}
