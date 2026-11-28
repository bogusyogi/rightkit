//! Telemetry of the CLI, `run` and TUI paths (cli-and-tui section 7, D4).
//!
//! Every process installs one `tracing` subscriber that writes JSON lines to
//! daily files under `<state root>/logs`, so an InlineHost warning is kept
//! instead of dropped. Nothing is written to the terminal: the TUI owns it.
//! While the TUI's viewport is up, raw stderr (a library `eprintln!`, a
//! child's diagnostic) goes to `<state root>/logs/stderr.log`, not to the
//! null device.

use std::sync::Mutex;

/// Daily files kept, today's included.
const LOG_FILES: usize = 7;
/// Raw stderr file: rotated once it passes this size, one predecessor kept.
const STDERR_LOG_MAX_BYTES: u64 = 1_048_576;

static GUARD: Mutex<Option<rightkit_logs::TracingGuard>> = Mutex::new(None);

/// Storage D-S38: `<state root>/crash` keeps the newest 20 reports (the native
/// dumps and the size cap keep the crate's defaults).
pub(crate) fn crash_retention() -> rightkit_crash::Retention {
    rightkit_crash::Retention {
        max_reports: 20,
        ..rightkit_crash::Retention::default()
    }
}

/// `<state root>/logs`.
pub(crate) fn log_dir() -> Option<std::path::PathBuf> {
    rightkit_agent_config::state_root().map(|root| root.join("logs"))
}

/// The tracing settings for `dir`.
pub(crate) fn file_config(dir: &std::path::Path) -> rightkit_logs::TracingFileConfig {
    // The stem of the daily tracing files: `{app}.<date>.jsonl`.
    let mut config = rightkit_logs::TracingFileConfig::new(dir, crate::host_identity::app());
    config.max_files = LOG_FILES;
    config
}

/// Whether this invocation installs the file log. `serve` does not: it
/// installs the daemon's own subscriber (`rightkit_agent_telemetry::init`) and a
/// second global install fails silently, which would drop the daemon's log.
pub(crate) fn installs_file_log(args: &[String]) -> bool {
    args.get(1).map(String::as_str) != Some("serve")
}

/// Install the process's one subscriber. A missing state root or a second
/// install is not an error for the call; the call runs without a log file.
pub(crate) fn install() {
    let Some(dir) = log_dir() else {
        return;
    };
    if let Ok(guard) = rightkit_logs::install_tracing(file_config(&dir)) {
        *GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(guard);
    }
}

/// Write every accepted line and stop the writer. Called before the process
/// exits, because `process::exit` drops no statics.
pub(crate) fn flush() {
    let guard = GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    drop(guard);
}

/// Rotation size of the daemon's log file, and how many rotated files are kept.
const SERVE_LOG_MAX_BYTES: u64 = 1_048_576;
const SERVE_LOG_GENERATIONS: u32 = 4;

/// `<state root>/logs/serve.log`.
pub(crate) fn serve_log_path() -> Option<std::path::PathBuf> {
    log_dir().map(|dir| dir.join("serve.log"))
}

/// A standalone `{app} serve` keeps its tracing on stderr, which the
/// desktop shell and the autostart capture read, and also writes every line to
/// the rotating file `<state root>/logs/serve.log`. The daemon installs one
/// subscriber (`rightkit_agent_telemetry::init`, stderr); stderr itself is pointed at
/// a pipe and a thread copies each line to the original stderr and to the file.
/// There is no second global subscriber, and the same tee serves Unix (fd 2)
/// and Windows (the process's standard-error handle). Lines are scrubbed of the
/// host's known secrets. Best effort: any failure leaves stderr as it was.
pub(crate) fn tee_serve_stderr_to_log_file() {
    let Some(path) = serve_log_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let Some((reader, original, restore)) = redirect_stderr_to_pipe() else {
        return;
    };
    let secrets = rightkit_llm::agent_providers::host_credentials::host().known_secrets();
    let writer = rightkit_logs::RotatingTextWriter::new(path, SERVE_LOG_MAX_BYTES)
        .with_generations(SERVE_LOG_GENERATIONS);
    if let Ok(copier) = std::thread::Builder::new()
        .name(crate::host_identity::prefixed("serve-log"))
        .spawn(move || copy_lines(reader, original, &writer, &secrets))
    {
        if let Ok(mut slot) = SERVE_TEE.lock() {
            *slot = Some((copier, restore));
        }
    }
}

/// `process::exit` for the `serve` path: drains the stderr tee first.
pub(crate) fn exit_after_tee(code: i32) -> ! {
    // A forced daemon exit (shutdown deadline, drain watchdog) runs no destructor either (issue 465).
    rightkit_agent_coding_tools::browser::close_owned_browsers_at_exit(
        std::time::Duration::from_secs(3),
    );
    finish_serve_tee();
    std::process::exit(code)
}

/// The running tee: its copy thread and what puts stderr back.
static SERVE_TEE: std::sync::Mutex<Option<(std::thread::JoinHandle<()>, StderrRestore)>> =
    std::sync::Mutex::new(None);

/// Put stderr back and wait until every line written so far has reached the
/// original stderr and the log file. Call it before `process::exit`, which would
/// otherwise kill the copy thread with lines still in the pipe.
pub(crate) fn finish_serve_tee() {
    let taken = SERVE_TEE.lock().ok().and_then(|mut slot| slot.take());
    if let Some((copier, restore)) = taken {
        restore_stderr(restore);
        if copier.join().is_err() {
            tracing::debug!("the serve log copier thread panicked");
        }
    }
}

/// Read `reader` line by line: each line goes unchanged to `original` (stderr)
/// and scrubbed to `writer`. Ends when the pipe closes.
fn copy_lines(
    reader: std::fs::File,
    mut original: std::fs::File,
    writer: &rightkit_logs::RotatingTextWriter,
    secrets: &rightkit_logs::KnownSecrets,
) {
    use std::io::{BufRead, Write};
    let mut lines = std::io::BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match lines.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        // A closed stderr must not stop the copy to the file, and a file that cannot be written
        // must not stop stderr: each side is best effort.
        original.write_all(&buffer).ok();
        let text = String::from_utf8_lossy(&buffer);
        writer
            .write_line(&secrets.redact(text.trim_end_matches(['\r', '\n'])))
            .ok();
    }
}

/// Point fd 2 at a fresh pipe. Returns the pipe's read end and the original
/// stderr, or `None` when anything failed (stderr is then untouched).
#[cfg(unix)]
fn redirect_stderr_to_pipe() -> Option<(std::fs::File, std::fs::File, StderrRestore)> {
    use std::os::fd::FromRawFd;
    // SAFETY: plain fd duplication on this process's own stderr and a fresh pipe.
    unsafe {
        let mut fds = [0i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        let original = libc::dup(2);
        if original < 0 || libc::dup2(fds[1], 2) < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            if original >= 0 {
                libc::close(original);
            }
            return None;
        }
        libc::close(fds[1]);
        let restore = libc::dup(original);
        if restore < 0 {
            libc::dup2(original, 2);
            libc::close(original);
            libc::close(fds[0]);
            return None;
        }
        Some((
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(original),
            StderrRestore(restore),
        ))
    }
}

/// A duplicate of the original stderr, used to point fd 2 back at it.
#[cfg(unix)]
struct StderrRestore(i32);

/// Point fd 2 back at the original stderr. That closes the pipe's last write end,
/// so the copy thread reads end-of-file and stops.
#[cfg(unix)]
fn restore_stderr(restore: StderrRestore) {
    // SAFETY: `restore.0` is a duplicate of this process's original stderr.
    unsafe {
        libc::dup2(restore.0, 2);
        libc::close(restore.0);
    }
}

/// Point the process's standard-error handle at a fresh pipe (Rust's
/// `io::stderr` reads the handle on every write, so the subscriber's writes
/// reach the pipe). Returns the pipe's read end and the original handle.
#[cfg(windows)]
fn redirect_stderr_to_pipe() -> Option<(std::fs::File, std::fs::File, StderrRestore)> {
    use std::os::windows::io::FromRawHandle;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreatePipe(
            read: *mut isize,
            write: *mut isize,
            attributes: *mut core::ffi::c_void,
            size: u32,
        ) -> i32;
        fn GetStdHandle(which: u32) -> isize;
        fn SetStdHandle(which: u32, handle: isize) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    // SAFETY: handle queries and swaps on this process's own standard handles
    // and one fresh anonymous pipe; every handle ends up owned by one `File`
    // or is closed on failure.
    unsafe {
        let original = GetStdHandle(STD_ERROR_HANDLE);
        if original == 0 || original == -1 {
            return None;
        }
        let (mut read, mut write) = (0isize, 0isize);
        if CreatePipe(&mut read, &mut write, core::ptr::null_mut(), 0) == 0 {
            return None;
        }
        if SetStdHandle(STD_ERROR_HANDLE, write) == 0 {
            CloseHandle(read);
            CloseHandle(write);
            return None;
        }
        Some((
            std::fs::File::from_raw_handle(read as *mut core::ffi::c_void),
            std::fs::File::from_raw_handle(original as *mut core::ffi::c_void),
            StderrRestore { original, write },
        ))
    }
}

/// The original standard-error handle and the pipe's write end.
#[cfg(windows)]
struct StderrRestore {
    original: isize,
    write: isize,
}

/// Point the standard-error handle back at the original and close the pipe's
/// write end, so the copy thread reads end-of-file and stops.
#[cfg(windows)]
fn restore_stderr(restore: StderrRestore) {
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetStdHandle(which: u32, handle: isize) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    // SAFETY: restores this process's own standard-error handle and closes the
    // pipe write end this module created.
    unsafe {
        SetStdHandle(STD_ERROR_HANDLE, restore.original);
        CloseHandle(restore.write);
    }
}

/// The file raw stderr goes to while the TUI owns the terminal, opened for
/// append after rotating a file that passed its size bound. `None` when the
/// state root or the file is unavailable.
pub(crate) fn open_stderr_sink() -> Option<std::fs::File> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("stderr.log");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= STDERR_LOG_MAX_BYTES) {
        // A rotation that fails leaves the file to grow; it never stops the log.
        std::fs::rename(&path, dir.join("stderr.log.1")).ok();
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}
