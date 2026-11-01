//! Local, current-user-only endpoint: rightkit-daemon's `Listener` (unix socket on
//! Unix, owner-only named pipe on Windows). It authenticates every peer as the
//! current user before a byte is read; this module runs the accept loop and, per
//! connection, `peer_trust::admit` (Orthic code signature) before `serve` reads anything.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rightkit_daemon::{Accepted, Listener, DEFAULT_MAX_PIPE_INSTANCES};

use super::conn::Ctx;
use super::peer_trust;

#[cfg(unix)]
pub fn user_tag() -> io::Result<String> {
    Ok("user".into())
}

#[cfg(windows)]
pub use win::user_tag;

/// Bind `endpoint` and serve accepted connections until `stop` is set. Returns the
/// listener so the owner can `wake()` the blocked accept when it stops.
pub fn listen(ctx: Arc<Ctx>, endpoint: &str, stop: Arc<AtomicBool>) -> io::Result<Arc<Listener>> {
    #[cfg(unix)]
    if let Some(dir) = std::path::Path::new(endpoint).parent() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let listener = Arc::new(Listener::bind(endpoint, DEFAULT_MAX_PIPE_INSTANCES)?);
    let accept = listener.clone();
    std::thread::Builder::new()
        .name("hr-speech-accept".into())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match accept.accept() {
                    Ok(Accepted::Conn(conn)) => {
                        if stop.load(Ordering::SeqCst) {
                            (conn.shutdown)();
                            return;
                        }
                        let ctx = ctx.clone();
                        let _ = std::thread::Builder::new()
                            .name("hr-speech-conn".into())
                            .spawn(move || {
                                // Orthic-only service: a peer that is not proven Orthic-signed is closed
                                // before one request byte is read. Runs here, not in the accept loop, so a
                                // slow signature query never stalls other connections.
                                if !peer_trust::admit(&conn.peer) {
                                    (conn.shutdown)();
                                    return;
                                }
                                super::conn::serve(ctx, conn.reader, conn.writer, conn.shutdown)
                            });
                    }
                    Ok(Accepted::Rejected(reason)) => {
                        tracing::warn!(event = "service_peer_rejected", %reason);
                    }
                    Ok(Accepted::Vanished) => {}
                    Err(error) => {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        tracing::warn!(event = "service_accept_failed", %error);
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                }
            }
        })?;
    Ok(listener)
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::io;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // GAP (rightkit-daemon): `current_user_sid_string` is pub(crate); the pipe-name tag still
    // needs the SID, so this one query stays until rightkit-daemon exports it.
    /// Current user's SID string (also the pipe-name tag).
    pub fn user_tag() -> io::Result<String> {
        // SAFETY: standard token query; buffers sized from the first call.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            if len == 0 {
                let e = io::Error::last_os_error();
                CloseHandle(token);
                return Err(e);
            }
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let ok = GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut c_void,
                len,
                &mut len,
            );
            CloseHandle(token);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut sid_str: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid_str) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut n = 0usize;
            while *sid_str.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, n));
            LocalFree(sid_str as *mut c_void);
            Ok(s)
        }
    }
}
