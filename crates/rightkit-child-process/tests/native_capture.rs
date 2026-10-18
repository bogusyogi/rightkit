#![cfg(any(unix, windows))]

use rightkit_child_process::{NativeOutputCapture, OwnedChild, OwnedCommand, WaitOutcome};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const MODE: &str = "RIGHTKIT_NATIVE_CAPTURE_FIXTURE";
const DIRECTORY: &str = "RIGHTKIT_NATIVE_CAPTURE_DIRECTORY";

// Regular test in ordinary runs; re-executed in a dedicated process for real native pipes.
#[test]
fn capture_child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    match mode.as_str() {
        "normal" => {
            let bytes = vec![b'x'; 300_000];
            std::io::stdout().lock().write_all(&bytes).unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stderr().lock().write_all(b"err").unwrap();
            std::io::stderr().flush().unwrap();
        }
        "early" => {
            std::io::stdout().lock().write_all(b"early").unwrap();
            std::io::stdout().flush().unwrap();
        }
        "probe" => {
            std::io::stderr().lock().write_all(b"ready").unwrap();
            std::io::stderr().flush().unwrap();
            let _ = std::io::stdin().read_exact(&mut [0]);
            let result = std::io::stdout()
                .lock()
                .write_all(b"probe\n")
                .and_then(|_| std::io::stdout().flush());
            std::process::exit(if result.is_err() { 0 } else { 2 });
        }
        "parent" => {
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "capture_child", "--nocapture"])
                .env(MODE, "holder")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            // Deliberately outlives this test child, with a bounded 10-second fallback &
            // explicit release/done files owned by the parent test's fixture guard.
            drop(child);
        }
        "holder" => {
            let dir = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
            std::fs::write(dir.join("ready"), b"ready").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !dir.join("release").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let result = std::io::stdout()
                .lock()
                .write_all(b"probe\n")
                .and_then(|_| std::io::stdout().flush());
            let _ = std::fs::write(
                dir.join("done"),
                if result.is_err() {
                    b"closed".as_slice()
                } else {
                    b"open".as_slice()
                },
            );
        }
        other => panic!("unknown fixture mode {other}"),
    }
    std::process::exit(0);
}

fn child(mode: &str, directory: Option<&Path>) -> OwnedChild {
    let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
    command
        .command_mut()
        .args(["--exact", "capture_child", "--nocapture"])
        .env(MODE, mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = directory {
        command.command_mut().env(DIRECTORY, directory);
    }
    command.spawn().unwrap()
}

// A broken cancellation implementation must fail within a deadline, not hang the suite.
// The watchdog owns only a process handle, never an output capture being tested.
fn run_isolated(name: &str) -> bool {
    const CASE: &str = "RIGHTKIT_NATIVE_CAPTURE_CASE";
    if std::env::var(CASE).ok().as_deref() == Some(name) {
        return false;
    }
    let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
    command.windows_hide();
    command
        .command_mut()
        .args(["--exact", name, "--nocapture"])
        .env(CASE, name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut child = command.spawn().unwrap();
    assert!(
        matches!(child.wait_then_kill(Duration::from_secs(15)).unwrap(), WaitOutcome::Exited(status) if status.success()),
        "native capture scenario failed or exceeded 15 seconds: {name}"
    );
    true
}

#[test]
fn native_capture_drains_chatty_output_caps_retention_and_reaches_eof() {
    if run_isolated("native_capture_drains_chatty_output_caps_retention_and_reaches_eof") {
        return;
    }
    let mut child = child("normal", None);
    let mut stdout = NativeOutputCapture::stdout(child.take_stdout().unwrap(), 1024).unwrap();
    let mut stderr = NativeOutputCapture::stderr(child.take_stderr().unwrap(), 1024).unwrap();
    assert!(
        matches!(child.wait_then_kill(Duration::from_secs(5)).unwrap(), WaitOutcome::Exited(status) if status.success())
    );
    let output = stdout.finish_within(Duration::from_secs(2));
    assert_eq!(output.bytes.len(), 1024);
    assert!(output.total_bytes >= 300_000);
    assert!(output.truncated);
    assert_eq!(stderr.finish_within(Duration::from_secs(2)).bytes, b"err");
    assert!(stdout.is_complete() && stderr.is_complete());
    assert!(stdout.take_error().is_none() && stderr.take_error().is_none());
}

#[test]
fn cancel_and_drop_join_and_close_reader_while_child_still_holds_writer() {
    if run_isolated("cancel_and_drop_join_and_close_reader_while_child_still_holds_writer") {
        return;
    }
    for explicit_cancel in [true, false] {
        let mut child = child("probe", None);
        let mut stdout =
            Some(NativeOutputCapture::stdout(child.take_stdout().unwrap(), 1024).unwrap());
        let mut stderr = NativeOutputCapture::stderr(child.take_stderr().unwrap(), 1024).unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(3);
        while !stderr
            .snapshot()
            .bytes
            .windows(5)
            .any(|bytes| bytes == b"ready")
            && Instant::now() < ready_deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(stderr
            .snapshot()
            .bytes
            .windows(5)
            .any(|bytes| bytes == b"ready"));
        let started = Instant::now();
        if explicit_cancel {
            stdout.as_mut().unwrap().cancel();
        } else {
            drop(stdout.take());
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        child.take_stdin().unwrap().write_all(&[1]).unwrap();
        assert!(
            matches!(child.wait_then_kill(Duration::from_secs(5)).unwrap(), WaitOutcome::Exited(status) if status.success()),
            "reader must be closed before cancellation returns"
        );
        stderr.finish_within(Duration::from_secs(2));
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "rightkit-native-capture-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn wait_for(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.0.join(name).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(self.0.join(name).exists(), "fixture did not create {name}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::write(self.0.join("release"), b"release");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !self.0.join("done").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.0.join("done").exists() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[test]
fn finish_cancels_joins_and_closes_readers_after_leader_exits_with_descendant_pipes_open() {
    if run_isolated(
        "finish_cancels_joins_and_closes_readers_after_leader_exits_with_descendant_pipes_open",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let mut child = child("parent", Some(fixture.0.as_path()));
    let mut stdout = NativeOutputCapture::stdout(child.take_stdout().unwrap(), 1024).unwrap();
    let mut stderr = NativeOutputCapture::stderr(child.take_stderr().unwrap(), 1024).unwrap();
    fixture.wait_for("ready");
    assert!(child
        .wait_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap()
        .success());
    let started = Instant::now();
    stdout.finish_within(Duration::from_millis(30));
    stderr.finish_within(Duration::from_millis(30));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        !stdout.is_complete() && !stderr.is_complete(),
        "descendant must still hold the pipes"
    );
    std::fs::write(fixture.0.join("release"), b"release").unwrap();
    fixture.wait_for("done");
    assert_eq!(
        std::fs::read(fixture.0.join("done")).unwrap(),
        b"closed",
        "reader must be closed before finish returns"
    );
}

#[cfg(unix)]
#[test]
fn a_child_stdout_constructed_from_a_regular_file_is_rejected() {
    if run_isolated("a_child_stdout_constructed_from_a_regular_file_is_rejected") {
        return;
    }
    use std::os::fd::OwnedFd;
    let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let stdout = std::process::ChildStdout::from(OwnedFd::from(file));
    match NativeOutputCapture::stdout(stdout, 1024) {
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput),
        Ok(_) => panic!("regular files are not cancellable native pipes"),
    }
}

#[cfg(windows)]
#[test]
fn a_synchronous_windows_pipe_is_rejected_before_starting_a_reader() {
    if run_isolated("a_synchronous_windows_pipe_is_rejected_before_starting_a_reader") {
        return;
    }
    use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreatePipe(
            read: *mut RawHandle,
            write: *mut RawHandle,
            attributes: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    // SAFETY: output pointers are valid; null attributes creates non-inherited handles.
    assert_ne!(
        unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) },
        0
    );
    // SAFETY: CreatePipe returned two new owned handles, adopted exactly once.
    let read = unsafe { OwnedHandle::from_raw_handle(read) };
    let _write = unsafe { OwnedHandle::from_raw_handle(write) };
    match NativeOutputCapture::stdout(std::process::ChildStdout::from(read), 1024) {
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput),
        Ok(_) => panic!("synchronous handles cannot provide overlapped cancellation"),
    }
}

#[cfg(windows)]
#[test]
fn a_windows_child_stdout_constructed_from_a_regular_file_is_rejected() {
    if run_isolated("a_windows_child_stdout_constructed_from_a_regular_file_is_rejected") {
        return;
    }
    use std::os::windows::io::OwnedHandle;
    let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let stdout = std::process::ChildStdout::from(OwnedHandle::from(file));
    match NativeOutputCapture::stdout(stdout, 1024) {
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput),
        Ok(_) => panic!("regular files are not cancellable native pipes"),
    }
}

#[test]
fn capture_can_start_after_child_has_already_exited() {
    if run_isolated("capture_can_start_after_child_has_already_exited") {
        return;
    }
    let mut child = child("early", None);
    assert!(child
        .wait_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap()
        .success());
    let mut stdout = NativeOutputCapture::stdout(child.take_stdout().unwrap(), 1024).unwrap();
    let mut stderr = NativeOutputCapture::stderr(child.take_stderr().unwrap(), 1024).unwrap();
    assert!(stdout
        .finish_within(Duration::from_secs(2))
        .bytes
        .windows(5)
        .any(|bytes| bytes == b"early"));
    assert!(stderr
        .finish_within(Duration::from_secs(2))
        .bytes
        .is_empty());
    assert!(stdout.is_complete() && stderr.is_complete());
    assert!(stdout.take_error().is_none() && stderr.take_error().is_none());
}

#[cfg(windows)]
#[test]
fn a_disconnected_overlapped_windows_pipe_is_accepted_as_eof() {
    if run_isolated("a_disconnected_overlapped_windows_pipe_is_accepted_as_eof") {
        return;
    }
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            access: u32,
            mode: u32,
            instances: u32,
            out_size: u32,
            in_size: u32,
            timeout: u32,
            attributes: *const std::ffi::c_void,
        ) -> RawHandle;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            attributes: *const std::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: RawHandle,
        ) -> RawHandle;
        fn DisconnectNamedPipe(handle: RawHandle) -> i32;
        fn GetNamedPipeInfo(
            handle: RawHandle,
            flags: *mut u32,
            out_size: *mut u32,
            in_size: *mut u32,
            instances: *mut u32,
        ) -> i32;
    }
    let name: Vec<u16> = format!(
        "\\\\.\\pipe\\rightkit-capture-disconnected-{}",
        std::process::id()
    )
    .encode_utf16()
    .chain(Some(0))
    .collect();
    // SAFETY: terminated name, null security attributes, outbound byte-pipe server.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            2 | 0x0008_0000,
            8,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(server as isize, -1);
    // SAFETY: successful creation returned a new owned pipe handle.
    let server = unsafe { OwnedHandle::from_raw_handle(server) };
    // SAFETY: open an overlapped read client with no inherited attributes or template.
    let read = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x8000_0000,
            0,
            std::ptr::null(),
            3,
            0x4000_0080,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(read as isize, -1);
    // SAFETY: successful open returned a new owned handle.
    let read = unsafe { OwnedHandle::from_raw_handle(read) };
    // SAFETY: live server-end handle, connected by the read-end open above.
    assert_ne!(unsafe { DisconnectNamedPipe(server.as_raw_handle()) }, 0);
    // Confirm the orphaned client reaches the exact constructor edge.
    // SAFETY: live client-end handle; all optional outputs may be null.
    assert_eq!(
        unsafe {
            GetNamedPipeInfo(
                read.as_raw_handle(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(233));
    let mut capture =
        NativeOutputCapture::stdout(std::process::ChildStdout::from(read), 1024).unwrap();
    assert!(capture
        .finish_within(Duration::from_secs(1))
        .bytes
        .is_empty());
    assert!(capture.is_complete());
    assert!(capture.take_error().is_none());
}
