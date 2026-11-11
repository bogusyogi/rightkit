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

/// Current user's SID string (the pipe-name tag), from `rightkit_daemon::current_user_sid`.
#[cfg(windows)]
pub fn user_tag() -> io::Result<String> {
    rightkit_daemon::current_user_sid()
}

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
                    Ok(Accepted::Unidentified(refusal)) => {
                        tracing::warn!(
                            event = "service_peer_unidentified",
                            pid = ?refusal.pid,
                            image = ?refusal.image,
                            kind = ?refusal.kind,
                            reason = %refusal.reason,
                        );
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
