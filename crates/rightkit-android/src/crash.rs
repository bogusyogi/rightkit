//! Rust panic capture for Android (rightkit#156, bounded piece).
//!
//! [`install_panic_recorder`] chains a panic hook that writes one small text report per panic into
//! a crash directory (conventionally [`AppPaths::crash_dir`], `<files_dir>/crashes`). The app reads
//! the reports on the next launch with [`pending_reports`] and [`read_report`] and clears them with
//! [`clear_reports`]; nothing is uploaded here.
//!
//! A report holds the bridge version, a wall-clock second, the thread name and the source
//! location. Its file name carries the time in milliseconds, the process id and a per-process
//! sequence number, and the file is created exclusively, so concurrent panics never overwrite
//! one another. It never holds the panic message or payload (a message can carry user text or a
//! secret). The hook never panics and never fails the process: a failed write is dropped. At most
//! [`MAX_REPORTS`] reports are kept, the newest first.
//!
//! Not covered (deferred in #156): native signal crashes (SIGSEGV, SIGABRT from C code) and the
//! Java-side `UncaughtExceptionHandler`. Both need a native signal handler or a device pass.

use crate::error::AndroidError;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Reports kept in the crash directory; older ones are deleted when a new one is written.
pub const MAX_REPORTS: usize = 8;
/// A report file larger than this is refused by [`read_report`].
pub const MAX_REPORT_BYTES: u64 = 8 * 1024;

const PREFIX: &str = "panic-";
const SUFFIX: &str = ".txt";
/// Names tried for one report before it is dropped; a clash only means a name was taken.
const CREATE_ATTEMPTS: usize = 8;

static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Per-process sequence stamped into report names, so two panics in one millisecond differ.
static NEXT_SEQ: AtomicU32 = AtomicU32::new(0);

/// The report text for one panic. Pure; `location` is `(file, line, column)`.
pub fn format_report(
    version: &str,
    unix_secs: u64,
    thread: &str,
    location: Option<(&str, u32, u32)>,
) -> String {
    let at = match location {
        Some((file, line, column)) => format!("{file}:{line}:{column}"),
        None => "unknown".to_string(),
    };
    format!(
        "rightkit-android panic report\nbridge: {version}\ntime_unix: {unix_secs}\nthread: {thread}\nat: {at}\n"
    )
}

/// The file name of a report: `panic-<unix millis, 15 digits>-<pid>-<sequence, 8 digits>.txt`.
/// Zero padding keeps names sortable by time, and the pid and sequence keep them unique.
fn report_name(unix_ms: u64, pid: u32, seq: u32) -> String {
    format!("{PREFIX}{unix_ms:015}-{pid}-{seq:08}{SUFFIX}")
}

fn is_report_name(name: &str) -> bool {
    name.len() <= 64
        && name.starts_with(PREFIX)
        && name.ends_with(SUFFIX)
        && name[PREFIX.len()..name.len() - SUFFIX.len()]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-')
}

fn checked_dir(dir: &str) -> Result<PathBuf, AndroidError> {
    if dir.is_empty() || dir.contains('\0') || !dir.starts_with('/') {
        return Err(AndroidError::invalid("crash dir is not an absolute path"));
    }
    if dir.split('/').any(|c| c == "..") {
        return Err(AndroidError::invalid("crash dir contains `..`"));
    }
    Ok(PathBuf::from(dir))
}

/// Report file names in `dir`, newest first. A missing directory is an empty list.
fn list(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_report_name(n))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
}

/// Writes one report under a new name and returns the name. The file is created exclusively, so
/// an existing report is never overwritten: a clash retries with the next sequence number.
fn write_report(dir: &Path, report: &str, unix_ms: u64, pid: u32) -> Option<String> {
    if fs::create_dir_all(dir).is_err() {
        return None;
    }
    for _ in 0..CREATE_ATTEMPTS {
        let name = report_name(unix_ms, pid, NEXT_SEQ.fetch_add(1, Ordering::Relaxed));
        let path = dir.join(&name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if file.write_all(report.as_bytes()).is_err() {
                    let _ = fs::remove_file(&path);
                    return None;
                }
                for old in list(dir).into_iter().skip(MAX_REPORTS) {
                    let _ = fs::remove_file(dir.join(old));
                }
                return Some(name);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Installs the recording panic hook once per process; a second call is a no-op that still
/// validates `dir`. The previous hook (logcat output) keeps running after the report is written.
pub fn install_panic_recorder(dir: &str) -> Result<(), AndroidError> {
    let dir = checked_dir(dir)?;
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let thread = std::thread::current();
        let report = format_report(
            env!("CARGO_PKG_VERSION"),
            now.as_secs(),
            thread.name().unwrap_or("unnamed"),
            info.location().map(|l| (l.file(), l.line(), l.column())),
        );
        let unix_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
        write_report(&dir, &report, unix_ms, std::process::id());
        previous(info);
    }));
    Ok(())
}

/// Names of the reports waiting in `dir`, newest first.
pub fn pending_reports(dir: &str) -> Result<Vec<String>, AndroidError> {
    Ok(list(&checked_dir(dir)?))
}

/// Text of one report from [`pending_reports`]. A name that is not a report name, a missing file
/// and an oversized file are all errors; nothing outside `dir` is ever read.
pub fn read_report(dir: &str, name: &str) -> Result<String, AndroidError> {
    let dir = checked_dir(dir)?;
    if !is_report_name(name) {
        return Err(AndroidError::invalid("not a crash report name"));
    }
    let path = dir.join(name);
    let meta = fs::symlink_metadata(&path)
        .map_err(|_| AndroidError::invalid("crash report is not readable"))?;
    if !meta.is_file() || meta.len() > MAX_REPORT_BYTES {
        return Err(AndroidError::invalid(
            "crash report is not a small regular file",
        ));
    }
    fs::read_to_string(&path).map_err(|_| AndroidError::invalid("crash report is not readable"))
}

/// Deletes every report in `dir`; returns how many were removed.
pub fn clear_reports(dir: &str) -> Result<usize, AndroidError> {
    let dir = checked_dir(dir)?;
    let mut removed = 0;
    for name in list(&dir) {
        if fs::remove_file(dir.join(name)).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rk-android-crash-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn report_has_location_and_no_message() {
        let text = format_report("0.2.0", 5, "main", Some(("src/a.rs", 3, 9)));
        assert!(text.contains("at: src/a.rs:3:9"));
        assert!(text.contains("thread: main"));
        assert!(format_report("0.2.0", 5, "t", None).contains("at: unknown"));
    }

    #[test]
    fn names_are_validated() {
        assert!(is_report_name(&report_name(7, 42, 0)));
        assert!(is_report_name(&report_name(u64::MAX, u32::MAX, u32::MAX)));
        for bad in [
            "panic-1.txt/../x",
            "../panic-1-2.txt",
            "panic-a-2.txt",
            "x.txt",
            "panic-1-2.log",
        ] {
            assert!(!is_report_name(bad), "{bad}");
        }
    }

    #[test]
    fn bad_directories_are_refused() {
        for bad in ["", "relative", "/a/../b", "/a/\0"] {
            assert!(pending_reports(bad).is_err(), "{bad:?}");
            assert!(install_panic_recorder(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn writes_prunes_reads_and_clears() {
        let dir = temp("rw");
        for secs in 0..(MAX_REPORTS as u64 + 3) {
            let text = format_report("v", secs, "t", None);
            assert!(write_report(&dir, &text, secs * 1000, 1).is_some());
        }
        let d = dir.to_str().unwrap();
        let names = pending_reports(d).unwrap();
        assert_eq!(names.len(), MAX_REPORTS);
        let newest_ms = (MAX_REPORTS as u64 + 2) * 1000;
        assert!(names[0].starts_with(&format!("{PREFIX}{newest_ms:015}-1-")));
        assert!(read_report(d, &names[0]).unwrap().contains("time_unix: 10"));
        assert!(read_report(d, "../../etc/passwd").is_err());
        assert!(read_report(d, &report_name(999, 9, 0)).is_err());
        assert_eq!(clear_reports(d).unwrap(), MAX_REPORTS);
        assert!(pending_reports(d).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_report_is_refused() {
        let dir = temp("big");
        fs::create_dir_all(&dir).unwrap();
        let name = report_name(1, 1, 0);
        fs::write(dir.join(&name), vec![b'x'; MAX_REPORT_BYTES as usize + 1]).unwrap();
        assert!(read_report(dir.to_str().unwrap(), &name).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_millisecond_writes_get_distinct_files() {
        let dir = temp("seq");
        let a = write_report(&dir, "first", 5_000, 7).unwrap();
        let b = write_report(&dir, "second", 5_000, 7).unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read_to_string(dir.join(&a)).unwrap(), "first");
        assert_eq!(fs::read_to_string(dir.join(&b)).unwrap(), "second");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_report_is_never_overwritten() {
        let dir = temp("excl");
        fs::create_dir_all(&dir).unwrap();
        // Occupy the name the next write would pick first.
        let taken = report_name(5_000, 7, NEXT_SEQ.load(Ordering::Relaxed));
        fs::write(dir.join(&taken), "keep").unwrap();
        let written = write_report(&dir, "new", 5_000, 7).unwrap();
        assert_ne!(written, taken);
        assert_eq!(fs::read_to_string(dir.join(&taken)).unwrap(), "keep");
        assert_eq!(fs::read_to_string(dir.join(&written)).unwrap(), "new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_lists_nothing() {
        assert!(pending_reports("/definitely/not/here/rk-crash")
            .unwrap()
            .is_empty());
    }
}
