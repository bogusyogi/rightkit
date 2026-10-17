//! Android native-crash and ANR records from the OS exit history.
//!
//! An app process cannot read `/data/tombstones`, and a signal handler inside the cdylib would
//! need `unsafe` and runs in a corrupt process. Android already records why every previous
//! process of the app ended: `ActivityManager.getHistoricalProcessExitReasons` (API 30+) returns
//! one `ApplicationExitInfo` per death, with `REASON_CRASH` (4), `REASON_CRASH_NATIVE` (5) and
//! `REASON_ANR` (6), and the tombstone or ANR trace as a stream. The OS detects the ANR, so no
//! thread of ours watches the main thread and nothing can block it.
//!
//! The Kotlin shell reads those entries at next launch and passes each one here as an
//! [`AndroidExit`]. [`record_android_exits`] records the crash-like ones through
//! [`crate::record_report`] (owner scrub, known-secret redaction, retention and sink all apply)
//! and keeps a cursor so a death is recorded once however often the app starts. The trace is
//! reduced to header and backtrace text first: stack contents, memory dumps, registers, the
//! memory map, open files and log excerpts are dropped before anything is stored, because they
//! can hold app content.
//!
//! The module has no OS gate so its logic is tested on every host; only an Android shell has
//! exit entries to feed it. Nothing here has run on a device (docs/design/android-minimum.md,
//! row 16).

use crate::hook::{record_report, CrashConfig};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Why the OS says a process ended, from `ApplicationExitInfo.getReason()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidExitReason {
    /// `REASON_CRASH` (4): an uncaught Java or Kotlin exception.
    JavaCrash,
    /// `REASON_CRASH_NATIVE` (5): a native signal; the trace is a tombstone.
    NativeCrash,
    /// `REASON_ANR` (6): application not responding; the trace holds the thread stacks.
    Anr,
    /// Any other reason (user stop, low memory, update, ...). Not crash-like; never recorded.
    Other,
}

impl AndroidExitReason {
    /// Map the `ApplicationExitInfo` reason code.
    pub fn from_code(code: i32) -> Self {
        match code {
            4 => Self::JavaCrash,
            5 => Self::NativeCrash,
            6 => Self::Anr,
            _ => Self::Other,
        }
    }

    /// Fixed `kind` written to the report.
    pub fn kind(self) -> &'static str {
        match self {
            Self::JavaCrash => "java_crash",
            Self::NativeCrash => "native_crash",
            Self::Anr => "anr",
            Self::Other => "exit",
        }
    }

    /// Whether this reason produces a report.
    pub fn is_crash_like(self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// One previous process death, as the Kotlin shell read it from the OS.
#[derive(Debug, Clone)]
pub struct AndroidExit {
    pub reason: AndroidExitReason,
    /// Process id of the dead process.
    pub pid: u32,
    /// `getTimestamp()`: milliseconds since the Unix epoch. Orders exits and drives the cursor.
    pub timestamp_ms: u64,
    /// `getProcessName()`, for example `com.example.app:engine`.
    pub process_name: String,
    /// `getDescription()`, a short OS-written string. Scrubbed like any report text.
    pub description: String,
    /// `getTraceInputStream()` read as text: a tombstone, or the ANR thread dump.
    pub trace: Option<String>,
}

const CURSOR_FILE: &str = "android_exit_cursor";
/// Largest trace read into a report before filtering; the report itself truncates further.
const MAX_TRACE_BYTES: usize = 256 * 1024;

/// Section header prefixes (lower case, at column 0) whose content is dropped from a tombstone.
const DROPPED_SECTIONS: &[&str] = &[
    "stack:",
    "memory near",
    "memory map",
    "open files",
    "code around",
    "logcat",
    "--- log",
    "log:",
];

/// A register dump line: pairs of a register name and a 8 to 16 digit hex value.
fn is_register_line(line: &str) -> bool {
    let mut tokens = line.split_whitespace();
    let mut pairs = 0;
    while let Some(name) = tokens.next() {
        let Some(value) = tokens.next() else {
            return false;
        };
        let name_ok = name.chars().all(|c| c.is_ascii_alphanumeric());
        let value_ok =
            (8..=16).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit());
        if !(name_ok && value_ok) {
            return false;
        }
        pairs += 1;
    }
    pairs >= 1
}

/// Keep the header and backtrace parts of a tombstone or ANR trace and drop memory-bearing
/// sections: stack contents, memory near registers, the memory map, open files, log excerpts and
/// register dumps. Text that is not a tombstone passes through (an ANR trace has no such
/// sections). A dropped section ends at the next line that starts in column 0 and is not a
/// dropped header; the input is capped at 256 KiB first.
pub fn reduce_trace(trace: &str) -> String {
    let mut text = trace;
    if text.len() > MAX_TRACE_BYTES {
        let mut cut = MAX_TRACE_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text = &text[..cut];
    }
    let mut out = String::new();
    let mut dropping = false;
    for line in text.lines() {
        let at_column_zero = !line.starts_with(char::is_whitespace) && !line.is_empty();
        if at_column_zero {
            let lower = line.to_ascii_lowercase();
            dropping = DROPPED_SECTIONS.iter().any(|p| lower.starts_with(p));
        }
        if dropping || is_register_line(line) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn read_cursor(dir: &Path) -> u64 {
    std::fs::read_to_string(dir.join(CURSOR_FILE))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn write_cursor(dir: &Path, value: u64) {
    let options = rightkit_fs::AtomicWriteOptions {
        private: true,
        ..rightkit_fs::AtomicWriteOptions::default()
    };
    let _ = rightkit_fs::atomic_write_with_options(&dir.join(CURSOR_FILE), options, |file| {
        file.write_all(value.to_string().as_bytes())
    });
}

/// The crash-like exits newer than `cursor_ms`, oldest first. Pure; used by
/// [`record_android_exits`] and testable on any host.
pub fn unseen_exits(exits: &[AndroidExit], cursor_ms: u64) -> Vec<&AndroidExit> {
    let mut found: Vec<&AndroidExit> = exits
        .iter()
        .filter(|e| e.reason.is_crash_like() && e.timestamp_ms > cursor_ms)
        .collect();
    found.sort_by_key(|e| e.timestamp_ms);
    found
}

/// Record each crash-like exit in `exits` that has not been recorded before, and return the
/// report paths written. The cursor (`android_exit_cursor` in `config.crash_dir`) holds the
/// newest exit timestamp handled, so a second call with the same entries records nothing. Call
/// it once at launch, before [`crate::enqueue_pending`], with the list from
/// `getHistoricalProcessExitReasons`. When two exits of the same reason land in one batch the
/// later ones get the kind suffix `_2`, `_3`, so their report files do not collide. Returns an
/// empty list when recording is off (see [`crate::set_recording_enabled`]); the cursor then does
/// not move.
pub fn record_android_exits(config: &CrashConfig, exits: &[AndroidExit]) -> Vec<PathBuf> {
    if !crate::recording_enabled() {
        return Vec::new();
    }
    let cursor = read_cursor(&config.crash_dir);
    let mut paths = Vec::new();
    let mut newest = cursor;
    let mut seen_kinds: Vec<(&'static str, u32)> = Vec::new();
    for exit in unseen_exits(exits, cursor) {
        let base = exit.reason.kind();
        let count = match seen_kinds.iter_mut().find(|(k, _)| *k == base) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                seen_kinds.push((base, 1));
                1
            }
        };
        let kind = if count > 1 {
            format!("{base}_{count}")
        } else {
            base.to_string()
        };
        let message = format!(
            "android exit: {} (pid {}, process {}): {}",
            base, exit.pid, exit.process_name, exit.description
        );
        let trace = exit.trace.as_deref().map(reduce_trace).unwrap_or_default();
        if let Some(path) =
            record_report(config, &kind, &message, None, "android_exit_history", &trace)
        {
            paths.push(path);
        }
        newest = newest.max(exit.timestamp_ms);
    }
    if newest > cursor {
        let _ = std::fs::create_dir_all(&config.crash_dir);
        write_cursor(&config.crash_dir, newest);
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(reason: i32, ts: u64, trace: Option<&str>) -> AndroidExit {
        AndroidExit {
            reason: AndroidExitReason::from_code(reason),
            pid: 4242,
            timestamp_ms: ts,
            process_name: "com.example.app".into(),
            description: "signal 11".into(),
            trace: trace.map(str::to_string),
        }
    }

    #[test]
    fn reason_codes_map_and_only_crash_like_reasons_are_kept() {
        assert_eq!(AndroidExitReason::from_code(4).kind(), "java_crash");
        assert_eq!(AndroidExitReason::from_code(5).kind(), "native_crash");
        assert_eq!(AndroidExitReason::from_code(6).kind(), "anr");
        assert!(!AndroidExitReason::from_code(10).is_crash_like());
        let exits = [exit(5, 30, None), exit(10, 40, None), exit(6, 20, None)];
        let unseen = unseen_exits(&exits, 20);
        assert_eq!(unseen.len(), 1);
        assert_eq!(unseen[0].timestamp_ms, 30);
    }

    #[test]
    fn a_tombstone_keeps_the_backtrace_and_drops_memory_bearing_sections() {
        let tombstone = "\
*** *** ***
pid: 4242, tid: 4250, name: worker  >>> com.example.app <<<
signal 11 (SIGSEGV), code 1 (SEGV_MAPERR), fault addr 0x0
    x0  0000007ff0001234  x1  0000007ff0005678
backtrace:
      #00 pc 0000000000012345  /data/app/lib/arm64/libapp.so (crash+16)
stack:
         0000007fe0000000  6162636465666768
memory near x0:
    0000007ff0001230 73656372657420 secret
memory map (12 entries):
    0000007ff0000000-0000007ff0002000 rw- /data/data/x
open files:
    fd 3: /data/user/0/com.example.app/files/notes.db
";
        let reduced = reduce_trace(tombstone);
        assert!(reduced.contains("signal 11"));
        assert!(reduced.contains("backtrace:"));
        assert!(reduced.contains("libapp.so (crash+16)"));
        assert!(!reduced.contains("x0  0000007ff0001234"));
        assert!(!reduced.contains("6162636465666768"));
        assert!(!reduced.contains("secret"));
        assert!(!reduced.contains("notes.db"));
        assert!(!reduced.contains("memory map"));
    }

    #[test]
    fn an_anr_trace_passes_through() {
        let trace = "\"main\" prio=5 tid=1 Blocked\n  at com.example.Main.run(Main.java:10)\n";
        assert_eq!(reduce_trace(trace), trace);
    }

    #[test]
    fn each_death_is_recorded_once_and_the_cursor_survives_a_second_launch() {
        let _serial = crate::hook::TEST_SERIAL
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "rightkit-crash-android-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let config = CrashConfig::new("App", "1", &dir);
        let exits = [
            exit(5, 100, Some("backtrace:\n  #00 pc 1234 libapp.so\n")),
            exit(5, 200, None),
            exit(6, 150, Some("\"main\" prio=5\n")),
            exit(10, 300, None),
        ];
        let first = record_android_exits(&config, &exits);
        assert_eq!(first.len(), 3);
        let again = record_android_exits(&config, &exits);
        assert!(again.is_empty(), "a recorded death is not recorded twice");
        assert_eq!(read_cursor(&dir), 200);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
