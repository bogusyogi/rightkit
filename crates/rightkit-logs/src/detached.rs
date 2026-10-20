//! Durable capture of a detached child's stderr.
//!
//! [`capture_stderr`](crate::capture_stderr) needs a live parent that drains a pipe. A detached
//! job (one that outlives the process that started it) has no such parent, and a pipe whose
//! reader exits kills or blocks the child. [`DetachedLog`] hands the child a **file** instead:
//! the child appends raw stderr straight to a private raw file, and whoever is alive later
//! (the parent while it runs, or the next launch after a restart) drains that file into a
//! [`RotatingTextWriter`], scrubbed, prefixed and rotated. A read position stored beside the raw
//! file lets a restarted process continue where the last drain stopped.

use crate::scrub::scrub_text_counted;
use crate::textlog::RotatingTextWriter;
use crate::KnownSecrets;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Longest single drained line; longer output is split into several lines.
const MAX_DRAINED_LINE_BYTES: u64 = 64 * 1024;
/// Raw size at which a fully drained raw file is emptied, unless configured otherwise.
const DEFAULT_COMPACT_AT: u64 = 4 * 1024 * 1024;
/// How long a drain waits for another process's drain before it skips.
const DRAIN_LOCK_WAIT: Duration = Duration::from_millis(500);
const DRAIN_LOCK_POLL: Duration = Duration::from_millis(10);

/// What one drain did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DrainReport {
    /// Lines written to the sink by this drain.
    pub lines: u64,
    /// Raw bytes consumed by this drain.
    pub bytes: u64,
    /// Raw bytes left unconsumed: an unfinished last line, waiting for its newline.
    pub pending_bytes: u64,
    /// The raw file was fully drained and emptied (see [`DetachedLog::with_compact_at`]).
    pub compacted: bool,
    /// [`DetachedLog::finish`] consumed everything and removed the raw file.
    pub finished: bool,
    /// Another process held the drain lock past the wait, so this drain did nothing.
    pub skipped: bool,
}

/// Durable stderr capture for a detached child. See the [module docs](self).
///
/// ```text
/// let sink = Arc::new(RotatingTextWriter::new(dir.join("job.log"), 1 << 20).with_generations(3));
/// let log = DetachedLog::new(sink).with_prefix("[job] ");
/// let child = Command::new(program).stderr(Stdio::from(log.stderr_file()?)).spawn()?;
/// // later, while the parent lives:   log.drain()?;
/// // after the child has exited:      log.finish()?;
/// // after a parent restart:          DetachedLog::new(same_sink).finish()?   (child known dead)
/// ```
///
/// The raw file (`<sink path>.raw` unless [`with_raw_path`](Self::with_raw_path) says otherwise)
/// holds the child's **unscrubbed** output until it is drained. It is created owner-only, and
/// must not sit in a directory that diagnostics collection or a support bundle reads. Only the
/// sink holds scrubbed text.
#[derive(Debug)]
pub struct DetachedLog {
    sink: Arc<RotatingTextWriter>,
    raw: PathBuf,
    prefix: String,
    secrets: KnownSecrets,
    compact_at: u64,
    /// Serialises drains inside this process.
    gate: Mutex<()>,
    last_error: Mutex<Option<String>>,
}

impl DetachedLog {
    /// Captures into `sink`. The raw file is `<sink path>.raw`.
    pub fn new(sink: Arc<RotatingTextWriter>) -> Self {
        let raw = sibling(sink.path(), ".raw");
        Self {
            sink,
            raw,
            prefix: String::new(),
            secrets: KnownSecrets::new(),
            compact_at: DEFAULT_COMPACT_AT,
            gate: Mutex::new(()),
            last_error: Mutex::new(None),
        }
    }

    /// Use `raw` for the raw file, for example to keep it outside a collected directory.
    pub fn with_raw_path(mut self, raw: impl Into<PathBuf>) -> Self {
        self.raw = raw.into();
        self
    }

    /// Put `prefix` before every drained line.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Remove these known secrets from every drained line, before the pattern scrub.
    pub fn with_known_secrets(mut self, secrets: KnownSecrets) -> Self {
        self.secrets = secrets;
        self
    }

    /// Empty the raw file once a drain has consumed all of it and it holds at least `bytes`.
    /// Default 4 MiB; `0` never empties it, so a chatty child grows it until [`finish`](Self::finish).
    ///
    /// The raw file is bounded only when it is drained. The child keeps its handle open, so
    /// emptying races the child's writes: bytes it appends between the final length check and
    /// the truncation are lost (never leaked). Text from a child that outruns the drain is
    /// dropped rather than left unscrubbed.
    pub fn with_compact_at(mut self, bytes: u64) -> Self {
        self.compact_at = bytes;
        self
    }

    /// Path of the raw file.
    pub fn raw_path(&self) -> &Path {
        &self.raw
    }

    /// The scrubbed sink.
    pub fn sink(&self) -> &Arc<RotatingTextWriter> {
        &self.sink
    }

    /// Whether the raw file exists and holds bytes not yet drained (undrained output from a
    /// run that ended without [`finish`](Self::finish), for example after a crash).
    pub fn has_pending(&self) -> bool {
        let len = match fs::metadata(&self.raw) {
            Ok(meta) => meta.len(),
            Err(_) => return false,
        };
        len > self.read_cursor().min(len)
    }

    /// The last drain error seen by a [`Drainer`] thread, if any. Cleared by a later success.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// An append-mode handle on the raw file for the child's stderr:
    /// `Command::stderr(Stdio::from(log.stderr_file()?))`. The parent may drop its copy and exit;
    /// the child keeps writing. The file is created owner-only; a symlink or non-regular file at
    /// the path is refused. A stale read position left from a raw file that is gone is cleared.
    pub fn stderr_file(&self) -> io::Result<File> {
        if let Some(parent) = self.raw.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::metadata(&self.raw).map_or(true, |meta| meta.len() == 0) {
            let _ = fs::remove_file(self.cursor_path());
        }
        rightkit_fs::open_private_append(&self.raw).map_err(fs_to_io)
    }

    /// Drain every complete line written so far into the sink. An unfinished last line stays in
    /// the raw file until its newline arrives. A missing raw file is an empty drain. Safe to call
    /// while the child runs, from several threads, and from several processes (one drains, the
    /// others wait up to 500 ms, then skip). A write failure stops the drain at that line, keeps
    /// the position before it, and returns the error, so nothing is dropped and the next drain
    /// retries.
    pub fn drain(&self) -> io::Result<DrainReport> {
        self.drain_inner(false)
    }

    /// Drain everything, including an unfinished last line, then remove the raw file and its
    /// position file. Call it only after the child has exited; a later write by the child would
    /// be lost. A raw file that cannot be removed (for example a handle still open on Windows)
    /// is an error and the output stays drained.
    pub fn finish(&self) -> io::Result<DrainReport> {
        self.drain_inner(true)
    }

    /// Drain on a background thread every `interval` until [`Drainer::stop`]. Errors are kept in
    /// [`last_error`](Self::last_error).
    pub fn spawn_drainer(self: &Arc<Self>, interval: Duration) -> io::Result<Drainer> {
        let log = Arc::clone(self);
        let signal = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_signal = Arc::clone(&signal);
        let handle = std::thread::Builder::new()
            .name("rightkit-logs-detached".into())
            .spawn(move || {
                let (stop, wake) = &*thread_signal;
                loop {
                    let outcome = log.drain();
                    *log.last_error.lock().unwrap_or_else(|p| p.into_inner()) =
                        outcome.err().map(|e| e.to_string());
                    let stopped = stop.lock().unwrap_or_else(|p| p.into_inner());
                    if *stopped {
                        return;
                    }
                    let (stopped, _) = wake
                        .wait_timeout(stopped, interval)
                        .unwrap_or_else(|p| p.into_inner());
                    if *stopped {
                        return;
                    }
                }
            })?;
        Ok(Drainer { signal, handle })
    }

    fn drain_inner(&self, finish: bool) -> io::Result<DrainReport> {
        let _gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
        let Some(_lock) = self.acquire_lock()? else {
            return Ok(DrainReport {
                skipped: true,
                ..DrainReport::default()
            });
        };
        let mut report = DrainReport::default();
        let mut file = match File::open(&self.raw) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if finish {
                    let _ = fs::remove_file(self.cursor_path());
                    report.finished = true;
                }
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        let len = file.metadata()?.len();
        let start = self.read_cursor();
        let start = if start > len { 0 } else { start };
        file.seek(SeekFrom::Start(start))?;
        // Bound the read to the snapshot so concurrent appends are left for the next drain.
        let mut reader = io::BufReader::new(file.take(len - start));
        let mut offset = start;
        let mut buf = Vec::new();
        let mut failure = None;
        loop {
            buf.clear();
            let read = io::BufRead::read_until(
                &mut (&mut reader).take(MAX_DRAINED_LINE_BYTES),
                b'\n',
                &mut buf,
            );
            let n = match read {
                Ok(0) => break,
                Ok(n) => n as u64,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            let complete = buf.last() == Some(&b'\n');
            let cut = !complete && n >= MAX_DRAINED_LINE_BYTES;
            if !complete && !cut && !finish {
                // An unfinished last line: wait for its newline.
                break;
            }
            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                buf.pop();
            }
            let text = String::from_utf8_lossy(&buf);
            let known_free = self.secrets.redact(&text);
            // A cut or unterminated line may end inside a credential, so the counted scrub (which
            // treats the end of input as a possible truncation) runs on every line; on a complete
            // line the only difference is a credential prefix that ends it.
            let scrubbed = scrub_text_counted(&known_free).0;
            if let Err(error) = self.sink.write_line(&format!("{}{scrubbed}", self.prefix)) {
                failure = Some(error);
                break;
            }
            offset += n;
            report.lines += 1;
            report.bytes += n;
        }
        report.pending_bytes = len.saturating_sub(offset);
        if offset != start {
            if let Err(error) = self.write_cursor(offset) {
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        drop(reader);
        if finish {
            remove_if_present(&self.raw)?;
            let _ = fs::remove_file(self.cursor_path());
            report.finished = true;
            report.pending_bytes = 0;
        } else if self.compact_at > 0 && offset >= self.compact_at && offset == len {
            report.compacted = self.compact(offset)?;
        }
        Ok(report)
    }

    /// Empty the raw file when it still ends where the drain stopped.
    fn compact(&self, consumed: u64) -> io::Result<bool> {
        let file = OpenOptions::new().write(true).open(&self.raw)?;
        if file.metadata()?.len() != consumed {
            return Ok(false);
        }
        file.set_len(0)?;
        self.write_cursor(0)?;
        Ok(true)
    }

    fn cursor_path(&self) -> PathBuf {
        sibling(&self.raw, ".pos")
    }

    fn lock_path(&self) -> PathBuf {
        sibling(&self.raw, ".lock")
    }

    /// The saved read position; a missing or unreadable file reads as 0 (drain from the start,
    /// which can repeat lines but never skip any).
    fn read_cursor(&self) -> u64 {
        fs::read_to_string(self.cursor_path())
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    fn write_cursor(&self, offset: u64) -> io::Result<()> {
        crate::atomic::write_atomic(&self.cursor_path(), offset.to_string().as_bytes())
    }

    fn acquire_lock(&self) -> io::Result<Option<File>> {
        if let Some(parent) = self.raw.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.lock_path())?;
        let deadline = Instant::now() + DRAIN_LOCK_WAIT;
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(Some(lock)),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(DRAIN_LOCK_POLL);
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
    }
}

/// Handle to the thread started by [`DetachedLog::spawn_drainer`].
#[derive(Debug)]
pub struct Drainer {
    signal: Arc<(Mutex<bool>, Condvar)>,
    handle: JoinHandle<()>,
}

impl Drainer {
    /// Stop the thread after its current drain and wait for it. Does not call
    /// [`DetachedLog::finish`]; call that once the child has exited.
    pub fn stop(self) {
        {
            let (stop, wake) = &*self.signal;
            *stop.lock().unwrap_or_else(|p| p.into_inner()) = true;
            wake.notify_all();
        }
        let _ = self.handle.join();
    }
}

/// `path` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("detached.log"));
    name.push(suffix);
    path.with_file_name(name)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn fs_to_io(error: rightkit_fs::FsError) -> io::Error {
    match error {
        rightkit_fs::FsError::Io { source, .. } => source,
        other => io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_appends_to_the_file_name() {
        assert_eq!(
            sibling(Path::new("/x/job.log"), ".raw"),
            Path::new("/x/job.log.raw")
        );
        assert_eq!(sibling(Path::new("job"), ".pos"), Path::new("job.pos"));
    }
}
