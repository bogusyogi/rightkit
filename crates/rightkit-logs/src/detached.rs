//! Durable capture of a detached child's stderr.
//!
//! [`capture_stderr`](crate::capture_stderr) needs a live parent that drains a pipe. A detached
//! job (one that outlives the process that started it) has no such parent, and a pipe whose
//! reader exits kills or blocks the child. [`DetachedLog`] hands the child a **file** instead:
//! the child appends raw stderr straight to a private raw file, and whoever is alive later
//! (the parent while it runs, or the next launch after a restart) drains that file into a
//! [`RotatingTextWriter`], scrubbed, prefixed and rotated. A read position stored beside the raw
//! file lets a restarted process continue where the last drain stopped.
//!
//! A line longer than 64 KiB is never written, in whole or in part: it is replaced by one
//! `[redacted: long line omitted]` line (see [`DetachedLog::drain`]). When a drained raw file is
//! emptied, the first line written after it is replaced by one marker line too, because bytes the
//! child appended while the file was emptied are lost and the next bytes may be the tail of a
//! line whose start was lost (see [`DetachedLog::with_compact_at`]).

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

/// Longest drained line, in bytes before its line break. A longer line is omitted whole: nothing
/// of it is written but the marker, because a secret or credential that straddles a piece
/// boundary cannot be matched, and no piece of it is known to be safe.
const MAX_DRAINED_LINE_BYTES: u64 = 64 * 1024;
/// Written once, in place of a line longer than [`MAX_DRAINED_LINE_BYTES`].
const OMITTED_LINE_MARKER: &str = "[redacted: long line omitted]";
/// Written once, in place of the first line after the raw file was emptied (or shrank without
/// explanation): that line may be the tail of a line whose start was dropped.
const COMPACTED_LINE_MARKER: &str = "[redacted: line after raw-file compaction omitted]";
/// Raw size at which a fully drained raw file is emptied, unless configured otherwise.
const DEFAULT_COMPACT_AT: u64 = 4 * 1024 * 1024;
/// Where the next unread raw byte stands, saved with the read position (`<raw>.pos`) as
/// `<offset>`, `<offset> omit` or `<offset> guard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resume {
    /// At the start of a line.
    Line,
    /// Inside an omitted line whose marker is already written: discard up to its newline.
    Omit,
    /// After the raw file was emptied, or shrank without explanation: the next bytes may be the
    /// tail of a line whose start was lost, so the first line is replaced by one marker line.
    Guard,
}

/// Test-only point at which a compaction stops with an error, as a crash would.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactStop {
    BeforeTruncate,
    AfterTruncate,
}

/// Test-only closure run inside the window between the length check and the truncation.
#[cfg(test)]
struct CompactHook(Box<dyn Fn() + Send + Sync>);

#[cfg(test)]
impl std::fmt::Debug for CompactHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactHook")
    }
}

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

/// Durable stderr capture for a detached child. See the module docs.
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
    #[cfg(test)]
    compact_stop: Option<CompactStop>,
    #[cfg(test)]
    compact_hook: Option<CompactHook>,
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
            #[cfg(test)]
            compact_stop: None,
            #[cfg(test)]
            compact_hook: None,
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

    /// Remove these known secrets from every drained line, before the pattern scrub. Drained text
    /// is handled one line at a time, so a secret that holds a line break is also removed
    /// fragment by fragment: every non-empty piece of it between CR and LF characters (and the
    /// encoded forms of each piece) is removed wherever it appears in a line, and the encoded
    /// forms of the whole value are removed as before. A one-character piece removes that
    /// character everywhere, as any one-character secret does. A line longer than 64 KiB is not
    /// scanned at all: it is omitted whole.
    pub fn with_known_secrets(mut self, secrets: KnownSecrets) -> Self {
        self.secrets = secrets.for_lines();
        self
    }

    /// Empty the raw file once a drain has consumed all of it and it holds at least `bytes`.
    /// Default 4 MiB; `0` never empties it, so a chatty child grows it until [`finish`](Self::finish).
    ///
    /// The raw file is bounded only when it is drained. The child keeps its handle open, so
    /// emptying races the child's writes, and bytes it appends between the final length check
    /// and the truncation are lost. What is guaranteed: before the file is truncated, the saved
    /// position is replaced (durably, by atomic rename) with `0 guard`, or `0 omit` when the
    /// drain stopped inside an omitted line. Whatever happens next, including a crash before or
    /// after the truncation, the first line read from the emptied file is not written: one
    /// `[redacted: line after raw-file compaction omitted]` line stands for it (or, inside an
    /// omitted line, the rest of that line is discarded), so a fragment whose start was lost is
    /// never written. Lines after that first one are written normally. The cost is that one line
    /// per compaction is replaced by the marker, whether or not bytes were lost. Lost bytes are
    /// never reported; the marker is the only trace.
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
        match fs::metadata(&self.raw) {
            Ok(meta) if meta.len() == 0 => {
                // Emptied by a compaction whose old child may still write: keep its guard.
                match self.read_cursor_state().1 {
                    Resume::Line => {
                        let _ = fs::remove_file(self.cursor_path());
                    }
                    mode => {
                        let _ = self.write_cursor(0, mode);
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {
                let _ = fs::remove_file(self.cursor_path());
            }
        }
        rightkit_fs::open_private_append(&self.raw).map_err(fs_to_io)
    }

    /// Drain every complete line written so far into the sink. An unfinished last line stays in
    /// the raw file until its newline arrives, unless it is already longer than 64 KiB (below).
    ///
    /// **A line longer than 64 KiB is omitted whole.** At the first byte past 64 KiB of one line,
    /// with no newline yet, one `[redacted: long line omitted]` line (with the prefix) is written
    /// in its place, no earlier part of the line is written, and the rest of the line is read in
    /// 64 KiB pieces and discarded up to its newline. This holds with or without known secrets,
    /// so no piece of a secret or credential that spans the 64 KiB point is ever written. The
    /// state "inside an omitted line" is saved with the read position (`<raw>.pos`), so the
    /// marker is written once even across drains and restarts. Lines of 64 KiB or less are
    /// unchanged. A missing raw file is an empty drain. Safe to call
    /// while the child runs, from several threads, and from several processes (one drains, the
    /// others wait up to 500 ms, then skip). A write failure stops the drain at that line, keeps
    /// the position before it, and returns the error, so nothing is dropped and the next drain
    /// retries.
    pub fn drain(&self) -> io::Result<DrainReport> {
        self.drain_inner(false)
    }

    /// Drain everything, including an unfinished last line (an unfinished line over 64 KiB
    /// becomes the omission marker alone), then remove the raw file and its
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
                        .wait_timeout_while(stopped, interval, |stopped| !*stopped)
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
        // An empty snapshot has nothing to consume. Leave the saved guard/omission state
        // untouched: a child may append a continuation after this check, for the next drain.
        // Metadata failures still take the ordinary lock/open path and its error handling.
        if !finish && fs::metadata(&self.raw).is_ok_and(|meta| meta.is_file() && meta.len() == 0) {
            return Ok(DrainReport::default());
        }
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
        let (start, saved_mode) = self.read_cursor_state();
        let (start, mut mode) = if start > len {
            // The raw file is shorter than the saved position: it was emptied or replaced
            // without a record. Its first bytes may continue a line we cannot see the start of.
            let mode = if saved_mode == Resume::Omit {
                Resume::Omit
            } else {
                Resume::Guard
            };
            (0, mode)
        } else {
            (start, saved_mode)
        };
        file.seek(SeekFrom::Start(start))?;
        // Bound the read to the snapshot so concurrent appends are left for the next drain.
        let mut reader = io::BufReader::new(file.take(len - start));
        let mut offset = start;
        let started_mode = mode;
        let mut buf = Vec::new();
        let mut failure = None;
        loop {
            buf.clear();
            let read = io::BufRead::read_until(
                &mut (&mut reader).take(MAX_DRAINED_LINE_BYTES + 1),
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
            if mode == Resume::Omit {
                // The rest of an omitted line: read and discarded, never written.
                offset += n;
                report.bytes += n;
                mode = if complete { Resume::Line } else { Resume::Omit };
                continue;
            }
            if mode == Resume::Guard {
                // The first bytes after the raw file was emptied: replace this line by a marker.
                let marker = format!("{}{COMPACTED_LINE_MARKER}", self.prefix);
                if let Err(error) = self.sink.write_line(&marker) {
                    failure = Some(error);
                    break;
                }
                offset += n;
                report.lines += 1;
                report.bytes += n;
                mode = if complete { Resume::Line } else { Resume::Omit };
                continue;
            }
            if !complete && n > MAX_DRAINED_LINE_BYTES {
                // More than 64 KiB of one line and no newline: omit the whole line. Nothing of it
                // has been written, and none of the 64 KiB read so far is.
                let marker = format!("{}{OMITTED_LINE_MARKER}", self.prefix);
                if let Err(error) = self.sink.write_line(&marker) {
                    failure = Some(error);
                    break;
                }
                offset += n;
                report.lines += 1;
                report.bytes += n;
                mode = Resume::Omit;
                continue;
            }
            if !complete && !finish {
                // An unfinished last line: wait for its newline.
                break;
            }
            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                buf.pop();
            }
            let text = String::from_utf8_lossy(&buf);
            let known_free = self.secrets.redact(&text);
            // An unterminated line may end inside a credential, so the counted scrub (which
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
        if offset != start || mode != started_mode {
            if let Err(error) = self.write_cursor(offset, mode) {
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
            report.compacted = self.compact(offset, mode)?;
        }
        Ok(report)
    }

    /// Empty the raw file when it still ends where the drain stopped. The new position (`0 guard`,
    /// or `0 omit` inside an omitted line) is saved durably BEFORE the truncation, so no crash
    /// point leaves a position that treats bytes appended after the truncation as a line start.
    /// If the file grew after the length check, the truncation drops those bytes; the saved
    /// guard makes the first line read afterwards a marker.
    fn compact(&self, consumed: u64, mode: Resume) -> io::Result<bool> {
        let file = OpenOptions::new().write(true).open(&self.raw)?;
        if file.metadata()?.len() != consumed {
            return Ok(false);
        }
        let after = if mode == Resume::Omit {
            Resume::Omit
        } else {
            Resume::Guard
        };
        self.write_cursor(0, after)?;
        #[cfg(test)]
        {
            if let Some(hook) = &self.compact_hook {
                (hook.0)();
            }
            if self.compact_stop == Some(CompactStop::BeforeTruncate) {
                return Err(io::Error::other("injected stop before truncate"));
            }
        }
        if let Err(error) = file.set_len(0) {
            let _ = self.write_cursor(consumed, mode);
            return Err(error);
        }
        #[cfg(test)]
        {
            if self.compact_stop == Some(CompactStop::AfterTruncate) {
                return Err(io::Error::other("injected stop after truncate"));
            }
        }
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
        self.read_cursor_state().0
    }

    /// The saved read position and what it stands at (`<offset>`, `<offset> omit`,
    /// `<offset> guard`).
    fn read_cursor_state(&self) -> (u64, Resume) {
        let Ok(text) = fs::read_to_string(self.cursor_path()) else {
            return (0, Resume::Line);
        };
        let mut parts = text.split_whitespace();
        match parts.next().and_then(|offset| offset.parse().ok()) {
            Some(offset) => {
                let mode = match parts.next() {
                    Some("omit") => Resume::Omit,
                    Some("guard") => Resume::Guard,
                    _ => Resume::Line,
                };
                (offset, mode)
            }
            None => (0, Resume::Line),
        }
    }

    /// Saves the position by atomic replacement (written, fsynced, renamed).
    fn write_cursor(&self, offset: u64, mode: Resume) -> io::Result<()> {
        let text = match mode {
            Resume::Line => offset.to_string(),
            Resume::Omit => format!("{offset} omit"),
            Resume::Guard => format!("{offset} guard"),
        };
        crate::atomic::write_atomic(&self.cursor_path(), text.as_bytes())
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

    use std::io::Write as _;

    fn sink(dir: &Path) -> Arc<RotatingTextWriter> {
        Arc::new(RotatingTextWriter::new(dir.join("job.log"), 1 << 20).with_generations(3))
    }

    fn text(dir: &Path) -> String {
        fs::read_to_string(dir.join("job.log")).unwrap_or_default()
    }

    fn cursor(dir: &Path) -> String {
        fs::read_to_string(dir.join("job.log.raw.pos"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn append(path: &Path, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    /// A log whose child wrote 70,000 bytes of one unterminated line, drained into the
    /// omitted state and compacted; the compaction stops at `stop`, as a crash would.
    fn omitted_then_compacted(dir: &Path, stop: CompactStop) -> DetachedLog {
        let mut log = DetachedLog::new(sink(dir)).with_compact_at(1000);
        log.compact_stop = Some(stop);
        let mut child = log.stderr_file().unwrap();
        child.write_all(&vec![b'a'; 70_000]).unwrap();
        assert!(log.drain().is_err(), "the injected stop fails the drain");
        assert_eq!(text(dir), "[redacted: long line omitted]\n");
        log
    }

    #[test]
    fn crash_after_truncate_keeps_the_omitted_line_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let log = omitted_then_compacted(dir.path(), CompactStop::AfterTruncate);
        assert_eq!(fs::metadata(log.raw_path()).unwrap().len(), 0);
        assert_eq!(cursor(dir.path()), "0 omit");
        // The old child finishes its line and writes on; a new process drains.
        append(log.raw_path(), b"SECRETSUFFIX\nnext\n");
        let restarted = DetachedLog::new(sink(dir.path()));
        assert_eq!(restarted.drain().unwrap().lines, 1);
        assert_eq!(text(dir.path()), "[redacted: long line omitted]\nnext\n");
    }

    #[test]
    fn crash_before_truncate_keeps_the_omitted_line_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let log = omitted_then_compacted(dir.path(), CompactStop::BeforeTruncate);
        assert_eq!(fs::metadata(log.raw_path()).unwrap().len(), 70_000);
        assert_eq!(cursor(dir.path()), "0 omit");
        append(log.raw_path(), b"SECRETSUFFIX\nnext\n");
        let restarted = DetachedLog::new(sink(dir.path()));
        assert_eq!(restarted.drain().unwrap().lines, 1);
        assert_eq!(text(dir.path()), "[redacted: long line omitted]\nnext\n");
    }

    #[test]
    fn a_file_that_regrows_past_the_old_position_before_restart_leaks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let log = omitted_then_compacted(dir.path(), CompactStop::AfterTruncate);
        // The old position was 70,000; the file now holds more than that.
        let mut grown = vec![b'b'; 150_000];
        grown.extend_from_slice(b"\nnext\n");
        append(log.raw_path(), &grown);
        let restarted = DetachedLog::new(sink(dir.path()));
        restarted.drain().unwrap();
        assert_eq!(text(dir.path()), "[redacted: long line omitted]\nnext\n");
    }

    #[test]
    fn bytes_appended_during_compaction_are_dropped_and_their_tail_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = DetachedLog::new(sink(dir.path())).with_compact_at(1);
        let raw = log.raw_path().to_path_buf();
        // The child writes a credential prefix inside the window; the truncation drops it.
        log.compact_hook = Some(CompactHook(Box::new(move || append(&raw, b"sk-abc"))));
        let mut child = log.stderr_file().unwrap();
        child.write_all(b"one\n").unwrap();
        assert!(log.drain().unwrap().compacted);
        assert_eq!(fs::metadata(log.raw_path()).unwrap().len(), 0);
        assert_eq!(cursor(dir.path()), "0 guard");
        // The rest of the credential arrives after the truncation, then ordinary lines.
        child.write_all(b"defghijklmnop\nnext\n").unwrap();
        log.compact_hook = None;
        assert_eq!(log.drain().unwrap().lines, 2);
        assert_eq!(
            text(dir.path()),
            "one\n[redacted: line after raw-file compaction omitted]\nnext\n"
        );
    }

    #[test]
    fn crash_after_a_compaction_with_no_omission_keeps_the_guard() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = DetachedLog::new(sink(dir.path())).with_compact_at(1);
        log.compact_stop = Some(CompactStop::AfterTruncate);
        let mut child = log.stderr_file().unwrap();
        child.write_all(b"one\n").unwrap();
        assert!(log.drain().is_err());
        assert_eq!(cursor(dir.path()), "0 guard");
        child.write_all(b"defghi\nnext\n").unwrap();
        let restarted = DetachedLog::new(sink(dir.path()));
        restarted.drain().unwrap();
        assert_eq!(
            text(dir.path()),
            "one\n[redacted: line after raw-file compaction omitted]\nnext\n"
        );
    }

    #[test]
    fn a_position_beyond_the_raw_file_is_guarded_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let log = DetachedLog::new(sink(dir.path()));
        let mut child = log.stderr_file().unwrap();
        child.write_all(b"SECRETSUFFIX\nnext\n").unwrap();
        fs::write(dir.path().join("job.log.raw.pos"), "100000").unwrap();
        log.drain().unwrap();
        assert_eq!(
            text(dir.path()),
            "[redacted: line after raw-file compaction omitted]\nnext\n"
        );
        // A stale omitting position stays omitting.
        let dir = tempfile::tempdir().unwrap();
        let log = DetachedLog::new(sink(dir.path()));
        let mut child = log.stderr_file().unwrap();
        child.write_all(b"SECRETSUFFIX\nnext\n").unwrap();
        fs::write(dir.path().join("job.log.raw.pos"), "100000 omit").unwrap();
        log.drain().unwrap();
        assert_eq!(text(dir.path()), "next\n");
    }

    #[test]
    fn a_new_child_on_an_emptied_file_keeps_the_guard() {
        let dir = tempfile::tempdir().unwrap();
        let log = DetachedLog::new(sink(dir.path()));
        fs::write(log.raw_path(), b"").unwrap();
        fs::write(dir.path().join("job.log.raw.pos"), "0 guard").unwrap();
        let _child = log.stderr_file().unwrap();
        assert_eq!(cursor(dir.path()), "0 guard");
        // A plain position on an empty file is cleared as before.
        fs::write(dir.path().join("job.log.raw.pos"), "500").unwrap();
        let _again = log.stderr_file().unwrap();
        assert_eq!(cursor(dir.path()), "");
    }

    #[test]
    fn cursor_text_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let log = DetachedLog::new(sink(dir.path()));
        for (offset, mode) in [(0, Resume::Line), (7, Resume::Omit), (9, Resume::Guard)] {
            log.write_cursor(offset, mode).unwrap();
            assert_eq!(log.read_cursor_state(), (offset, mode));
        }
        fs::write(log.cursor_path(), "junk").unwrap();
        assert_eq!(log.read_cursor_state(), (0, Resume::Line));
    }
}
