//! The operating system's own crash and resource reports about an app's processes, read by name
//! and time only.
//!
//! - macOS: the files in `/Library/Logs/DiagnosticReports` and `~/Library/Logs/DiagnosticReports`
//!   whose names start with one of the app's process prefixes and end `.diag`, `.crash` or `.ips`.
//!   The kind comes from the name (`cpu_resource`, `disk_writes`), `crash` for `.crash` and `.ips`,
//!   and for an older `.diag` from its `Event:` line (the first 4 KB are read for that one word and
//!   nothing else is kept); `resource` when a `.diag` says none of these.
//! - Windows: the Windows Error Reporting folders `AppCrash_<exe>_...` and `AppHang_<exe>_...`
//!   under the machine's and the user's `ReportArchive`. Nothing inside a folder is opened.
//!
//! A report's date is the `YYYY-MM-DD-HHMMSS` stamp in its name (local time), else its modified
//! time (UTC), as "YYYY-MM-DD HH:MM". A Windows report's executable name is reduced to letters,
//! digits and `_ - . :` (at most 40, else "?"); a macOS report's process is the name before its
//! stamp. What a host does with a report (raise an event, keep a seen-marker, show a row) is the
//! host's.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// What a report says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    CpuResource,
    DiskWrites,
    Crash,
    Hang,
    Resource,
}

impl ReportKind {
    /// The stable code: `cpu_resource`, `disk_writes`, `crash`, `hang` or `resource`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CpuResource => "cpu_resource",
            Self::DiskWrites => "disk_writes",
            Self::Crash => "crash",
            Self::Hang => "hang",
            Self::Resource => "resource",
        }
    }
}

/// One report, by name and time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsReport {
    pub kind: ReportKind,
    /// The process the report names (on Windows reduced to a code: letters, digits, `_ - . :`).
    pub process: String,
    /// "YYYY-MM-DD HH:MM": the name's stamp (local time) or the modified time (UTC).
    pub date: String,
    /// The file's or folder's modified time, Unix milliseconds.
    pub modified_ms: i64,
    /// The report's file or folder name (never a path): its identity.
    pub file: String,
}

/// What one listing of every report folder found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OsReports {
    /// Newest first.
    pub reports: Vec<OsReport>,
    /// Folders that could be listed (a missing folder is neither).
    pub readable: usize,
    /// Folders that exist but could not be listed.
    pub unreadable: usize,
}

/// macOS's two DiagnosticReports folders, the machine's and `home`'s.
pub fn macos_report_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/Library/Logs/DiagnosticReports"),
        home.join("Library/Logs/DiagnosticReports"),
    ]
}

/// The machine's and the user's Windows Error Reporting `ReportArchive`; empty off Windows.
pub fn wer_report_dirs() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    ["ProgramData", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Microsoft/Windows/WER/ReportArchive"))
        .collect()
}

/// Every report on this system about a process whose name starts with one of `prefixes`
/// (compared lower-case), newest first. macOS reads `home`'s and the machine's
/// DiagnosticReports, Windows its Error Reporting archives; another system has none.
pub fn os_reports(home: &Path, prefixes: &[&str]) -> OsReports {
    let mut out = OsReports::default();
    let mut add = |found: std::io::Result<Vec<OsReport>>| match found {
        Ok(found) => {
            out.readable += 1;
            out.reports.extend(found);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => out.unreadable += 1,
    };
    if cfg!(target_os = "macos") {
        for dir in macos_report_dirs(home) {
            add(macos_reports_in(&dir, prefixes));
        }
    }
    for dir in wer_report_dirs() {
        add(wer_reports_in(&dir, prefixes));
    }
    out.reports
        .sort_by_key(|report| std::cmp::Reverse(report.modified_ms));
    out
}

fn matches_prefix(lower: &str, prefixes: &[&str]) -> bool {
    prefixes
        .iter()
        .any(|prefix| lower.starts_with(&prefix.to_lowercase()))
}

fn modified_ms(entry: &std::fs::DirEntry) -> Option<i64> {
    let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
    Some(
        modified
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0),
    )
}

/// The reports in one macOS DiagnosticReports folder.
pub fn macos_reports_in(dir: &Path, prefixes: &[&str]) -> std::io::Result<Vec<OsReport>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        if !matches_prefix(&lower, prefixes) {
            continue;
        }
        let Some(extension) = ["diag", "crash", "ips"]
            .into_iter()
            .find(|e| lower.ends_with(&format!(".{e}")))
        else {
            continue;
        };
        let Some(modified_ms) = modified_ms(&entry) else {
            continue;
        };
        let stamp = name_stamp(&name);
        let date = stamp
            .as_ref()
            .map_or_else(|| format_ms(modified_ms), |(_, text)| text.clone());
        let process = match &stamp {
            Some((at, _)) => name[..*at].trim_end_matches(['_', '-', '.']).to_string(),
            None => name
                .split(['_', '.'])
                .next()
                .unwrap_or_default()
                .to_string(),
        };
        out.push(OsReport {
            kind: classify(&entry.path(), &lower, extension),
            process,
            date,
            modified_ms,
            file: name,
        });
    }
    Ok(out)
}

/// The `AppCrash_` and `AppHang_` folders in one Windows Error Reporting archive.
pub fn wer_reports_in(dir: &Path, prefixes: &[&str]) -> std::io::Result<Vec<OsReport>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        let kind = if lower.starts_with("appcrash_") {
            ReportKind::Crash
        } else if lower.starts_with("apphang_") {
            ReportKind::Hang
        } else {
            continue;
        };
        let exe = name.split('_').nth(1).unwrap_or("");
        if !matches_prefix(&exe.to_lowercase(), prefixes) {
            continue;
        }
        let Some(modified_ms) = modified_ms(&entry) else {
            continue;
        };
        out.push(OsReport {
            kind,
            process: code(exe, 40),
            date: format_ms(modified_ms),
            modified_ms,
            file: name,
        });
    }
    Ok(out)
}

fn classify(path: &Path, lower_name: &str, extension: &str) -> ReportKind {
    if lower_name.contains("cpu_resource") {
        return ReportKind::CpuResource;
    }
    if lower_name.contains("disk_writes") {
        return ReportKind::DiskWrites;
    }
    if extension != "diag" {
        return ReportKind::Crash;
    }
    // An older `.diag` names its kind only on its `Event:` line; read that and keep nothing else.
    use std::io::Read;
    let mut head = vec![0u8; 4096];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let text = String::from_utf8_lossy(&head[..read]).to_lowercase();
    let event = text
        .lines()
        .find(|l| l.trim_start().starts_with("event:"))
        .unwrap_or("");
    if event.contains("disk writes") {
        ReportKind::DiskWrites
    } else if event.contains("cpu") {
        ReportKind::CpuResource
    } else {
        ReportKind::Resource
    }
}

/// The `YYYY-MM-DD-HHMMSS` stamp in a report's name: where it starts and "YYYY-MM-DD HH:MM".
fn name_stamp(name: &str) -> Option<(usize, String)> {
    let bytes = name.as_bytes();
    let digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    for start in 0..bytes.len().saturating_sub(16) {
        let shape = [
            (0..4, true),
            (4..5, false),
            (5..7, true),
            (7..8, false),
            (8..10, true),
            (10..11, false),
            (11..17, true),
        ];
        let matches = shape.iter().all(|(range, digits)| {
            range.clone().all(|i| {
                if *digits {
                    digit(start + i)
                } else {
                    bytes.get(start + i) == Some(&b'-')
                }
            })
        });
        if matches {
            let text = &name[start..start + 17];
            return Some((
                start,
                format!("{} {}:{}", &text[..10], &text[11..13], &text[13..15]),
            ));
        }
    }
    None
}

/// "YYYY-MM-DD HH:MM" (UTC) from Unix milliseconds.
pub fn format_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (year, month, day) = rightkit_time::civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3_600,
        rest % 3_600 / 60
    )
}

/// A fixed code of letters, digits and `_ - . :` only, at most `max` long; anything else is "?".
pub fn code(text: &str, max: usize) -> String {
    let fine = !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
    if fine {
        text.to_string()
    } else {
        "?".to_string()
    }
}
