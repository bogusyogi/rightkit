//! Keep WindowServer enumeration out of the shell's CoreAnimation connection.
//!
//! A pre-armed 0.1.199 profile caught CGWindowListCopyWindowInfo waiting on
//! SLSConnectionSynchronizeSLSCATransaction while AppKit's frame commit waited
//! on the same connection lock (513 ms). A background thread is insufficient.
//! The shell uses a persistent, UI-free instance of its own signed executable;
//! non-UI engine callers retain the direct query. No cached destination fallback.

use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    data::CFData,
    propertylist,
};
use rightkit_child_process::{OwnedChild, OwnedCommand};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use std::time::{Duration, Instant};

const HELPER_ARG: &str = "--rightkit-window-query-helper";
const QUERY_BUDGET: Duration = Duration::from_millis(150);
const RESTART_BACKOFF: Duration = Duration::from_secs(1);
const MAX_REPLY: usize = 2 * 1024 * 1024;
static ISOLATED: AtomicBool = AtomicBool::new(false);
static DIAGNOSTIC: OnceLock<fn(&str, u32, &str)> = OnceLock::new();

type WindowList = CFArray<*const std::ffi::c_void>;

pub fn set_window_query_diagnostic_hook(hook: fn(&str, u32, &str)) {
    let _ = DIAGNOSTIC.set(hook);
}

pub fn enable_isolated_window_queries() {
    ISOLATED.store(true, Ordering::Release);
    if let Ok(mut client) = client().lock() {
        start_if_needed(&mut client);
    }
}

fn direct_metadata() -> Option<WindowList> {
    use core_graphics::window::*;
    copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )
}

fn encode_metadata() -> Vec<u8> {
    direct_metadata()
        .and_then(|windows| {
            propertylist::create_data(
                windows.as_CFTypeRef(),
                propertylist::kCFPropertyListBinaryFormat_v1_0,
            )
            .ok()
        })
        .filter(|data| data.len() as usize <= MAX_REPLY)
        .map(|data| data.bytes().to_vec())
        .unwrap_or_default()
}

fn decode_metadata(bytes: &[u8]) -> Option<WindowList> {
    let (raw, _) = propertylist::create_with_data(
        CFData::from_buffer(bytes),
        propertylist::kCFPropertyListImmutable,
    )
    .ok()?;
    let value = unsafe { CFType::wrap_under_create_rule(raw) };
    // Callers use dictionary references directly; reject malformed element
    // types before allowing those casts, even on this private transport.
    let array = value.downcast::<WindowList>()?;
    if array.iter().any(|raw| {
        unsafe { CFType::wrap_under_get_rule(*raw) }
            .downcast::<core_foundation::dictionary::CFDictionary>()
            .is_none()
    }) {
        return None;
    }
    Some(array)
}

fn lifecycle(phase: &str, pid: u32, detail: &str) {
    if let Some(hook) = DIAGNOSTIC.get() {
        hook(phase, pid, detail);
    }
    // Fixed phase/detail codes only: no window titles, paths or editor content.
    eprintln!("{{\"event\":\"window_query_helper\",\"phase\":\"{phase}\",\"pid\":{pid},\"detail\":\"{detail}\"}}");
}

fn serve(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut request = [0u8; 1];
    while input.read(&mut request)? != 0 {
        if request[0] != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid window query",
            ));
        }
        let bytes = encode_metadata();
        output.write_all(&(bytes.len() as u32).to_le_bytes())?;
        output.write_all(&bytes)?;
        output.flush()?;
    }
    Ok(())
}

/// Run before initializing AppKit, single-instance handling, or engine state.
/// EOF on the private inherited socket also ends the helper after parent exit.
pub fn run_window_query_helper() -> bool {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) {
        return false;
    }
    lifecycle("ready", std::process::id(), "ui_free");
    let result = serve(io::stdin().lock(), io::stdout().lock());
    lifecycle(
        "exited",
        std::process::id(),
        if result.is_ok() {
            "parent_closed"
        } else {
            "protocol_io"
        },
    );
    true
}

struct Helper {
    child: OwnedChild,
    stream: UnixStream,
}

impl Helper {
    fn spawn() -> io::Result<Self> {
        let (stream, remote) = UnixStream::pair()?;
        let input: OwnedFd = remote.try_clone()?.into();
        let output: OwnedFd = remote.into();
        let mut command = OwnedCommand::new(std::env::current_exe()?);
        command
            .command_mut()
            .arg(HELPER_ARG)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::inherit());
        // The owner ends the helper if this process dies, and kills and reaps it on drop.
        command.kill_on_parent_exit(true);
        let mut child = command.spawn()?;
        // Teardown happens inside the caller's 150 ms query deadline: Drop kills at once and must
        // not wait. ZERO is the owner's "do not wait" control; it reaps on a background thread.
        child.set_drop_reap_timeout(Duration::ZERO);
        lifecycle("started", child.id(), "isolated_connection");
        Ok(Self { child, stream })
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Closing the private socket ends the helper's serve loop; dropping the owned
        // child then kills it at once and the owner reaps it in the background (zero drop wait).
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "window query deadline"))
}

fn read_until(stream: &mut UnixStream, bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let count = stream.read(&mut bytes[offset..])?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        offset += count;
    }
    Ok(())
}

fn request(stream: &mut UnixStream, deadline: Instant) -> io::Result<Option<WindowList>> {
    stream.set_write_timeout(Some(remaining(deadline)?))?;
    stream.write_all(&[1])?;
    let mut header = [0; 4];
    read_until(stream, &mut header, deadline)?;
    let size = u32::from_le_bytes(header) as usize;
    if size > MAX_REPLY {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if size == 0 {
        return Ok(None);
    }
    let mut bytes = vec![0; size];
    read_until(stream, &mut bytes, deadline)?;
    let windows = decode_metadata(&bytes).ok_or(io::ErrorKind::InvalidData)?;
    remaining(deadline)?;
    Ok(Some(windows))
}

#[derive(Default)]
struct Client {
    helper: Option<Helper>,
    retry_after: Option<Instant>,
    starting: bool,
}

fn client() -> &'static Mutex<Client> {
    static CLIENT: OnceLock<Mutex<Client>> = OnceLock::new();
    CLIENT.get_or_init(|| Mutex::new(Client::default()))
}

fn start_if_needed(state: &mut Client) {
    if state.helper.is_some()
        || state.starting
        || state.retry_after.is_some_and(|at| Instant::now() < at)
    {
        return;
    }
    state.starting = true;
    // First connection setup may exceed a query budget. Warm it independently
    // of callers, including after a crash. Pending callers get no identity.
    let spawn = std::thread::Builder::new()
        .name("window-query-start".into())
        .spawn(|| {
            let ready = Helper::spawn().and_then(|mut helper| {
                request(&mut helper.stream, Instant::now() + Duration::from_secs(2))?;
                lifecycle("warmed", helper.child.id(), "fresh_queries_ready");
                Ok(helper)
            });
            if let Ok(mut state) = client().lock() {
                state.starting = false;
                match ready {
                    Ok(helper) => {
                        state.helper = Some(helper);
                        state.retry_after = None;
                    }
                    Err(_) => {
                        lifecycle("failed", std::process::id(), "startup");
                        state.retry_after = Some(Instant::now() + RESTART_BACKOFF);
                    }
                }
            }
        });
    if spawn.is_err() {
        state.starting = false;
        state.retry_after = Some(Instant::now() + RESTART_BACKOFF);
        lifecycle("failed", std::process::id(), "thread_spawn");
    }
}

/// Fresh on-screen metadata with the same flags/order as the original query.
/// Busy, timed-out, crashed or malformed helpers yield unavailable identity.
/// The shell must never fall back to an in-process query or a stale snapshot.
pub fn window_metadata() -> Option<WindowList> {
    if !ISOLATED.load(Ordering::Acquire) {
        return direct_metadata();
    }
    let deadline = Instant::now() + QUERY_BUDGET;
    let mutex = client();
    let mut client = loop {
        match mutex.try_lock() {
            Ok(client) => break client,
            Err(std::sync::TryLockError::Poisoned(_)) => return None,
            Err(std::sync::TryLockError::WouldBlock) => {
                remaining(deadline).ok()?;
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };
    if client.retry_after.is_some_and(|at| Instant::now() < at) {
        return None;
    }
    if client.helper.is_none() {
        start_if_needed(&mut client);
        return None;
    }
    let helper = client.helper.as_mut()?;
    match request(&mut helper.stream, deadline) {
        Ok(windows) => windows,
        Err(error) => {
            lifecycle(
                "failed",
                helper.child.id(),
                match error.kind() {
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "deadline",
                    io::ErrorKind::InvalidData => "protocol",
                    _ => "io",
                },
            );
            client.helper = None;
            client.retry_after = Some(Instant::now() + RESTART_BACKOFF);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_window_server_ids_order_and_metadata() {
        use core_foundation::{dictionary::CFDictionary, number::CFNumber, string::CFString};
        let first = CFDictionary::from_CFType_pairs(&[
            (
                CFString::from_static_string("kCGWindowNumber"),
                CFNumber::from(123i32).as_CFType(),
            ),
            (
                CFString::from_static_string("kCGWindowOwnerPID"),
                CFNumber::from(456i32).as_CFType(),
            ),
        ]);
        let second = CFDictionary::from_CFType_pairs(&[(
            CFString::from_static_string("kCGWindowNumber"),
            CFNumber::from(789i32).as_CFType(),
        )]);
        let original = CFArray::from_CFTypes(&[first.as_CFType(), second.as_CFType()]);
        let bytes = propertylist::create_data(
            original.as_CFTypeRef(),
            propertylist::kCFPropertyListBinaryFormat_v1_0,
        )
        .unwrap();
        let restored = decode_metadata(bytes.bytes()).unwrap();
        assert_eq!(original.as_CFType(), restored.as_CFType());
    }

    #[test]
    fn hung_helper_obeys_total_deadline() {
        let (mut client, _hung) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let error = request(&mut client, started + Duration::from_millis(30)).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn partial_reply_cannot_extend_deadline_or_become_identity() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.write_all(&20u32.to_le_bytes()).unwrap();
        server.write_all(&[1, 2]).unwrap();
        assert!(request(&mut client, Instant::now() + Duration::from_millis(30)).is_err());
    }

    #[test]
    fn malformed_oversized_and_closed_replies_fail() {
        for bytes in [vec![255; 4], vec![1, 0, 0, 0, 9], vec![]] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            server.write_all(&bytes).unwrap();
            // Keep peer's read half open so request write succeeds.
            server.shutdown(std::net::Shutdown::Write).unwrap();
            assert!(request(&mut client, Instant::now() + QUERY_BUDGET).is_err());
        }
    }

    #[test]
    fn empty_reply_preserves_unavailable_identity() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.write_all(&0u32.to_le_bytes()).unwrap();
        assert!(request(&mut client, Instant::now() + QUERY_BUDGET)
            .unwrap()
            .is_none());
    }
}
