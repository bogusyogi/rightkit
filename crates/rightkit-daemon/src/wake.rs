//! A cross-process wake signal (rightkit#404): `post` from any process of the current user, and
//! every [`WakeListener`] on the same name in any process wakes.
//!
//! The wake carries no data (it says "look again": a hub, a notch and an engine use it to tell each
//! other that shared state changed, and each then reads that state). Posts coalesce: any number of
//! posts before a listener looks is one wake. Nothing polls.
//!
//! * **Unix**: each listener binds a datagram socket in a 0700 directory made for the name; `post`
//!   sends one byte to every socket there and removes the sockets nobody listens on any more. The
//!   directory is under the per-user temporary directory (a socket path is limited to about 100
//!   bytes, which a suite root often exceeds), named from the suite root and the name, and must be
//!   owned by the current user and not group or world writable, or the call fails closed.
//! * **Windows**: each listener creates an auto-reset named event, with an owner-only DACL, and
//!   writes its name to a registration file under the suite's runtime directory; `post` opens every
//!   registered event and sets it, and removes the registrations that no longer open.
//!
//! A listener that is dropped removes its endpoint. A crashed listener leaves a stale endpoint that
//! the next `post` removes.

use crate::server::valid_service_name;
use rightkit_roots::Suite;
use std::io;
use std::time::Duration;

/// A name that is not a valid service name (the wake directory is derived from it).
fn invalid_name(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{name:?} is not a valid wake name"),
    )
}

fn hash_hex(text: &str) -> String {
    // FNV-1a, 64 bit: a short stable directory name, not a security property.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Wake every listener on `name` in `suite`. Returns how many listeners were signalled; zero when
/// nobody listens (not an error).
pub fn post(suite: &Suite, name: &str) -> io::Result<usize> {
    if !valid_service_name(name) {
        return Err(invalid_name(name));
    }
    imp::post(suite, name)
}

/// Listen for wakes on `name` in `suite`.
pub fn listen(suite: &Suite, name: &str) -> io::Result<WakeListener> {
    if !valid_service_name(name) {
        return Err(invalid_name(name));
    }
    imp::listen(suite, name).map(|inner| WakeListener { inner })
}

/// One listener. Dropping it removes its endpoint.
pub struct WakeListener {
    inner: imp::Inner,
}

impl WakeListener {
    /// Wait up to `timeout` for a wake. `true` when woken (every post since the last call counts
    /// once), `false` at the timeout.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        self.inner.wait(timeout)
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixDatagram;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn dir_for(suite: &Suite, name: &str) -> PathBuf {
        let key = format!("{}|{name}", suite.root().display());
        std::env::temp_dir().join(format!(
            "rkwake-{}-{}",
            crate::transport::current_uid(),
            hash_hex(&key)
        ))
    }

    /// The directory, created 0700 when missing, refused unless it is a real directory owned by
    /// the current user that nobody else can write.
    fn ensure_dir(dir: &Path) -> io::Result<()> {
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let md = std::fs::symlink_metadata(dir)?;
        if !md.is_dir()
            || md.uid() != crate::transport::current_uid()
            || md.permissions().mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the wake directory is not a private directory of this user",
            ));
        }
        Ok(())
    }

    pub(super) struct Inner {
        sock: UnixDatagram,
        path: PathBuf,
    }

    pub(super) fn listen(suite: &Suite, name: &str) -> io::Result<Inner> {
        let dir = dir_for(suite, name);
        ensure_dir(&dir)?;
        let path = dir.join(format!(
            "{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let sock = UnixDatagram::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Inner { sock, path })
    }

    impl Inner {
        pub(super) fn wait(&self, timeout: Duration) -> io::Result<bool> {
            self.sock
                .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
            let mut buf = [0u8; 16];
            match self.sock.recv(&mut buf) {
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(false)
                }
                Err(e) => return Err(e),
            }
            // Coalesce: anything already queued is the same wake.
            self.sock.set_nonblocking(true)?;
            while self.sock.recv(&mut buf).is_ok() {}
            self.sock.set_nonblocking(false)?;
            Ok(true)
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub(super) fn post(suite: &Suite, name: &str) -> io::Result<usize> {
        let dir = dir_for(suite, name);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        ensure_dir(&dir)?;
        let sender = UnixDatagram::unbound()?;
        sender.set_nonblocking(true)?;
        let mut woken = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sock") {
                continue;
            }
            match sender.send_to(&[1], &path) {
                Ok(_) => woken += 1,
                // A full queue is a pending wake already: the listener will look.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => woken += 1,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    let _ = std::fs::remove_file(&path);
                }
                Err(_) => {}
            }
        }
        Ok(woken)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    };

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn dir_for(suite: &Suite, name: &str) -> PathBuf {
        suite.service_dir(&format!("wake.{name}"))
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) struct Inner {
        event: HANDLE,
        registration: PathBuf,
    }

    // SAFETY: an event handle may be waited on from any thread.
    unsafe impl Send for Inner {}
    unsafe impl Sync for Inner {}

    pub(super) fn listen(suite: &Suite, name: &str) -> io::Result<Inner> {
        let dir = dir_for(suite, name);
        rightkit_roots::make_private_dir(&dir).map_err(io::Error::other)?;
        let event_name = format!(
            "Local\\rk-wake-{}-{}-{}",
            hash_hex(&format!("{}|{name}", suite.root().display())),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let attrs = crate::peer::owner_only_security_attributes()?;
        let wname = wide(&event_name);
        // Auto-reset: one wait consumes the signal; posts before it coalesce into one.
        // SAFETY: `attrs` and `wname` outlive the call; the DACL is copied into the kernel object.
        let event = unsafe {
            CreateEventW(
                Some(attrs.raw() as *const _),
                false,
                false,
                PCWSTR(wname.as_ptr()),
            )
        }
        .map_err(|e| io::Error::from_raw_os_error((e.code().0 as u32 & 0xFFFF) as i32))?;
        let registration = dir.join(format!(
            "{}-{}.ev",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        if let Err(e) = crate::record::write_atomic(&registration, event_name.as_bytes()) {
            // SAFETY: the handle was just created and is closed once.
            unsafe {
                let _ = CloseHandle(event);
            }
            return Err(e);
        }
        Ok(Inner {
            event,
            registration,
        })
    }

    impl Inner {
        pub(super) fn wait(&self, timeout: Duration) -> io::Result<bool> {
            let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: the handle is open for the life of `self`.
            let status = unsafe { WaitForSingleObject(self.event, ms) };
            Ok(status == WAIT_OBJECT_0)
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.registration);
            // SAFETY: closed once, here.
            unsafe {
                let _ = CloseHandle(self.event);
            }
        }
    }

    pub(super) fn post(suite: &Suite, name: &str) -> io::Result<usize> {
        let dir = dir_for(suite, name);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut woken = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ev") {
                continue;
            }
            let Ok(event_name) = std::fs::read_to_string(&path) else {
                continue;
            };
            let wname = wide(event_name.trim());
            // SAFETY: `wname` is NUL-terminated and outlives the call.
            match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(wname.as_ptr())) } {
                Ok(handle) => {
                    // SAFETY: the handle was just opened and is closed once.
                    unsafe {
                        if SetEvent(handle).is_ok() {
                            woken += 1;
                        }
                        let _ = CloseHandle(handle);
                    }
                }
                // The listener is gone: its registration is stale.
                Err(_) => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        Ok(woken)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub(super) struct Inner;

    pub(super) fn listen(_suite: &Suite, _name: &str) -> io::Result<Inner> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no cross-process wake on this platform",
        ))
    }

    impl Inner {
        pub(super) fn wait(&self, _timeout: Duration) -> io::Result<bool> {
            Ok(false)
        }
    }

    pub(super) fn post(_suite: &Suite, _name: &str) -> io::Result<usize> {
        Ok(0)
    }
}
