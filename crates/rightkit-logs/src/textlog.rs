//! Plain-text rotating log writer and child-process stderr capture.

use crate::rotate::{self, RotatedNaming};
use crate::scrub::scrub_text;
use crate::store::StoreStatus;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const DEFAULT_NAME: &str = "app.log";
/// Longest single captured line; longer output is split into several lines.
const MAX_CAPTURED_LINE_BYTES: u64 = 64 * 1024;

/// Append-only plain-text log with size rotation, sharing the rotation code (and the one-`.1`
/// default, hard cap and failure counting) of [`crate::JsonlStore`]. Each [`write_line`] appends
/// the text and a `\n`; nothing is scrubbed here (see [`capture_stderr`] for scrubbed capture).
///
/// [`write_line`]: RotatingTextWriter::write_line
#[derive(Debug)]
pub struct RotatingTextWriter {
    path: PathBuf,
    max_file_bytes: u64,
    generations: u32,
    naming: RotatedNaming,
    rotate_on_open: bool,
    /// Held across writes; `true` once the open-time rotation has run (or been done by hand).
    lock: Mutex<bool>,
    failures: Mutex<StoreStatus>,
}

impl RotatingTextWriter {
    /// `max_file_bytes` is the rotation size; `0` disables rotation. One predecessor is kept
    /// unless [`with_generations`](Self::with_generations) says otherwise.
    pub fn new(path: impl Into<PathBuf>, max_file_bytes: u64) -> Self {
        Self {
            path: path.into(),
            max_file_bytes,
            generations: 1,
            naming: RotatedNaming::Suffix,
            rotate_on_open: false,
            lock: Mutex::new(false),
            failures: Mutex::new(StoreStatus::default()),
        }
    }

    /// Keep `generations` rotated predecessors (`.1` newest through `.N` oldest); `0` behaves as 1.
    pub fn with_generations(mut self, generations: u32) -> Self {
        self.generations = generations.max(1);
        self
    }

    /// Name rotated files with `naming`. The default is [`RotatedNaming::Suffix`] (`app.log.1`);
    /// [`RotatedNaming::StemNumbered`] gives `app.1.log`.
    pub fn with_naming(mut self, naming: RotatedNaming) -> Self {
        self.naming = naming;
        self
    }

    /// Start a fresh file for each launch: before the first write (or [`rotate_now`]), a
    /// non-empty current file is rolled into generation 1, older generations shift up, and the
    /// oldest beyond [`with_generations`](Self::with_generations) is dropped. Happens once per
    /// writer; size rotation still applies afterwards. A failed rotation is counted in
    /// [`status`](Self::status) and the write still appends to the existing file.
    ///
    /// [`rotate_now`]: Self::rotate_now
    pub fn with_rotate_on_open(mut self, rotate_on_open: bool) -> Self {
        self.rotate_on_open = rotate_on_open;
        self
    }

    /// Roll the current file into generation 1 now, regardless of size. A missing or empty file
    /// is left alone (`Ok`). Marks the open-time rotation as done, so it does not repeat. A
    /// failure is returned and counted in [`status`](Self::status).
    pub fn rotate_now(&self) -> io::Result<()> {
        let mut opened = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        *opened = true;
        self.roll_current()
    }

    fn roll_current(&self) -> io::Result<()> {
        match fs::metadata(&self.path) {
            Ok(m) if m.len() > 0 => {}
            _ => return Ok(()),
        }
        let result =
            rotate::roll_generations(&self.path, self.generations, DEFAULT_NAME, self.naming);
        if let Err(error) = &result {
            self.record(|status| {
                status.failed_rotations = status.failed_rotations.saturating_add(1);
                status.last_error = Some(format!(
                    "rotate {} failed ({error}); line kept in the current file",
                    self.path.display()
                ));
            });
        }
        result
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Counts and last-error for rotations and appends that failed. Never blocks on a write.
    pub fn status(&self) -> StoreStatus {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Append `line` plus a newline, rotating first when the file has reached the rotation size.
    /// A failed rotation still appends up to twice the rotation size, then refuses. Failures are
    /// returned and also counted in [`status`](Self::status).
    pub fn write_line(&self, line: &str) -> io::Result<()> {
        let mut guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let result = self.write_locked(line, &mut guard);
        if let Err(error) = &result {
            self.record(|status| {
                status.failed_appends = status.failed_appends.saturating_add(1);
                status.last_error = Some(format!(
                    "append to {} failed ({error})",
                    self.path.display()
                ));
            });
        }
        result
    }

    fn write_locked(&self, line: &str, opened: &mut bool) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.rotate_on_open && !*opened {
            *opened = true;
            // A failure is counted inside; the line is still appended.
            let _ = self.roll_current();
        }
        let size_rotation = if self.max_file_bytes == 0 {
            Ok(())
        } else {
            match fs::metadata(&self.path) {
                Ok(m) if m.len() >= self.max_file_bytes => rotate::roll_generations(
                    &self.path,
                    self.generations,
                    DEFAULT_NAME,
                    self.naming,
                ),
                _ => Ok(()),
            }
        };
        if let Err(error) = size_rotation {
            self.record(|status| {
                status.failed_rotations = status.failed_rotations.saturating_add(1);
                status.last_error = Some(format!(
                    "rotate {} failed ({error}); line kept in the current file",
                    self.path.display()
                ));
            });
            rotate::refuse_at_hard_cap(&self.path, self.max_file_bytes)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        file.write_all(&bytes)
    }

    fn record(&self, update: impl FnOnce(&mut StoreStatus)) {
        let mut status = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        update(&mut status);
    }
}

/// Raw stderr lines for output that a consumer matches exactly, such as a renderer's
/// `[app] ...` diagnostics that QA scripts grep. Unlike [`capture_stderr`], nothing is scrubbed:
/// callers must not pass secrets or personal data here.
///
/// Each line is written as `prefix` + `line` + `\n` in one `write_all`, so it stays whole when
/// other threads write stderr at the same time. A closed or broken stderr is ignored, and nothing
/// here panics, so the functions are safe to call from an FFI callback.
#[derive(Debug, Clone, Copy)]
pub struct StderrLine;

impl StderrLine {
    /// Write one line to the process's stderr.
    pub fn write(prefix: &str, line: &str) {
        let mut bytes = String::with_capacity(prefix.len() + line.len() + 1);
        bytes.push_str(prefix);
        bytes.push_str(line);
        bytes.push('\n');
        let _ = io::stderr().lock().write_all(bytes.as_bytes());
    }

    /// Write one line to stderr and append the same unscrubbed line to `mirror`.
    pub fn write_mirrored(mirror: &RotatingTextWriter, prefix: &str, line: &str) {
        Self::write(prefix, line);
        let _ = mirror.write_line(&format!("{prefix}{line}"));
    }
}

/// Handle to a thread copying a child process's stderr into a [`RotatingTextWriter`].
#[derive(Debug)]
pub struct StderrCapture {
    handle: JoinHandle<u64>,
}

impl StderrCapture {
    /// Wait for the stream to end (the child closed stderr) and return the number of lines
    /// captured. Lines that failed to write are counted in the writer's status, not here.
    pub fn join(self) -> u64 {
        self.handle.join().unwrap_or(0)
    }
}

/// Copy `reader` (typically `ChildStderr`) into `writer` on a background thread, one record per
/// line: `prefix` + [`scrub_text`] of the line (home directories, e-mail addresses, credential-like
/// words). Rotation and generations are the writer's. Non-UTF-8 bytes are replaced; a line longer
/// than 64 KiB is split. Write failures never stop the capture; they are counted by the writer.
pub fn capture_stderr<R: Read + Send + 'static>(
    reader: R,
    writer: Arc<RotatingTextWriter>,
    prefix: impl Into<String>,
) -> io::Result<StderrCapture> {
    let prefix = prefix.into();
    let handle = std::thread::Builder::new()
        .name("rightkit-logs-stderr".into())
        .spawn(move || capture_loop(reader, &writer, &prefix))?;
    Ok(StderrCapture { handle })
}

fn capture_loop<R: Read>(reader: R, writer: &RotatingTextWriter, prefix: &str) -> u64 {
    let mut reader = io::BufReader::new(reader);
    let mut count = 0u64;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match io::BufRead::read_until(
            &mut (&mut reader).take(MAX_CAPTURED_LINE_BYTES),
            b'\n',
            &mut buf,
        ) {
            Ok(0) => return count,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return count,
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        let text = String::from_utf8_lossy(&buf);
        let _ = writer.write_line(&format!("{prefix}{}", scrub_text(&text)));
        count += 1;
    }
}
