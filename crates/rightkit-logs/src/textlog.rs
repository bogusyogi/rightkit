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

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rightkit-logs-text-{label}-{}-{}",
            std::process::id(),
            crate::now_ms()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn default_keeps_one_generation() {
        let d = dir("one");
        let w = RotatingTextWriter::new(d.join("a.log"), 10);
        for i in 0..4 {
            w.write_line(&format!("line-{i}-xxxxxxxx")).unwrap();
        }
        assert!(d.join("a.log.1").exists());
        assert!(!d.join("a.log.2").exists());
        assert_eq!(
            fs::read_to_string(d.join("a.log.1")).unwrap(),
            "line-2-xxxxxxxx\n"
        );
        assert_eq!(
            fs::read_to_string(d.join("a.log")).unwrap(),
            "line-3-xxxxxxxx\n"
        );
    }

    #[test]
    fn keeps_n_generations_and_drops_the_oldest() {
        let d = dir("n");
        let w = RotatingTextWriter::new(d.join("a.log"), 10).with_generations(3);
        for i in 0..6 {
            w.write_line(&format!("line-{i}-xxxxxxxx")).unwrap();
        }
        let read = |n: &str| fs::read_to_string(d.join(n)).unwrap();
        assert_eq!(read("a.log"), "line-5-xxxxxxxx\n");
        assert_eq!(read("a.log.1"), "line-4-xxxxxxxx\n");
        assert_eq!(read("a.log.2"), "line-3-xxxxxxxx\n");
        assert_eq!(read("a.log.3"), "line-2-xxxxxxxx\n");
        assert!(!d.join("a.log.4").exists());
    }

    #[test]
    fn zero_size_disables_rotation_and_zero_generations_is_one() {
        let d = dir("zero");
        let w = RotatingTextWriter::new(d.join("a.log"), 0).with_generations(0);
        for _ in 0..5 {
            w.write_line("0123456789").unwrap();
        }
        assert!(!d.join("a.log.1").exists());
        let w = RotatingTextWriter::new(d.join("b.log"), 5).with_generations(0);
        for _ in 0..4 {
            w.write_line("0123456789").unwrap();
        }
        assert!(d.join("b.log.1").exists());
        assert!(!d.join("b.log.2").exists());
    }

    #[test]
    fn gaps_in_the_chain_are_tolerated() {
        let d = dir("gap");
        fs::write(d.join("a.log.2"), "old\n").unwrap();
        let w = RotatingTextWriter::new(d.join("a.log"), 5).with_generations(4);
        w.write_line("0123456789").unwrap();
        w.write_line("next").unwrap();
        assert_eq!(fs::read_to_string(d.join("a.log.3")).unwrap(), "old\n");
        assert_eq!(
            fs::read_to_string(d.join("a.log.1")).unwrap(),
            "0123456789\n"
        );
    }

    #[test]
    fn failed_rotation_is_counted_and_capped() {
        let d = dir("fail");
        let path = d.join("a.log");
        // A directory at `.1` makes removing the predecessor fail.
        fs::create_dir_all(d.join("a.log.1").join("keep")).unwrap();
        let w = RotatingTextWriter::new(&path, 10);
        w.write_line("0123456789").unwrap();
        w.write_line("0123456789").unwrap();
        assert!(w.write_line("0123456789").is_err());
        let status = w.status();
        assert!(status.failed_rotations >= 1);
        assert_eq!(status.failed_appends, 1);
    }

    #[test]
    fn capture_prefixes_scrubs_and_rotates() {
        let d = dir("cap");
        let w = Arc::new(RotatingTextWriter::new(d.join("daemon.log"), 60).with_generations(3));
        let input = b"starting in /Users/ada/project\nmail ada@example.com failed\r\nlast".to_vec();
        let cap = capture_stderr(io::Cursor::new(input), w.clone(), "[daemon] ").unwrap();
        assert_eq!(cap.join(), 3);
        let mut all = String::new();
        for name in ["daemon.log", "daemon.log.1", "daemon.log.2", "daemon.log.3"] {
            all.push_str(&fs::read_to_string(d.join(name)).unwrap_or_default());
        }
        assert!(all.contains("[daemon] "));
        assert!(!all.contains("ada@example.com"), "{all}");
        assert!(!all.contains("/Users/ada"), "{all}");
        assert!(all.contains("last"));
        assert!(d.join("daemon.log.1").exists());
    }

    fn read(d: &Path, n: &str) -> String {
        fs::read_to_string(d.join(n)).unwrap()
    }

    #[test]
    fn stem_numbered_names_rotated_files() {
        let d = dir("stem");
        let w = RotatingTextWriter::new(d.join("app.log"), 10)
            .with_generations(2)
            .with_naming(RotatedNaming::StemNumbered);
        for i in 0..5 {
            w.write_line(&format!("line-{i}-xxxxxxxx")).unwrap();
        }
        assert_eq!(read(&d, "app.log"), "line-4-xxxxxxxx\n");
        assert_eq!(read(&d, "app.1.log"), "line-3-xxxxxxxx\n");
        assert_eq!(read(&d, "app.2.log"), "line-2-xxxxxxxx\n");
        assert!(!d.join("app.3.log").exists());
        assert!(!d.join("app.log.1").exists());
    }

    #[test]
    fn stem_numbered_without_extension() {
        let p = Path::new("/x/app");
        assert_eq!(
            rotate::named_generation_path(p, 2, "app.log", RotatedNaming::StemNumbered),
            Path::new("/x/app.2")
        );
    }

    #[test]
    fn rotate_on_open_keeps_previous_logs_and_starts_empty_file() {
        let d = dir("open");
        let open = || {
            RotatingTextWriter::new(d.join("x.log"), 0)
                .with_generations(2)
                .with_naming(RotatedNaming::StemNumbered)
                .with_rotate_on_open(true)
        };
        for run in 1..=3 {
            let w = open();
            w.write_line(&format!("run-{run}")).unwrap();
            w.write_line(&format!("run-{run}-again")).unwrap();
            // Once per writer: the second write did not rotate again.
            assert_eq!(read(&d, "x.log"), format!("run-{run}\nrun-{run}-again\n"));
        }
        assert_eq!(read(&d, "x.log"), "run-3\nrun-3-again\n");
        assert_eq!(read(&d, "x.1.log"), "run-2\nrun-2-again\n");
        assert_eq!(read(&d, "x.2.log"), "run-1\nrun-1-again\n");
        // A fourth open drops the oldest.
        let w = open();
        w.write_line("run-4").unwrap();
        assert_eq!(read(&d, "x.log"), "run-4\n");
        assert_eq!(read(&d, "x.1.log"), "run-3\nrun-3-again\n");
        assert_eq!(read(&d, "x.2.log"), "run-2\nrun-2-again\n");
        assert!(!d.join("x.3.log").exists());
    }

    #[test]
    fn rotate_on_open_with_default_naming_and_empty_file() {
        let d = dir("open-default");
        fs::write(d.join("a.log"), "").unwrap();
        let w = RotatingTextWriter::new(d.join("a.log"), 0).with_rotate_on_open(true);
        w.write_line("first").unwrap();
        // An empty previous file is not rolled into a generation.
        assert!(!d.join("a.log.1").exists());
        let w = RotatingTextWriter::new(d.join("a.log"), 0).with_rotate_on_open(true);
        w.write_line("second").unwrap();
        assert_eq!(read(&d, "a.log.1"), "first\n");
        assert_eq!(read(&d, "a.log"), "second\n");
    }

    #[test]
    fn rotate_now_rolls_once_and_prevents_open_rotation() {
        let d = dir("now");
        fs::write(d.join("a.log"), "old\n").unwrap();
        let w = RotatingTextWriter::new(d.join("a.log"), 0).with_rotate_on_open(true);
        w.rotate_now().unwrap();
        assert!(!d.join("a.log").exists());
        w.write_line("new").unwrap();
        w.write_line("newer").unwrap();
        assert_eq!(read(&d, "a.log.1"), "old\n");
        assert_eq!(read(&d, "a.log"), "new\nnewer\n");
    }

    #[test]
    fn rotation_into_an_unusable_directory_is_an_error_not_a_panic() {
        let d = dir("unusable");
        fs::write(d.join("a.log"), "old\n").unwrap();
        // A non-empty directory at generation 1 makes the roll fail.
        fs::create_dir_all(d.join("a.log.1").join("keep")).unwrap();
        let w = RotatingTextWriter::new(d.join("a.log"), 0).with_rotate_on_open(true);
        assert!(w.rotate_now().is_err());
        assert_eq!(w.status().failed_rotations, 1);
        // A failed open-time rotation still appends and is counted.
        let w = RotatingTextWriter::new(d.join("a.log"), 0).with_rotate_on_open(true);
        w.write_line("kept").unwrap();
        assert_eq!(w.status().failed_rotations, 1);
        assert_eq!(read(&d, "a.log"), "old\nkept\n");
    }
}
