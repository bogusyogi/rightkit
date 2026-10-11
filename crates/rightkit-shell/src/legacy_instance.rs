//! Detect and forward to an instance started by `tauri-plugin-single-instance` 2.x, so an old-build
//! instance is not invisible during an upgrade.
//!
//! The names come from the plugin source (`platform_impl/macos.rs`, `windows.rs`, `linux.rs`, read at
//! 2.4.2 and 2.4.3; HeardRight's lockfile pins 2.4.5, whose source was not available to read). See
//! [`crate::single_instance::LegacySingleInstanceNames`] for the derivation as data:
//! * macOS listens on `/tmp/<identifier>_si.sock`, where `.` and `-` in the identifier become `_`.
//!   A `_<semver>` infix appears when the plugin's `semver` feature is on, so `/tmp` is scanned for
//!   `<stem>_*_si.sock` too. A launch is written as `cwd`, two NUL bytes, then the argv joined by NUL,
//!   and the client closes (`notify_singleton`, `macos.rs` lines 78-93 at 2.4.3). The holder accepts,
//!   reads the connection to end-of-stream, runs the app's callback and drops the connection
//!   (`listen_for_other_instances`, lines 95-132; 2.4.2 is the same loop on a std listener). It never
//!   writes back: its close, after the callback returns, is the only answer the protocol has.
//! * Windows owns the named mutex `<identifier>-sim`, window class `<identifier>-sic` and window title
//!   `<identifier>-siw`. A launch is sent as `WM_COPYDATA` with `dwData == 1542` and the payload
//!   `cwd|arg|arg...` (NUL-terminated).
//! * Linux uses a D-Bus well-known name, which this crate does not probe. Linux therefore reports
//!   "no legacy holder"; the limitation is documented in the README.
//!
//! # A listener is not a holder
//!
//! A refused or missing socket is a crashed holder's leftover and means no live holder. But a
//! connect that succeeds proves only that some process still has the listening descriptor open. A
//! holder can leak that descriptor into a child it spawned (a sidecar); when the app exits the child
//! keeps the socket accepting into a backlog nobody reads, and a launch that wrote to it and exited
//! would be lost: the user double-clicks the app and nothing opens.
//!
//! So on macOS a connected socket is judged before the launch is believed delivered, from two
//! independent facts (the decision table is [`liveness::judge`]):
//!
//! * **The owner.** `getsockopt(SOL_LOCAL, LOCAL_PEERPID)` on the connected socket names the pid the
//!   kernel keeps for the listening end. If that pid is no longer running, nothing that owned the
//!   socket is left: the socket is stale and nothing is written to it. If it runs a different
//!   program than this process (not the same executable, not the same `.app` bundle), it is not the
//!   app.
//! * **The answer.** After the whole launch is written the write side is shut (the plugin reads to
//!   end-of-stream) and the holder gets [`CLOSE_WAIT_SECS`] to close its end, waited on with one
//!   kernel receive timeout, not a sleep loop. A close is the plugin's acknowledgement.
//!
//! A close confirms the launch ([`Delivery::Confirmed`]) whoever owns the socket. Silence from a
//! socket whose owner is a different program is stale. Silence from this app's own process, or from
//! an owner that could not be identified, cannot be told from a slow callback, so it keeps the
//! pre-existing behaviour and is reported as [`Delivery::Unconfirmed`], which callers log loudly. A
//! stale verdict never removes anything: only the caller that owns the path may unlink it.

use std::path::PathBuf;

/// Seconds a macOS holder gets to close the connection after the whole launch was written.
///
/// The plugin closes when its callback returns, which for a callback that shows a window is
/// milliseconds; the wait is a bound on a silent socket, not a pause on the normal path.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const CLOSE_WAIT_SECS: u64 = 2;

/// How a launch that reached a legacy holder was received.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) enum Delivery {
    /// The holder acknowledged the launch. Windows: `SendMessageTimeoutW` returned, which happens
    /// only after the holder's window procedure did. macOS: the holder read the launch to its end
    /// and closed the connection.
    Confirmed,
    /// The launch was handed to what is believed to be the live holder, but nothing showed that it
    /// took it (macOS only, see the module documentation). The caller proceeds as for
    /// [`Delivery::Confirmed`] and must log the reason.
    Unconfirmed(String),
}

impl Delivery {
    /// Log one loud line when the launch was not confirmed. `claimant` names the calling path.
    pub(crate) fn log_unconfirmed(&self, claimant: &str) {
        if let Self::Unconfirmed(why) = self {
            eprintln!(
                "rightkit-shell: {claimant}: a legacy single-instance launch was forwarded but NOT \
                 confirmed ({why}); if the app did not open, quit the older copy and start it again"
            );
        }
    }
}

/// Forward one launch to a live legacy holder. `Ok(Some(()))`: delivered (an unconfirmed delivery
/// is logged here; callers that need to see it use [`forward_checked`]). `Ok(None)`: no live legacy
/// holder. `Err`: a live holder exists but did not take the launch. The caller must treat `Err` as
/// fail-closed.
// The macOS and Windows hold paths in `legacy_hold` are the callers.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) fn forward(identifier: &str, argv: &[String], cwd: &str) -> Result<Option<()>, String> {
    Ok(forward_checked(identifier, argv, cwd)?.map(|delivery| {
        delivery.log_unconfirmed("legacy hold");
    }))
}

/// [`forward`], keeping what is known about how the holder received the launch.
pub(crate) fn forward_checked(
    identifier: &str,
    argv: &[String],
    cwd: &str,
) -> Result<Option<Delivery>, String> {
    #[cfg(target_os = "macos")]
    {
        forward_socket(identifier, argv, cwd)
    }
    #[cfg(windows)]
    {
        forward_window(identifier, argv, cwd).map(|sent| sent.map(|()| Delivery::Confirmed))
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
// Used by the macOS socket path only.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
// Used by the macOS socket path only.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

/// The judgement of a connected legacy socket. Pure decisions, so the table is the same (and
/// tested) on every host; only macOS feeds it.
mod liveness {
    #![cfg_attr(not(target_os = "macos"), allow(dead_code))]

    use std::path::{Path, PathBuf};

    use super::{Delivery, CLOSE_WAIT_SECS};

    /// Who the kernel says owns the listening end of the socket this process connected to.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Owner {
        /// This pid no longer names a running process, yet the socket still accepts connections:
        /// another process (a descendant that inherited the descriptor) keeps it open.
        Gone(u32),
        /// Running, and the same executable or `.app` bundle as this process.
        App(u32),
        /// Running, and a different program.
        Other(u32),
        /// Not determined (the kernel would not say, or the process cannot be described).
        Unknown,
    }

    /// What the holder did with the connection once the whole launch was written and shut.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Closure {
        /// End-of-stream, or any reply: the holder is done with the connection.
        Answered,
        /// Nothing within the wait.
        Silent,
        /// The connection was torn down with an error (reset, broken pipe).
        Torn,
    }

    /// What to do with a connected socket.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Judgement {
        /// The launch is delivered, confirmed or not.
        Delivered(Delivery),
        /// No live holder: look at the next candidate, and finally report "none". The reason is for
        /// the log.
        Stale(String),
        /// A holder that may be live did not take the launch. The caller must fail closed.
        Refused(String),
    }

    /// The decision table. `owner` is read before anything is written, `closure` after.
    ///
    /// | owner \ holder | closed | silent | torn down |
    /// |---|---|---|---|
    /// | gone | stale | stale | stale |
    /// | other program | delivered | stale | stale |
    /// | this app, or unknown | delivered | delivered, unconfirmed | refused |
    pub(super) fn judge(owner: Owner, closure: Closure) -> Judgement {
        match (owner, closure) {
            (Owner::Gone(pid), _) => Judgement::Stale(format!(
                "its owner (pid {pid}) has exited and another process keeps the listener open"
            )),
            (_, Closure::Answered) => Judgement::Delivered(Delivery::Confirmed),
            (Owner::Other(pid), Closure::Silent | Closure::Torn) => Judgement::Stale(format!(
                "its owner (pid {pid}) is not this app and did not take the launch"
            )),
            (Owner::App(_) | Owner::Unknown, Closure::Torn) => Judgement::Refused(
                "the holder dropped the connection after the launch was written".to_owned(),
            ),
            (Owner::App(pid), Closure::Silent) => {
                Judgement::Delivered(Delivery::Unconfirmed(format!(
                    "this app's own process (pid {pid}) did not close the connection within \
                     {CLOSE_WAIT_SECS} s"
                )))
            }
            (Owner::Unknown, Closure::Silent) => {
                Judgement::Delivered(Delivery::Unconfirmed(format!(
                    "the socket's owner could not be identified and it did not close the \
                     connection within {CLOSE_WAIT_SECS} s"
                )))
            }
        }
    }

    /// Whether `peer_exe` is this program: the same executable file, or an executable inside the
    /// same `.app` bundle (an update may rename the executable).
    pub(super) fn is_this_app(peer_exe: &Path) -> bool {
        std::env::current_exe().is_ok_and(|mine| same_program(peer_exe, &mine))
    }

    /// [`is_this_app`] for two known paths. Paths that cannot be resolved (a replaced binary) are
    /// compared as written.
    pub(super) fn same_program(a: &Path, b: &Path) -> bool {
        let a = canonical(a);
        let b = canonical(b);
        a == b || matches!((bundle_of(&a), bundle_of(&b)), (Some(x), Some(y)) if x == y)
    }

    fn canonical(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    /// The innermost `.app` directory containing `path`.
    fn bundle_of(path: &Path) -> Option<&Path> {
        path.ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
    }
}

#[cfg(target_os = "macos")]
use self::liveness::{judge, Closure, Judgement, Owner};

/// What the kernel can tell this process about the owner of a connected socket. The declarations
/// match `<sys/un.h>` (`SOL_LOCAL` 0, `LOCAL_PEERPID` 0x002), `<libproc.h>` (`proc_pidpath`),
/// `<sys/proc_info.h>` (`PROC_PIDPATHINFO_MAXSIZE`, 4 * MAXPATHLEN) and `<sys/errno.h>`.
#[cfg(target_os = "macos")]
mod peer {
    use std::ffi::{c_int, c_void, OsString};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    use super::liveness::{is_this_app, Owner};

    extern "C" {
        fn getsockopt(
            socket: c_int,
            level: c_int,
            name: c_int,
            value: *mut c_void,
            len: *mut u32,
        ) -> c_int;
        fn kill(pid: c_int, signal: c_int) -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    }

    const SOL_LOCAL: c_int = 0;
    const LOCAL_PEERPID: c_int = 0x002;
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;
    const EPERM: i32 = 1;
    const ESRCH: i32 = 3;

    enum Executable {
        /// No such process.
        Gone,
        /// A process that exists but whose path the kernel would not give.
        Unreadable,
        Path(PathBuf),
    }

    /// The owner of the listening end behind `stream`, a connected client socket.
    pub(super) fn owner(stream: &UnixStream) -> Owner {
        let Some(pid) = peer_pid(stream) else {
            return Owner::Unknown;
        };
        match executable(pid) {
            Executable::Gone => Owner::Gone(pid),
            Executable::Unreadable => Owner::Unknown,
            Executable::Path(path) if is_this_app(&path) => Owner::App(pid),
            Executable::Path(_) => Owner::Other(pid),
        }
    }

    fn peer_pid(stream: &UnixStream) -> Option<u32> {
        let mut pid: c_int = 0;
        let mut len = std::mem::size_of::<c_int>() as u32;
        // SAFETY: `pid` is a writable c_int and `len` holds its size; the descriptor is open for
        // the duration of the call.
        let rc = unsafe {
            getsockopt(
                stream.as_raw_fd(),
                SOL_LOCAL,
                LOCAL_PEERPID,
                (&mut pid as *mut c_int).cast(),
                &mut len,
            )
        };
        (rc == 0 && pid > 0).then_some(pid as u32)
    }

    fn executable(pid: u32) -> Executable {
        let mut buffer = vec![0_u8; PROC_PIDPATHINFO_MAXSIZE];
        // SAFETY: `buffer` is writable for the `size` bytes passed.
        let written = unsafe {
            proc_pidpath(
                pid as c_int,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
            )
        };
        if written > 0 {
            buffer.truncate(written as usize);
            if let Some(nul) = buffer.iter().position(|b| *b == 0) {
                buffer.truncate(nul);
            }
            return Executable::Path(PathBuf::from(OsString::from_vec(buffer)));
        }
        let errno = std::io::Error::last_os_error().raw_os_error();
        if errno == Some(ESRCH) || !is_running(pid) {
            Executable::Gone
        } else {
            Executable::Unreadable
        }
    }

    fn is_running(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists and could be signalled.
        if unsafe { kill(pid as c_int, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(EPERM)
    }
}

/// How long a write to a holder may block. A hung holder must not hang this launch.
#[cfg(target_os = "macos")]
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(target_os = "macos")]
fn forward_socket(
    identifier: &str,
    argv: &[String],
    cwd: &str,
) -> Result<Option<Delivery>, String> {
    use std::io::ErrorKind;
    use std::os::unix::net::UnixStream;

    let payload = socket_payload(argv, cwd);
    for path in socket_candidates(identifier) {
        match UnixStream::connect(&path) {
            Ok(stream) => {
                if let Some(delivery) = hand_over(stream, &payload, &path)? {
                    return Ok(Some(delivery));
                }
            }
            // A socket file with nothing listening is a stale record from a crashed holder.
            Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {}
            Err(e) => return Err(format!("connect {}: {e}", path.display())),
        }
    }
    Ok(None)
}

/// Write the launch to a connected legacy socket and judge who took it. `Ok(None)`: not a live
/// holder (nothing was delivered).
#[cfg(target_os = "macos")]
fn hand_over(
    mut stream: std::os::unix::net::UnixStream,
    payload: &[u8],
    path: &std::path::Path,
) -> Result<Option<Delivery>, String> {
    use std::io::Write;
    use std::net::Shutdown;

    let owner = peer::owner(&stream);
    // The owner is gone but the kernel still accepts: no one serves this socket, and a launch
    // written to it would only wait in a queue nobody reads.
    if matches!(owner, Owner::Gone(_)) {
        return settle(path, judge(owner, Closure::Silent));
    }
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    stream
        .write_all(payload)
        .and_then(|_| stream.flush())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    // The plugin reads to end-of-stream, so the launch is not taken until the write side is shut;
    // the holder's own close is then its answer.
    let _ = stream.shutdown(Shutdown::Write);
    let closure = wait_for_close(&mut stream);
    settle(path, judge(owner, closure))
}

#[cfg(target_os = "macos")]
fn settle(path: &std::path::Path, judgement: Judgement) -> Result<Option<Delivery>, String> {
    match judgement {
        Judgement::Delivered(delivery) => Ok(Some(delivery)),
        Judgement::Stale(why) => {
            eprintln!(
                "rightkit-shell: ignoring the legacy single-instance socket {}: {why}",
                path.display()
            );
            Ok(None)
        }
        Judgement::Refused(why) => Err(format!("legacy holder at {}: {why}", path.display())),
    }
}

/// Wait, event-driven (one kernel receive timeout per call), for the holder to close its end.
#[cfg(target_os = "macos")]
fn wait_for_close(stream: &mut std::os::unix::net::UnixStream) -> Closure {
    use std::io::{ErrorKind, Read};
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(CLOSE_WAIT_SECS);
    let mut reply = [0_u8; 64];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            return Closure::Silent;
        }
        match stream.read(&mut reply) {
            // End-of-stream: the holder closed its end, which is its answer.
            Ok(0) => return Closure::Answered,
            // Bytes: the holder replied, which is also an answer.
            Ok(_received) => return Closure::Answered,
            Err(e) => match e.kind() {
                ErrorKind::Interrupted => {}
                ErrorKind::WouldBlock | ErrorKind::TimedOut => return Closure::Silent,
                _ => return Closure::Torn,
            },
        }
    }
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

#[cfg(test)]
mod tests {
    use super::liveness::{judge, same_program, Closure, Judgement, Owner};
    use super::*;
    use std::path::Path;

    fn stale(j: &Judgement) -> bool {
        matches!(j, Judgement::Stale(_))
    }

    #[test]
    fn a_closed_connection_confirms_the_launch_whoever_owns_the_socket() {
        for owner in [Owner::App(10), Owner::Other(11), Owner::Unknown] {
            assert_eq!(
                judge(owner, Closure::Answered),
                Judgement::Delivered(Delivery::Confirmed),
                "{owner:?}"
            );
        }
    }

    #[test]
    fn a_socket_whose_owner_exited_is_stale_whatever_the_connection_does() {
        for closure in [Closure::Answered, Closure::Silent, Closure::Torn] {
            assert!(stale(&judge(Owner::Gone(7), closure)), "{closure:?}");
        }
    }

    #[test]
    fn silence_from_another_program_is_stale() {
        assert!(stale(&judge(Owner::Other(7), Closure::Silent)));
        assert!(stale(&judge(Owner::Other(7), Closure::Torn)));
    }

    #[test]
    fn silence_from_this_app_or_an_unknown_owner_is_unconfirmed_never_stale() {
        for owner in [Owner::App(7), Owner::Unknown] {
            assert!(
                matches!(
                    judge(owner, Closure::Silent),
                    Judgement::Delivered(Delivery::Unconfirmed(_))
                ),
                "{owner:?}"
            );
        }
    }

    #[test]
    fn a_torn_connection_to_a_possibly_live_holder_is_refused_never_stale() {
        for owner in [Owner::App(7), Owner::Unknown] {
            assert!(
                matches!(judge(owner, Closure::Torn), Judgement::Refused(_)),
                "{owner:?}"
            );
        }
    }

    #[test]
    fn this_app_is_the_same_executable_or_the_same_bundle() {
        let exe = Path::new("/nonexistent-rk/Applications/Foo.app/Contents/MacOS/foo");
        // Same file.
        assert!(same_program(exe, exe));
        // Another executable in the same bundle (a renamed binary).
        assert!(same_program(
            exe,
            Path::new("/nonexistent-rk/Applications/Foo.app/Contents/MacOS/foo-next")
        ));
        // Another bundle, even with the same executable name.
        assert!(!same_program(
            exe,
            Path::new("/nonexistent-rk/Users/me/Foo-copy.app/Contents/MacOS/foo")
        ));
        // A helper app nested inside the bundle is not the main program.
        assert!(!same_program(
            exe,
            Path::new(
                "/nonexistent-rk/Applications/Foo.app/Contents/Frameworks/Helper.app/Contents/MacOS/h"
            )
        ));
        // Programs outside any bundle are equal only when they are the same file.
        assert!(same_program(
            Path::new("/nonexistent-rk/bin/sleep"),
            Path::new("/nonexistent-rk/bin/sleep")
        ));
        assert!(!same_program(Path::new("/nonexistent-rk/bin/sleep"), exe));
    }

    #[cfg(target_os = "macos")]
    mod sockets {
        use super::*;
        use std::io::Read;
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};

        /// Removes the socket file when the test ends, pass or fail.
        struct Socket(PathBuf);

        impl Drop for Socket {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        /// An identifier no other socket in `/tmp` can share the stem of, or match by prefix.
        fn unique_identifier(tag: &str) -> String {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() % 1_000_000_000)
                .unwrap_or(0);
            format!("rk.li-{tag}-{}-{nanos}", std::process::id())
        }

        fn socket_of(identifier: &str) -> Socket {
            let path = PathBuf::from(format!("/tmp/{}_si.sock", socket_stem(identifier)));
            let _ = std::fs::remove_file(&path);
            Socket(path)
        }

        #[test]
        fn a_holder_that_reads_the_launch_and_closes_confirms_it() {
            let identifier = unique_identifier("answer");
            let socket = socket_of(&identifier);
            let listener = UnixListener::bind(&socket.0).unwrap();
            // Like the plugin: read to end-of-stream, then drop the connection.
            let holder = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut payload = Vec::new();
                stream.read_to_end(&mut payload).unwrap();
                payload
            });
            let argv = vec!["app".to_owned(), "--section".to_owned(), "x".to_owned()];
            // Confirmed, not merely delivered: a silent holder would be Unconfirmed after the wait.
            let delivery = forward_checked(&identifier, &argv, "/work").unwrap();
            assert_eq!(delivery, Some(Delivery::Confirmed));
            assert_eq!(holder.join().unwrap(), socket_payload(&argv, "/work"));
        }

        #[test]
        fn a_listener_nobody_serves_in_this_app_is_unconfirmed_within_the_bound() {
            let identifier = unique_identifier("silent");
            let socket = socket_of(&identifier);
            // Bound and listening, never accepted: connects succeed, nothing ever reads.
            let _listener = UnixListener::bind(&socket.0).unwrap();
            let argv = vec!["app".to_owned()];
            let started = Instant::now();
            let delivery = forward_checked(&identifier, &argv, "/work").unwrap();
            assert!(
                matches!(delivery, Some(Delivery::Unconfirmed(_))),
                "got {delivery:?}"
            );
            assert!(started.elapsed() < Duration::from_secs(CLOSE_WAIT_SECS + 8));
        }

        #[test]
        fn no_socket_means_no_holder() {
            let identifier = unique_identifier("absent");
            let _socket = socket_of(&identifier);
            let argv = vec!["app".to_owned()];
            assert_eq!(forward_checked(&identifier, &argv, "/work"), Ok(None));
        }
    }
}
