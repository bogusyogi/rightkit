//! Detect and forward to an instance started by `tauri-plugin-single-instance` 2.x, so an old-build
//! instance is not invisible during an upgrade.
//!
//! The names come from the plugin source (`platform_impl/macos.rs`, `windows.rs`, `linux.rs`, read at
//! 2.4.2 and 2.4.3; HeardRight's lockfile pins 2.4.5, whose source was not available to read). See
//! [`crate::single_instance::LegacySingleInstanceNames`] for the derivation as data:
//! * macOS listens on `/tmp/<identifier>_si.sock`, where `.` and `-` in the identifier become `_`.
//!   A `_<semver>` infix appears when the plugin's `semver` feature is on, so `/tmp` is scanned for
//!   `<stem>_*_si.sock` too. A launch is written as `cwd`, two NUL bytes, then the argv joined by NUL.
//! * Windows owns the named mutex `<identifier>-sim`, window class `<identifier>-sic` and window title
//!   `<identifier>-siw`. A launch is sent as `WM_COPYDATA` with `dwData == 1542` and the payload
//!   `cwd|arg|arg...` (NUL-terminated).
//! * Linux uses a D-Bus well-known name, which this crate does not probe. Linux therefore reports
//!   "no legacy holder"; the limitation is documented in the README.
//!
//! Probing never leaves a half-open connection behind: a stale socket answers "connection refused",
//! and that means no live holder.

use std::path::PathBuf;

/// Forward one launch to a live legacy holder. `Ok(Some(()))`: delivered. `Ok(None)`: no live
/// legacy holder. `Err`: a live holder exists but did not take the launch. The caller must treat
/// `Err` as fail-closed.
pub(crate) fn forward(identifier: &str, argv: &[String], cwd: &str) -> Result<Option<()>, String> {
    #[cfg(target_os = "macos")]
    {
        forward_socket(identifier, argv, cwd)
    }
    #[cfg(windows)]
    {
        forward_window(identifier, argv, cwd)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (identifier, argv, cwd);
        Ok(None)
    }
}

/// The socket name the plugin derives, without any version infix.
pub(crate) fn socket_stem(identifier: &str) -> String {
    identifier.replace(['.', '-'], "_")
}

/// Candidate socket paths for `identifier`: the exact name first, then `<stem>_*_si.sock` variants.
pub(crate) fn socket_candidates(identifier: &str) -> Vec<PathBuf> {
    let stem = socket_stem(identifier);
    let mut out = vec![PathBuf::from(format!("/tmp/{stem}_si.sock"))];
    if let Ok(entries) = std::fs::read_dir("/tmp") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&format!("{stem}_")) && name.ends_with("_si.sock") {
                let path = entry.path();
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// The macOS launch payload: `cwd`, `\0\0`, then the argv joined by `\0`.
pub(crate) fn socket_payload(argv: &[String], cwd: &str) -> Vec<u8> {
    let mut out = cwd.as_bytes().to_vec();
    out.extend_from_slice(b"\0\0");
    out.extend_from_slice(argv.join("\0").as_bytes());
    out
}

/// The Windows launch payload: `cwd|arg|arg`, NUL-terminated, as the plugin writes it.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn window_payload(argv: &[String], cwd: &str) -> Vec<u8> {
    let mut out = format!("{cwd}|{}", argv.join("|")).into_bytes();
    out.push(0);
    out
}

#[cfg(target_os = "macos")]
fn forward_socket(identifier: &str, argv: &[String], cwd: &str) -> Result<Option<()>, String> {
    use std::io::{ErrorKind, Write};
    use std::os::unix::net::UnixStream;

    for path in socket_candidates(identifier) {
        match UnixStream::connect(&path) {
            Ok(mut stream) => {
                // A hung holder must not hang this launch.
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
                stream
                    .write_all(&socket_payload(argv, cwd))
                    .and_then(|_| stream.flush())
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
                return Ok(Some(()));
            }
            // A socket file with nothing listening is a stale record from a crashed holder.
            Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
                continue
            }
            Err(e) => return Err(format!("connect {}: {e}", path.display())),
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn forward_window(identifier: &str, argv: &[String], cwd: &str) -> Result<Option<()>, String> {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WM_COPYDATA: u32 = 0x004A;
    const WMCOPYDATA_SINGLE_INSTANCE_DATA: usize = 1542;
    const SMTO_BLOCK: u32 = 0x1;
    const SMTO_ABORTIFHUNG: u32 = 0x2;
    const SEND_TIMEOUT_MS: u32 = 5000;

    #[repr(C)]
    struct CopyDataStruct {
        dw_data: usize,
        cb_data: u32,
        lp_data: *const u8,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenMutexW(desired_access: u32, inherit_handle: i32, name: *const u16) -> isize;
        fn CloseHandle(handle: isize) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class_name: *const u16, window_name: *const u16) -> isize;
        fn SendMessageTimeoutW(
            hwnd: isize,
            msg: u32,
            wparam: usize,
            lparam: isize,
            flags: u32,
            timeout_ms: u32,
            result: *mut usize,
        ) -> isize;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let names = crate::single_instance::LegacySingleInstanceNames::for_identifier(identifier);
    let mutex = wide(&names.windows_mutex);
    // SAFETY: `mutex` is a NUL-terminated UTF-16 string that outlives the call.
    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, mutex.as_ptr()) };
    if handle == 0 {
        return Ok(None);
    }
    // SAFETY: `handle` was returned by OpenMutexW and is closed exactly once.
    unsafe { CloseHandle(handle) };

    let class = wide(&names.windows_window_class);
    let title = wide(&names.windows_window_title);
    // SAFETY: both strings are NUL-terminated UTF-16 and outlive the call.
    let hwnd = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
    if hwnd == 0 {
        return Err(
            "the legacy holder owns the claim but has no window to receive the launch".into(),
        );
    }
    let payload = window_payload(argv, cwd);
    let data = CopyDataStruct {
        dw_data: WMCOPYDATA_SINGLE_INSTANCE_DATA,
        cb_data: payload.len() as u32,
        lp_data: payload.as_ptr(),
    };
    // SAFETY: `data` and `payload` stay alive for the synchronous call, and the receiver reads only
    // within `cb_data` bytes. A hung holder must not hang this launch: abort-if-hung and a 5 s cap.
    let mut result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            0,
            &data as *const CopyDataStruct as isize,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            SEND_TIMEOUT_MS,
            &mut result,
        )
    };
    if sent == 0 {
        return Err("the legacy holder did not answer the launch within 5 seconds".into());
    }
    Ok(Some(()))
}
