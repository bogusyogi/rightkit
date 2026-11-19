//! User-initiated diagnostics archive: staged files, a manifest and one ZIP, built from bounded
//! reads of local logs, a host-built summary and the crash admission of this crate. Nothing here
//! uploads or sends.
//!
//! What the archive can contain (every item is bounded):
//!
//! * each [`LogFile`] generation: the newest [`ExportLimits::log_tail_bytes`] bytes at a line
//!   boundary, one JSON object per line; a line that is not a JSON object is dropped and counted,
//!   never copied. Every kept line goes through the host's `sanitize` callback, then the host's
//!   [`rightkit_logs::RedactionPolicy`] (which masks each event's free-text `message`, as the host's
//!   summary does), then has every field of the named content events replaced, then is scrubbed
//!   with [`rightkit_logs::scrub_value_counted`];
//! * the host-built `summary` value, written as the host named it;
//! * crash reports (`crash_*.json` in the crash directory and in its `submitted/` folder), in every
//!   mode, whether or not native files are chosen: each is read by this module (one no-follow
//!   handle, contained to the crash directory, [`ExportLimits::max_report_bytes`] per file,
//!   [`ExportLimits::max_reports`] newest first, [`ExportLimits::max_report_total_bytes`] in all),
//!   parsed, and re-emitted as a new object built from an allow-list of fields. A report file is
//!   untrusted input (a panic message, a thread name, a source path or another process may have
//!   written any string in it), so a string survives only when it equals a member of a closed
//!   enumeration (compiled into this module or given by the host as [`ReportAllowList`]) or matches
//!   a strict numeric grammar; every other string is replaced by a fixed marker and counted.
//!   `message` is always replaced, unknown fields are dropped, a report that cannot be parsed is left
//!   out and counted, never copied raw. The crate's own `admit_crash_artifacts` is not called. The
//!   archive member name is generated (`crash/report-0001.json`, `crash/submitted/report-0001.json`)
//!   and never derived from the on-disk file name;
//! * only when `include_native_dumps` is set, native `dump_*` and `sample_*` files, admitted here
//!   under a per-file byte cap and a file-count cap ([`ExportLimits::max_native_file_bytes`],
//!   [`ExportLimits::max_native_files`]) that apply before any byte is copied, under generated
//!   member names (`crash/native-0001.dmp`, `crash/native-0002.txt`);
//! * `manifest.json`: the host's fields plus the schema version, the export time, the names of the
//!   log and summary files (the host's names), the redaction counts, the dropped line count, the
//!   counts of native files included and left out, the `crash_reports` counts and the content
//!   fingerprint. Crash members are counted, never listed, and no on-disk crash file name is
//!   written to it.
//!
//! There is no way to add a raw, unscrubbed file to the archive: every byte that is not a native
//! crash dump the user chose passes the redaction above.
//!
//! Safety properties: every host name is validated before any file is written; log files, crash
//! reports and native dumps are opened without following a link at the final component, through one
//! handle, with a byte cap (reports and native files also have their opened handle's real path
//! checked to lie inside the crash directory, and every byte they return, the probe byte and bytes
//! read before an error included, is charged against one shared budget); the staged tree and the
//! final archive are size-capped; a run stages in its own
//! directory (a killed run leaves nothing that blocks the next one, and stale runs are removed with
//! [`rightkit_fs::remove_tree`], which never follows a link); the archive is published atomically.
//! Errors carry no path and no file content.

use crate::create_zip;
use rightkit_fs::digest::Sha256Hasher;
use rightkit_fs::handle::{final_path as handle_final_path, open_read_nofollow, ReadOptions};
use rightkit_fs::{is_within, remove_tree, RemoveOptions};
use rightkit_logs::{scrub_value_counted, RedactionCounts, RedactionPolicy};
use serde_json::{json, Map, Value};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const CONTENT_MARKER: &str = "[redacted:diagnostics]";
/// Fields kept on a content event: the event name and its envelope, nothing else.
const CONTENT_EVENT_KEEP: &[&str] = &["event", "level", "ts_ms", "timestamp", "schema_version"];
/// A staging run older than this is a dead run and is removed.
const STALE_RUN_AFTER: Duration = Duration::from_secs(60 * 60);
const MAX_STALE_ENTRIES_SCANNED: usize = 256;
/// Crash directory entries looked at when choosing native files; a directory with more is cut off.
const MAX_CRASH_ENTRIES_SCANNED: usize = 4096;
/// A report's `submitted/` folder, relative to the crash directory.
const SUBMITTED_DIR: &str = "submitted";
/// Backtrace lines kept per report; the rest are dropped and counted.
const MAX_BACKTRACE_LINES: usize = 256;
const MAX_STAGED_FILES: usize = 4096;
const MAX_STAGED_DEPTH: usize = 8;
const MAX_FINGERPRINT_FILE_BYTES: u64 = 256;
const RESERVED_MANIFEST_KEYS: &[&str] = &[
    "schema_version",
    "exported_at_ms",
    "included_files",
    "content_fingerprint",
    "redactions",
    "log_lines_dropped",
    "native_files_included",
    "native_files_left_out",
    "crash_reports",
];
const MANIFEST_NAME: &str = "manifest.json";

static RUN_SEQ: AtomicU64 = AtomicU64::new(1);

/// Host-chosen names. Each is 1 to the stated length of `A-Za-z0-9._-`, with no `..`; only
/// `fingerprint` may start with one `.`. All four must differ.
#[derive(Clone, Debug)]
pub struct ExportNames {
    /// The archive's single top-level folder (at most 64 bytes).
    pub staging_dir: String,
    /// The fixed archive file name of a [`ExportMode::Local`] export; ends in `.zip` (at most 96).
    pub archive: String,
    /// The file holding the content fingerprint of the last local export (at most 96).
    pub fingerprint: String,
    /// Prefix of an [`ExportMode::Upload`] archive's unique name (at most 64).
    pub upload_archive_prefix: String,
}

/// Byte bounds. Ranges are checked on use; out of range is an error, never clamped silently.
#[derive(Clone, Copy, Debug)]
pub struct ExportLimits {
    /// Newest bytes read from each log generation: 4 KiB to 64 MiB.
    pub log_tail_bytes: u64,
    /// Sum of every staged file: 64 KiB to 512 MiB.
    pub max_staged_bytes: u64,
    /// Final ZIP size: 64 KiB to 512 MiB.
    pub max_archive_bytes: u64,
    /// Largest native `dump_*` or `sample_*` file admitted: 64 KiB to 64 MiB. A larger file is left
    /// out whole (a cut dump is not readable) and counted in `native_files_left_out`.
    pub max_native_file_bytes: u64,
    /// Most native files admitted, newest first: 1 to 16. The rest are left out and counted.
    pub max_native_files: u64,
    /// Largest crash report read: 16 KiB to 4 MiB. A larger file is left out whole and counted.
    pub max_report_bytes: u64,
    /// Most crash reports read (pending and `submitted/` together), newest first: 1 to 256. The rest
    /// are left out and counted.
    pub max_reports: u64,
    /// Sum of the bytes read from all crash reports: 64 KiB to 64 MiB. A report that would pass it
    /// is left out and counted.
    pub max_report_total_bytes: u64,
}

/// One log with its previous generation. Both file names must be plain (see [`ExportNames`]) and
/// differ from every other staged name.
#[derive(Clone, Debug)]
pub struct LogFile {
    pub current: PathBuf,
    pub rotated: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportMode {
    /// The user's export: the fixed archive name, and an unchanged export is not rebuilt.
    Local,
    /// An archive for one send: a unique name, no fingerprint, always rebuilt. The caller removes it.
    Upload,
}

/// What the host knows to be true of its own crash reports. A string in a report file survives into
/// the archive only when it equals a member of one of these lists (or of the closed lists compiled
/// into this module, or matches a numeric version) and is otherwise replaced by a marker. Nothing
/// has a product default: an empty list trusts nothing from the host. Each list has at most 128
/// entries of 1 to 128 printable ASCII bytes (`source_files`: no `\`; `symbol_roots`: identifier
/// characters only); an entry out of shape is an error, never clamped.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReportAllowList<'a> {
    /// Report `kind` values the host records itself, beyond the crate's own kinds.
    pub kinds: &'a [&'a str],
    /// `process_kind` values beyond the crate's `window`, `engine` and `helper`.
    pub process_kinds: &'a [&'a str],
    /// The app names the host's reports carry (`app_name`).
    pub app_names: &'a [&'a str],
    /// Exact `app_version` and `build` values the host's reports carry, beyond any value made only of
    /// digits and dots.
    pub versions: &'a [&'a str],
    /// Thread names the host's code sets or registers (the report's `thread`), beyond the crate's
    /// `<unnamed>`, `main`, `webview`, `supervisor`, `android_exit_history` and `rightkit-crash-sink`.
    pub threads: &'a [&'a str],
    /// Source files, written with `/`, that may appear in a `location` or in a stack frame; a path
    /// is compared whole, never by prefix, so a file name cannot carry anything the list does not
    /// already hold. A path not in the list is replaced by the marker and only its line and column
    /// remain.
    pub source_files: &'a [&'a str],
    /// First path segments (crate names) a Rust stack-frame symbol may start with, beyond `std`,
    /// `core`, `alloc` and `rightkit_crash`. A frame is shown as `root::[symbol]`: the crate (this
    /// list's own entry) and a fixed marker, never the function path.
    pub symbol_roots: &'a [&'a str],
}

impl ReportAllowList<'_> {
    fn validate(&self) -> Result<(), String> {
        let plain = |list: &[&str], extra: &dyn Fn(&str) -> bool| {
            list.len() <= 128
                && list.iter().all(|entry| {
                    !entry.is_empty()
                        && entry.len() <= 128
                        && entry.bytes().all(|b| (0x20..0x7f).contains(&b))
                        && extra(entry)
                })
        };
        let ok = plain(self.kinds, &|_| true)
            && plain(self.process_kinds, &|_| true)
            && plain(self.app_names, &|_| true)
            && plain(self.versions, &|_| true)
            && plain(self.threads, &|_| true)
            && plain(self.source_files, &|entry| !entry.contains('\\'))
            && plain(self.symbol_roots, &|entry| {
                entry
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            });
        if ok {
            Ok(())
        } else {
            Err("report allow-list is invalid".to_string())
        }
    }
}

pub struct ExportRequest<'a> {
    /// Export directory (created if missing). Everything this call writes lies under it.
    pub root: &'a Path,
    pub names: &'a ExportNames,
    pub limits: ExportLimits,
    pub mode: ExportMode,
    pub logs: &'a [LogFile],
    /// Event names (`"event"` field) whose payload fields are all replaced: events known to have
    /// carried content before the producer was fixed. At most 64 names of at most 128 bytes.
    pub content_events: &'a [&'a str],
    /// Host redaction applied to every kept log line before the component's own scrub.
    pub sanitize: &'a dyn Fn(&mut Value),
    /// Key redaction applied to every kept log line after `sanitize`. Its default exact keys include
    /// `message`, so each event's free text is replaced wholesale, exactly as the host's summary
    /// collection replaces it. Closed codes, counts, durations and ids under other keys stay.
    pub redaction: &'a RedactionPolicy,
    /// File name of the summary inside the archive (plain, at most 128 bytes).
    pub summary_name: &'a str,
    /// The summary, already collected and redacted by the host.
    pub summary: &'a Value,
    /// Crash directory to admit from (reports in every mode, native files only on request); `None`
    /// or a missing directory admits nothing.
    pub crash_dir: Option<&'a Path>,
    /// The host's closed lists for the strings of a crash report (see [`ReportAllowList`]).
    pub report_allow: &'a ReportAllowList<'a>,
    /// Native dumps and samples can hold memory contents: only when the user chose them. Crash
    /// reports are not governed by this flag; they are always re-emitted redacted.
    pub include_native_dumps: bool,
    /// Host manifest fields (app name, version, disclosures). Keys must not be reserved; at most
    /// 32 keys.
    pub manifest_fields: &'a Map<String, Value>,
    pub exported_at_ms: u64,
}

/// Why [`read_archive_bounded`] gave nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundedReadError {
    TooLarge,
    Unreadable,
}

fn name_ok(value: &str, max: usize, allow_leading_dot: bool) -> bool {
    let body = if allow_leading_dot {
        value.strip_prefix('.').unwrap_or(value)
    } else {
        value
    };
    !value.is_empty()
        && value.len() <= max
        && !body.starts_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn in_range(value: u64, low: u64, high: u64) -> bool {
    (low..=high).contains(&value)
}

impl ExportNames {
    fn validate(&self) -> Result<(), String> {
        let ok = name_ok(&self.staging_dir, 64, false)
            && name_ok(&self.archive, 96, false)
            && self.archive.ends_with(".zip")
            && name_ok(&self.fingerprint, 96, true)
            && name_ok(&self.upload_archive_prefix, 64, false)
            && self.staging_dir != self.archive
            && self.archive != self.fingerprint
            && self.staging_dir != self.fingerprint;
        if ok {
            Ok(())
        } else {
            Err("export names are invalid".to_string())
        }
    }
}

impl ExportLimits {
    fn validate(&self) -> Result<(), String> {
        const MIB: u64 = 1024 * 1024;
        let ok = in_range(self.log_tail_bytes, 4 * 1024, 64 * MIB)
            && in_range(self.max_staged_bytes, 64 * 1024, 512 * MIB)
            && in_range(self.max_archive_bytes, 64 * 1024, 512 * MIB)
            && in_range(self.max_native_file_bytes, 64 * 1024, 64 * MIB)
            && in_range(self.max_native_files, 1, 16)
            && in_range(self.max_report_bytes, 16 * 1024, 4 * MIB)
            && in_range(self.max_reports, 1, 256)
            && in_range(self.max_report_total_bytes, 64 * 1024, 64 * MIB);
        if ok {
            Ok(())
        } else {
            Err("export limits are out of range".to_string())
        }
    }
}

impl ExportRequest<'_> {
    fn validate(&self) -> Result<(), String> {
        self.names.validate()?;
        self.limits.validate()?;
        self.report_allow.validate()?;
        if !name_ok(self.summary_name, 128, false) {
            return Err("summary name is invalid".to_string());
        }
        if self.content_events.len() > 64
            || self
                .content_events
                .iter()
                .any(|name| name.is_empty() || name.len() > 128)
        {
            return Err("content event list is invalid".to_string());
        }
        if self.manifest_fields.len() > 32
            || self
                .manifest_fields
                .keys()
                .any(|key| RESERVED_MANIFEST_KEYS.contains(&key.as_str()))
        {
            return Err("manifest fields are invalid".to_string());
        }
        // Every staged top-level name, checked for validity and for collisions.
        let mut seen = std::collections::BTreeSet::new();
        let mut claim = |name: &str| -> Result<(), String> {
            if !seen.insert(name.to_string()) {
                return Err("export file names collide".to_string());
            }
            Ok(())
        };
        claim(MANIFEST_NAME)?;
        claim("crash")?;
        claim(self.summary_name)?;
        if self.logs.len() > 16 {
            return Err("too many export files".to_string());
        }
        for log in self.logs {
            for path in [&log.current, &log.rotated] {
                let name = file_name_of(path)?;
                claim(&name)?;
            }
        }
        Ok(())
    }
}

fn file_name_of(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "a log file has no portable name".to_string())?;
    if !name_ok(name, 128, false) {
        return Err("a log file name is invalid".to_string());
    }
    Ok(name.to_string())
}

/// Open one regular file without following a link at the final component (single hard link), read
/// at most the newest `cap` bytes through that one handle, and drop a partial first line.
/// `Ok(None)` when the file does not exist or is not a regular file.
fn read_tail(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, String> {
    let mut file = match open_read_nofollow(path, &ReadOptions::new().require_single_link(true)) {
        Ok(file) => file,
        Err(error) if error.kind() == Some(std::io::ErrorKind::NotFound) => return Ok(None),
        Err(_) => return Err("a diagnostics file could not be opened".to_string()),
    };
    let length = file
        .metadata()
        .map_err(|_| "a diagnostics file could not be read".to_string())?
        .len();
    let start = length.saturating_sub(cap);
    if start > 0 {
        file.seek(SeekFrom::Start(start))
            .map_err(|_| "a diagnostics file could not be read".to_string())?;
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(cap)
        .read_to_end(&mut bytes)
        .map_err(|_| "a diagnostics file could not be read".to_string())?;
    if start > 0 {
        match bytes.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                bytes.drain(0..=newline);
            }
            None => bytes.clear(),
        }
    }
    Ok(Some(bytes))
}

/// Read the archive once, never more than `limit` + 1 bytes, and refuse one over `limit`. The caller
/// sends exactly these bytes, so the checked size cannot differ from the sent body.
pub fn read_archive_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, BoundedReadError> {
    let file =
        open_read_nofollow(path, &ReadOptions::new()).map_err(|_| BoundedReadError::Unreadable)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| BoundedReadError::Unreadable)?;
    if bytes.len() as u64 > limit {
        return Err(BoundedReadError::TooLarge);
    }
    Ok(bytes)
}

fn replace_content_payload(value: &mut Value) {
    if let Value::Object(object) = value {
        for (key, field) in object.iter_mut() {
            if !CONTENT_EVENT_KEEP.contains(&key.as_str()) {
                *field = Value::String(CONTENT_MARKER.to_string());
            }
        }
    }
}

/// Re-redact one log generation: JSON object lines only, each through the host's `sanitize`, the
/// host's redaction policy, the content-event rule and the scrub. Returns the staged bytes and how many lines were dropped.
fn sanitize_log(
    bytes: &[u8],
    request: &ExportRequest<'_>,
    counts: &mut RedactionCounts,
) -> (Vec<u8>, usize) {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::with_capacity(bytes.len());
    let mut dropped = 0usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(mut value) = serde_json::from_str::<Value>(line) else {
            dropped += 1;
            continue;
        };
        if !value.is_object() {
            dropped += 1;
            continue;
        }
        (request.sanitize)(&mut value);
        counts.merge(&request.redaction.redact_value_counted(&mut value));
        let content_event = value
            .get("event")
            .and_then(Value::as_str)
            .is_some_and(|name| request.content_events.iter().any(|known| *known == name));
        if content_event {
            replace_content_payload(&mut value);
        }
        counts.merge(&scrub_value_counted(&mut value));
        if serde_json::to_writer(&mut out, &value).is_err() {
            dropped += 1;
            continue;
        }
        out.push(b'\n');
    }
    (out, dropped)
}

struct Staged {
    included: Vec<String>,
    bytes: u64,
    max_bytes: u64,
}

impl Staged {
    /// Count `length` bytes against the staged cap and list `name` in the manifest.
    fn add(&mut self, name: &str, length: usize) -> Result<(), String> {
        self.count(length)?;
        self.included.push(name.to_string());
        Ok(())
    }

    /// Count `length` bytes against the staged cap without listing a name: crash members are
    /// reported by count only.
    fn count(&mut self, length: usize) -> Result<(), String> {
        let length = u64::try_from(length)
            .map_err(|_| "diagnostics exceed the staged size limit".to_string())?;
        self.bytes = self
            .bytes
            .checked_add(length)
            .filter(|total| *total <= self.max_bytes)
            .ok_or_else(|| "diagnostics exceed the staged size limit".to_string())?;
        Ok(())
    }
}

fn write_staged(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))
        .map_err(|_| "a diagnostics file could not be staged".to_string())?;
    file.write_all(bytes)
        .map_err(|_| "a diagnostics file could not be staged".to_string())
}

/// Every regular file under `dir` (links are never followed or counted), relative to `root`,
/// bounded in depth and count.
fn staged_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if depth > MAX_STAGED_DEPTH {
        return Err("diagnostics staging is nested too deeply".to_string());
    }
    for entry in fs::read_dir(dir).map_err(|_| "diagnostics staging is unreadable".to_string())? {
        let entry = entry.map_err(|_| "diagnostics staging is unreadable".to_string())?;
        let kind = entry
            .file_type()
            .map_err(|_| "diagnostics staging is unreadable".to_string())?;
        let path = entry.path();
        if kind.is_dir() {
            staged_files(root, &path, depth + 1, files)?;
        } else if kind.is_file() {
            if files.len() >= MAX_STAGED_FILES {
                return Err("diagnostics staging holds too many files".to_string());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "diagnostics staging is unreadable".to_string())?;
            files.push(relative.to_path_buf());
        }
    }
    Ok(())
}

/// SHA-256 over the sorted relative names, lengths and bytes of the staged files, read in chunks.
/// Also returns the summed length.
fn content_fingerprint(staging: &Path) -> Result<(String, u64), String> {
    let mut files = Vec::new();
    staged_files(staging, staging, 0, &mut files)?;
    files.sort();
    let mut hash = Sha256Hasher::new();
    let mut total = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    for relative in files {
        let mut file = File::open(staging.join(&relative))
            .map_err(|_| "a staged file could not be read".to_string())?;
        let length = file
            .metadata()
            .map_err(|_| "a staged file could not be read".to_string())?
            .len();
        hash.update(relative.to_string_lossy().as_bytes());
        hash.update(&length.to_le_bytes());
        let mut remaining = length;
        while remaining > 0 {
            let want = chunk
                .len()
                .min(usize::try_from(remaining).unwrap_or(chunk.len()));
            let read = file
                .read(&mut chunk[..want])
                .map_err(|_| "a staged file could not be read".to_string())?;
            if read == 0 {
                return Err("a staged file changed while it was read".to_string());
            }
            hash.update(&chunk[..read]);
            remaining -= read as u64;
        }
        total = total.saturating_add(length);
    }
    Ok((hash.finalize(), total))
}

fn is_native_artifact_name(name: &str) -> bool {
    NativeKind::detect(name).is_some()
}

fn is_report_name(name: &str) -> bool {
    name.starts_with("crash_") && name.ends_with(".json")
}

/// The kind of a native file, detected from the prefix of its on-disk name against this closed
/// list. Only the kind reaches the archive (it picks the generated member name's extension); the
/// on-disk name never does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeKind {
    Dump,
    Sample,
}

impl NativeKind {
    fn detect(name: &str) -> Option<Self> {
        if name.starts_with("dump_") {
            Some(Self::Dump)
        } else if name.starts_with("sample_") {
            Some(Self::Sample)
        } else {
            None
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Dump => "dmp",
            Self::Sample => "txt",
        }
    }
}

/// Why a crash-directory file was not read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadFail {
    /// Longer than the per-file cap (by the handle's length, or by the probe byte).
    TooLarge,
    /// The shared byte budget could not cover the file, or ran out before its end was seen.
    OverBudget,
    Unreadable,
}

/// The bytes a group of reads may still consume. Every byte a read returns is deducted at the
/// moment it is returned: the probe byte after a cap, the bytes read before a later read error,
/// and the bytes of a file that is rejected afterwards (unparseable, too large) all count.
struct IoBudget {
    remaining: u64,
}

impl IoBudget {
    fn new(total: u64) -> Self {
        Self { remaining: total }
    }

    fn is_spent(&self) -> bool {
        self.remaining == 0
    }

    /// Bytes charged so far out of `total`.
    fn charged(&self, total: u64) -> u64 {
        total.saturating_sub(self.remaining)
    }
}

/// Read one file of the crash directory: `dir` is `root` or a folder directly below it, `root` is
/// the crash directory resolved once by the caller. One no-follow, single-link handle (a link at the
/// final component, a directory, a device and a hard-linked file are refused); the handle's real
/// path must lie inside `root`, which catches a folder swapped for a link after it was listed; the
/// length comes from that handle. A file longer than `cap` by that length is refused with no byte
/// read. Otherwise the file is read in chunks of at most 16 KiB, never past `cap + 1` bytes (the
/// extra byte is the probe that catches a file that grew) and never past what `budget` has left;
/// each chunk is deducted from `budget` as it arrives, so a later error or rejection cannot hide
/// bytes already consumed. The bytes are returned only when the end of the file was reached within
/// `cap` and within the budget (a file whose end cannot be confirmed inside the budget is
/// `OverBudget`, never returned cut). No metadata-then-reopen.
fn read_contained(
    root: &Path,
    dir: &Path,
    name: &str,
    cap: u64,
    budget: &mut IoBudget,
) -> Result<Vec<u8>, ReadFail> {
    let mut file = open_read_nofollow(
        &dir.join(name),
        &ReadOptions::new().require_single_link(true),
    )
    .map_err(|_| ReadFail::Unreadable)?;
    if !handle_final_path(&file).is_ok_and(|real| is_within(root, &real)) {
        return Err(ReadFail::Unreadable);
    }
    let length = file.metadata().map_err(|_| ReadFail::Unreadable)?.len();
    if length > cap {
        return Err(ReadFail::TooLarge);
    }
    if length > budget.remaining {
        return Err(ReadFail::OverBudget);
    }
    let probe_limit = cap.checked_add(1).ok_or(ReadFail::Unreadable)?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut got = 0u64;
    let mut interrupted = 0u8;
    let mut reached_end = false;
    loop {
        let want = (chunk.len() as u64)
            .min(probe_limit.saturating_sub(got))
            .min(budget.remaining);
        if want == 0 {
            break;
        }
        let want = usize::try_from(want).unwrap_or(chunk.len());
        match file.read(&mut chunk[..want]) {
            Ok(0) => {
                reached_end = true;
                break;
            }
            Ok(read) => {
                let read_bytes = read as u64;
                // Charge before anything else can fail.
                budget.remaining = budget
                    .remaining
                    .checked_sub(read_bytes)
                    .ok_or(ReadFail::Unreadable)?;
                got = got.checked_add(read_bytes).ok_or(ReadFail::Unreadable)?;
                bytes.extend_from_slice(&chunk[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted && interrupted < 8 => {
                interrupted += 1;
            }
            Err(_) => return Err(ReadFail::Unreadable),
        }
    }
    if got > cap {
        return Err(ReadFail::TooLarge);
    }
    if !reached_end {
        return Err(ReadFail::OverBudget);
    }
    Ok(bytes)
}

/// A crash-directory file chosen by name and age (`submitted` is true for the `submitted/` folder).
/// The name is used only to open the file; it is never written to the archive.
struct Candidate {
    modified: std::time::SystemTime,
    name: String,
    submitted: bool,
}

/// List the files of one crash-directory level whose names satisfy `wanted`, looking at no more than
/// `MAX_CRASH_ENTRIES_SCANNED` directory entries, an entry that cannot be read included (each
/// attempt counts). A matching name that is a link, a directory or not plain is counted in
/// `unusable`, never read. `DirEntry::file_type` does not follow a link.
fn scan_level(
    dir: &Path,
    submitted: bool,
    wanted: &dyn Fn(&str) -> bool,
    found: &mut Vec<Candidate>,
    unusable: &mut usize,
    truncated: &mut bool,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut scanned = 0usize;
    for entry in entries {
        scanned = scanned.saturating_add(1);
        if scanned > MAX_CRASH_ENTRIES_SCANNED {
            *truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !wanted(&name) {
            continue;
        }
        if !name_ok(&name, 128, false) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            *unusable = unusable.saturating_add(1);
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        found.push(Candidate {
            modified,
            name,
            submitted,
        });
    }
}

/// Native files and reports are listed the same way: newest first, name as the tie-break.
fn newest_first(found: &mut [Candidate]) {
    found.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.submitted.cmp(&b.submitted))
    });
}

/// `submitted/` is listed only when it is itself a plain folder (`symlink_metadata` does not follow).
fn submitted_dir(root: &Path) -> Option<PathBuf> {
    let dir = root.join(SUBMITTED_DIR);
    fs::symlink_metadata(&dir)
        .is_ok_and(|metadata| metadata.file_type().is_dir())
        .then_some(dir)
}

/// Admit native `dump_*` and `sample_*` files from `root` (the crash directory, resolved) into
/// `crash_staging`, at most `max_native_files` of them (newest first), each at most
/// `max_native_file_bytes` and read by [`read_contained`]. The reads share one byte budget of
/// `(max_native_file_bytes + 1) * max_native_files` (the `+ 1` is each file's probe byte), charged
/// as in [`read_contained`]. A `sample_*` text file is scrubbed again on the way in (a failed scrub
/// writes the crash component's withheld marker, never the raw bytes). Each admitted file is
/// written under a generated member name, `native-0001.dmp` for a dump and `native-0002.txt` for a
/// sample (the number counts admitted native files in order, newest first); the on-disk name is
/// never written to the archive or the manifest. Returns how many native files were admitted and how
/// many were left out (over a cap, unreadable, not a plain name, not a regular file reached by the
/// scan, or after the budget was spent).
fn admit_native_artifacts(
    root: &Path,
    crash_staging: &Path,
    request: &ExportRequest<'_>,
    staged: &mut Staged,
) -> Result<(usize, usize), String> {
    let mut candidates = Vec::new();
    let mut left_out = 0usize;
    let mut truncated = false;
    scan_level(
        root,
        false,
        &is_native_artifact_name,
        &mut candidates,
        &mut left_out,
        &mut truncated,
    );
    newest_first(&mut candidates);
    let allowed = usize::try_from(request.limits.max_native_files).unwrap_or(0);
    left_out = left_out.saturating_add(candidates.len().saturating_sub(allowed));
    candidates.truncate(allowed);
    if candidates.is_empty() {
        return Ok((0, left_out));
    }
    fs::create_dir_all(crash_staging)
        .map_err(|_| "the export staging could not be created".to_string())?;
    let cap = request.limits.max_native_file_bytes;
    let total = cap
        .checked_add(1)
        .and_then(|per_file| per_file.checked_mul(request.limits.max_native_files))
        .ok_or_else(|| "export limits are out of range".to_string())?;
    let mut budget = IoBudget::new(total);
    let mut member_seq = 0u32;
    let mut admitted = 0usize;
    for candidate in candidates {
        let Some(kind) = NativeKind::detect(&candidate.name) else {
            left_out = left_out.saturating_add(1);
            continue;
        };
        if budget.is_spent() {
            left_out = left_out.saturating_add(1);
            continue;
        }
        let Ok(bytes) = read_contained(root, root, &candidate.name, cap, &mut budget) else {
            left_out = left_out.saturating_add(1);
            continue;
        };
        let out = match kind {
            NativeKind::Sample => crate::native::scrub_for_bundle(&bytes).into_bytes(),
            NativeKind::Dump => bytes,
        };
        member_seq = member_seq
            .checked_add(1)
            .ok_or_else(|| "too many export files".to_string())?;
        let member = format!("native-{member_seq:04}.{}", kind.extension());
        staged.count(out.len())?;
        write_staged(crash_staging, &member, &out)?;
        admitted = admitted.saturating_add(1);
    }
    Ok((admitted, left_out))
}

/// What happened to the crash reports of one export (counts only), written to the manifest.
#[derive(Default)]
struct ReportStats {
    included: usize,
    /// Not JSON, not an object, or without an integer `schema_version` and a string `kind`.
    unparseable: usize,
    /// Over `max_report_bytes`.
    too_large: usize,
    /// Could not be opened or was not a plain regular file inside the crash directory (a link, a
    /// directory, a hard-linked file, a name that is not plain, a read error).
    unreadable: usize,
    /// Beyond `max_reports`.
    over_count_cap: usize,
    /// Not read, or not read to its end, because `max_report_total_bytes` was spent.
    over_total_cap: usize,
    /// More than `MAX_CRASH_ENTRIES_SCANNED` entries in a folder: the rest were not looked at.
    scan_truncated: bool,
    /// Values replaced by the marker: a string that is not a member of its closed enumeration or
    /// host list, `message` (always), a position's path part.
    fields_masked: usize,
    /// Fields the allow-list does not know, or known fields of the wrong type, left out.
    fields_dropped: usize,
    /// Backtrace lines that are not stack frames, left out.
    backtrace_lines_dropped: usize,
    /// Stack frames kept in shape whose function (every Rust frame but `<unknown>`, every JavaScript
    /// frame) or path was replaced by a marker.
    backtrace_frames_masked: usize,
    /// Bytes charged against `max_report_total_bytes` (every byte actually read, rejected files
    /// included).
    bytes_read: u64,
}

impl ReportStats {
    fn manifest_value(&self) -> Value {
        json!({
            "included": self.included,
            "left_out_unparseable": self.unparseable,
            "left_out_too_large": self.too_large,
            "left_out_unreadable": self.unreadable,
            "left_out_over_count_cap": self.over_count_cap,
            "left_out_over_total_cap": self.over_total_cap,
            "scan_truncated": self.scan_truncated,
            "fields_masked": self.fields_masked,
            "fields_dropped": self.fields_dropped,
            "backtrace_lines_dropped": self.backtrace_lines_dropped,
            "backtrace_frames_masked": self.backtrace_frames_masked,
            "bytes_read": self.bytes_read,
        })
    }
}

fn all_digits(text: &str, most: usize) -> bool {
    !text.is_empty() && text.len() <= most && text.bytes().all(|b| b.is_ascii_digit())
}

fn in_list(list: &[&str], text: &str) -> bool {
    list.iter().any(|known| *known == text)
}

/// `kind` values the crash crate itself writes (`hook.rs`, `hang.rs`, `window.rs`, `android.rs`).
const CRATE_REPORT_KINDS: &[&str] = &[
    "panic",
    "hang",
    "sidecar_crash",
    "window_error",
    "window_rejection",
    "java_crash",
    "native_crash",
    "anr",
    "exit",
];
/// The crate's Android kinds also appear numbered (`anr_2`) when one batch holds several exits.
const CRATE_NUMBERED_KINDS: &[&str] = &["java_crash", "native_crash", "anr", "exit"];
/// `ProcessKind::as_str` of the crate.
const CRATE_PROCESS_KINDS: &[&str] = &["window", "engine", "helper"];
/// Thread names the crate itself writes or the standard library fixes: an unnamed thread, the
/// main thread, the webview and supervisor pseudo-threads, the Android history, the crate's sink
/// thread.
const CRATE_THREADS: &[&str] = &[
    "<unnamed>",
    "main",
    "webview",
    "supervisor",
    "android_exit_history",
    "rightkit-crash-sink",
];
/// The codes the crate writes into `native_capture_note` (`native.rs`).
const CRATE_NOTE_CODES: &[&str] = &[
    "sample_scrub_failed",
    "sample_read_failed",
    "sample_publish_failed",
    "sample_raw_create_failed",
    "sample_raw_not_removed",
];
/// `std::env::consts::OS` values.
const OS_NAMES: &[&str] = &[
    "linux",
    "macos",
    "ios",
    "android",
    "windows",
    "freebsd",
    "netbsd",
    "openbsd",
    "dragonfly",
    "solaris",
    "illumos",
    "fuchsia",
    "haiku",
    "redox",
];
/// `std::env::consts::ARCH` values.
const ARCH_NAMES: &[&str] = &[
    "x86",
    "x86_64",
    "arm",
    "aarch64",
    "riscv32",
    "riscv64",
    "powerpc",
    "powerpc64",
    "s390x",
    "mips",
    "mips64",
    "sparc64",
    "loongarch64",
    "wasm32",
    "wasm64",
];
/// First path segment of a Rust symbol that is always accepted: the compiler's own crates and this
/// crate. Host crates are added through [`ReportAllowList::symbol_roots`].
const STD_SYMBOL_ROOTS: &[&str] = &["std", "core", "alloc", "rightkit_crash"];

fn is_numbered_kind(kind: &str) -> bool {
    kind.rsplit_once('_')
        .is_some_and(|(base, number)| in_list(CRATE_NUMBERED_KINDS, base) && all_digits(number, 3))
}

fn kind_trusted(text: &str, allow: &ReportAllowList<'_>) -> bool {
    in_list(CRATE_REPORT_KINDS, text) || in_list(allow.kinds, text) || is_numbered_kind(text)
}

fn process_kind_trusted(text: &str, allow: &ReportAllowList<'_>) -> bool {
    in_list(CRATE_PROCESS_KINDS, text) || in_list(allow.process_kinds, text)
}

fn thread_trusted(text: &str, allow: &ReportAllowList<'_>) -> bool {
    in_list(CRATE_THREADS, text) || in_list(allow.threads, text)
}

/// Digits separated by single dots, at most four groups of at most nine digits (`0.6.57`).
fn is_numeric_version(text: &str) -> bool {
    let mut groups = 0usize;
    for group in text.split('.') {
        groups += 1;
        if groups > 4 || !all_digits(group, 9) {
            return false;
        }
    }
    true
}

fn version_trusted(text: &str, allow: &ReportAllowList<'_>) -> bool {
    is_numeric_version(text) || in_list(allow.versions, text)
}

/// A `native_capture_note` is one or more crate codes joined by `"; "`, a code optionally followed
/// by a raw directory's name. Returns the codes only (the name is dropped), or `None` when any part
/// does not start with a crate code.
fn trusted_note(text: &str) -> Option<String> {
    if text.len() > 256 {
        return None;
    }
    let mut codes: Vec<&str> = Vec::new();
    for part in text.split("; ") {
        let code = part.split(' ').next().unwrap_or("");
        if !in_list(CRATE_NOTE_CODES, code) || codes.len() >= 8 {
            return None;
        }
        codes.push(code);
    }
    Some(codes.join("; "))
}

/// `PATH:LINE` or `PATH:LINE:COLUMN` with numeric positions (at most 512 bytes). Returns the
/// position as written to the archive and whether the path part was replaced: the path is shown
/// only when, with `\` read as `/`, it equals a member of [`ReportAllowList::source_files`];
/// otherwise the marker stands in its place and only the numbers remain.
fn shown_position(text: &str, allow: &ReportAllowList<'_>) -> Option<(String, bool)> {
    if text.len() > 512 {
        return None;
    }
    let (rest, last) = text.rsplit_once(':')?;
    if !all_digits(last, 7) {
        return None;
    }
    let (path, line, column) = match rest.rsplit_once(':') {
        Some((path, line)) if all_digits(line, 7) => (path, line, Some(last)),
        _ => (rest, last, None),
    };
    if path.is_empty() {
        return None;
    }
    let normalized = path.replace('\\', "/");
    let allowed = in_list(allow.source_files, &normalized);
    let shown = if allowed {
        normalized.as_str()
    } else {
        CONTENT_MARKER
    };
    let position = match column {
        Some(column) => format!("{shown}:{line}:{column}"),
        None => format!("{shown}:{line}"),
    };
    Some((position, !allowed))
}

/// The first path segment of a Rust symbol: leading `<`, `&`, `*`, spaces and the keywords `dyn`,
/// `mut` and `const` are skipped, then the identifier run.
fn symbol_root(symbol: &str) -> &str {
    let mut rest = symbol;
    loop {
        let trimmed = rest.trim_start_matches(['<', '&', '*', ' ']);
        match ["dyn ", "mut ", "const "]
            .iter()
            .find_map(|keyword| trimmed.strip_prefix(keyword))
        {
            Some(next) => rest = next,
            None => {
                rest = trimmed;
                break;
            }
        }
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// A Rust symbol of a stack frame: identifier, path and generic punctuation; a space only inside
/// the keywords ` as `, `dyn `, `mut ` and `; `.
fn is_rust_symbol(text: &str) -> bool {
    if text.is_empty() || text.len() > 256 {
        return false;
    }
    let plain = text
        .replace(" as ", "_")
        .replace("dyn ", "_")
        .replace("mut ", "_")
        .replace("; ", "_");
    plain.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'_' | b':'
                    | b'<'
                    | b'>'
                    | b'{'
                    | b'}'
                    | b'$'
                    | b'.'
                    | b','
                    | b'&'
                    | b'*'
                    | b'['
                    | b']'
                    | b'('
                    | b')'
                    | b'+'
                    | b'-'
                    | b'#'
                    | b'\''
                    | b'='
                    | b'!'
                    | b'?'
                    | b';'
            )
    })
}

/// The fixed text that stands for everything after a symbol's root crate. No part of the input
/// symbol survives past the root.
const SYMBOL_REST_MARKER: &str = "[symbol]";

/// The listed root a symbol starts with: the symbol must pass the strict Rust grammar and its first
/// path segment must EQUAL a member of [`STD_SYMBOL_ROOTS`] or [`ReportAllowList::symbol_roots`].
/// The returned text is the list's own entry, never a slice of the input. `None` when the symbol is
/// not Rust-shaped or its root is not listed.
fn listed_root<'a>(symbol: &str, allow: &ReportAllowList<'a>) -> Option<&'a str> {
    if !is_rust_symbol(symbol) {
        return None;
    }
    let root = symbol_root(symbol);
    if root.is_empty() {
        return None;
    }
    let std_root: Option<&'a str> = STD_SYMBOL_ROOTS
        .iter()
        .find(|known| **known == root)
        .copied();
    std_root.or_else(|| {
        allow
            .symbol_roots
            .iter()
            .find(|known| **known == root)
            .copied()
    })
}

/// One Rust frame symbol as it may appear in the archive, and whether the function part was
/// replaced: `<unknown>` stays (fixed text); a listed root becomes `root::[symbol]` (the root from
/// the list, the rest a fixed marker); anything else is the content marker whole.
fn shown_symbol(symbol: &str, allow: &ReportAllowList<'_>) -> (String, usize) {
    if symbol == "<unknown>" {
        return ("<unknown>".to_string(), 0);
    }
    match listed_root(symbol, allow) {
        Some(root) => (format!("{root}::{SYMBOL_REST_MARKER}"), 1),
        None => (CONTENT_MARKER.to_string(), 1),
    }
}

/// One backtrace line as it may appear in the archive, or `None` when it is not a stack frame.
/// Returns the line and how many of its parts were replaced by the marker (0, 1 or 2).
///
/// * `stack backtrace:` and `[truncated]`: kept as fixed lines;
/// * Rust `N: symbol`: `N: root::[symbol]` when the symbol's first path segment equals a listed root
///   (the root is the list's entry, the rest of the symbol is never copied), else `N: <marker>`;
///   `N: <unknown>` stays;
/// * Rust `at path:line:col`: the path is kept only when [`shown_position`] allows it;
/// * JavaScript `at name (path:line:col)`, `at path:line:col` and `name@path:line:col`: the function
///   name is always replaced by the marker (a page's function names are not enumerable) and the
///   path as above.
fn shown_frame(line: &str, allow: &ReportAllowList<'_>) -> Option<(String, usize)> {
    let text = line.trim();
    if text == "stack backtrace:" || text == "[truncated]" {
        return Some((text.to_string(), 0));
    }
    if let Some((index, symbol)) = text.split_once(": ") {
        if all_digits(index, 5) {
            let (shown, replaced) = shown_symbol(symbol.trim(), allow);
            return Some((format!("{index}: {shown}"), replaced));
        }
    }
    if let Some(rest) = text.strip_prefix("at ") {
        let named = rest
            .split_once(" (")
            .and_then(|(_, location)| location.strip_suffix(')'))
            .and_then(|location| shown_position(location, allow));
        if let Some((position, path_masked)) = named {
            return Some((
                format!("at {CONTENT_MARKER} ({position})"),
                1 + usize::from(path_masked),
            ));
        }
        let (position, path_masked) = shown_position(rest, allow)?;
        return Some((format!("at {position}"), usize::from(path_masked)));
    }
    if let Some((_, location)) = text.split_once('@') {
        let (position, path_masked) = shown_position(location, allow)?;
        return Some((
            format!("{CONTENT_MARKER}@{position}"),
            1 + usize::from(path_masked),
        ));
    }
    None
}

/// Rebuild a backtrace from its stack-frame lines (see [`shown_frame`]). Returns the new text, how
/// many non-empty lines were dropped (lines past `MAX_BACKTRACE_LINES` count as dropped) and how
/// many replacements the marker made inside kept frames.
fn frames_only(text: &str, allow: &ReportAllowList<'_>) -> (String, usize, usize) {
    let mut kept: Vec<String> = Vec::new();
    let mut dropped = 0usize;
    let mut masked = 0usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if kept.len() >= MAX_BACKTRACE_LINES {
            dropped = dropped.saturating_add(1);
            continue;
        }
        match shown_frame(line, allow) {
            Some((frame, replaced)) => {
                masked = masked.saturating_add(replaced);
                kept.push(frame);
            }
            None => dropped = dropped.saturating_add(1),
        }
    }
    (kept.join("\n"), dropped, masked)
}

/// A value of an enumerated field: kept when it is a string that `trusted` accepts, the marker
/// when it is any other string (counted in `masked`), nothing for `null`, and nothing for a value
/// of another type (counted in `dropped`).
fn enumerated(
    value: &Value,
    trusted: &dyn Fn(&str) -> bool,
    masked: &mut usize,
    dropped: &mut usize,
) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(text) if trusted(text.as_str()) => Some(json!(text)),
        Value::String(_) => {
            *masked += 1;
            Some(json!(CONTENT_MARKER))
        }
        _ => {
            *dropped += 1;
            None
        }
    }
}

/// Re-emit one crash report. The output is a new object built from an allow-list; nothing of the
/// input is copied that is not named below. The file is untrusted: any string in it could have been
/// written by an old build, a panic message, a thread name set from user data or another process,
/// and a character filter is not a trust decision. A string therefore survives only when it equals a
/// member of a closed enumeration (compiled into this module, or given by the host in
/// [`ReportAllowList`]) or matches a strict numeric grammar; every other string becomes the marker
/// and is counted. Returns `None` for a report that cannot be parsed.
///
/// | field | treatment |
/// |---|---|
/// | `schema_version`, `unix_time`, `pid` (integers) | kept as numbers (`pid` must fit 32 bits, else dropped) |
/// | `kind` | kept when it equals a crate kind (`panic`, `hang`, `sidecar_crash`, `window_error`, `window_rejection`, `java_crash`, `native_crash`, `anr`, `exit`, the four Android kinds with a `_N` suffix) or a host kind; else the marker |
/// | `process_kind` | kept when it equals a crate process kind (`window`, `engine`, `helper`) or a host one; else the marker |
/// | `os`, `arch` | kept when they equal a member of the fixed `std::env::consts` lists in this module; else the marker |
/// | `app_name` | kept when it equals a host app name; else the marker |
/// | `app_version`, `build` | kept when digits separated by dots (at most four groups) or equal to a host version; else the marker |
/// | `thread` | kept when it equals a crate thread name or a host thread name; else the marker |
/// | `native_capture_note` | kept as the crate's note codes only (a trailing directory name is dropped); else the marker |
/// | `location` | the path part is kept only when, with `\` read as `/`, it equals a host source file; otherwise `[redacted:diagnostics]:LINE[:COLUMN]` (numbers only) |
/// | `message` | always the marker; `message_chars` (its length) added |
/// | `backtrace` | rebuilt from stack-frame lines ([`shown_frame`]): a Rust symbol becomes `root::[symbol]` where `root` is the list entry equal to its first path segment (a std or host crate root), else the marker; a path only if it equals a host source file; every JavaScript function name is replaced; all other lines dropped |
/// | `native_artifacts` | not copied: only `native_artifact_count` (the number of entries) is written |
/// | anything else | dropped |
///
/// The result then goes through the host's `RedactionPolicy` and `scrub_value_counted`, the same
/// passes a log line takes.
fn sanitize_report(
    bytes: &[u8],
    request: &ExportRequest<'_>,
    counts: &mut RedactionCounts,
    stats: &mut ReportStats,
) -> Option<Vec<u8>> {
    let Value::Object(input) = serde_json::from_slice::<Value>(bytes).ok()? else {
        return None;
    };
    let allow = request.report_allow;
    let schema_version = input.get("schema_version").and_then(Value::as_u64)?;
    let kind = input.get("kind").and_then(Value::as_str)?;
    let mut out = Map::new();
    let mut masked = 0usize;
    let mut dropped = 0usize;
    let mut backtrace_dropped = 0usize;
    let mut frames_masked = 0usize;
    out.insert("schema_version".into(), json!(schema_version));
    if kind_trusted(kind, allow) {
        out.insert("kind".into(), json!(kind));
    } else {
        out.insert("kind".into(), json!(CONTENT_MARKER));
        masked += 1;
    }
    for (key, value) in &input {
        match key.as_str() {
            "schema_version" | "kind" => {}
            "unix_time" | "pid" => match value.as_u64() {
                Some(number) if key == "unix_time" || u32::try_from(number).is_ok() => {
                    out.insert(key.clone(), json!(number));
                }
                _ => dropped += 1,
            },
            "process_kind" => {
                if let Some(kept) = enumerated(
                    value,
                    &|text| process_kind_trusted(text, allow),
                    &mut masked,
                    &mut dropped,
                ) {
                    out.insert(key.clone(), kept);
                }
            }
            "os" => {
                if let Some(kept) = enumerated(
                    value,
                    &|text| in_list(OS_NAMES, text),
                    &mut masked,
                    &mut dropped,
                ) {
                    out.insert(key.clone(), kept);
                }
            }
            "arch" => {
                if let Some(kept) = enumerated(
                    value,
                    &|text| in_list(ARCH_NAMES, text),
                    &mut masked,
                    &mut dropped,
                ) {
                    out.insert(key.clone(), kept);
                }
            }
            "app_name" => {
                if let Some(kept) = enumerated(
                    value,
                    &|text| in_list(allow.app_names, text),
                    &mut masked,
                    &mut dropped,
                ) {
                    out.insert(key.clone(), kept);
                }
            }
            "app_version" | "build" => {
                if let Some(kept) = enumerated(
                    value,
                    &|text| version_trusted(text, allow),
                    &mut masked,
                    &mut dropped,
                ) {
                    out.insert(key.clone(), kept);
                }
            }
            "thread" => {
                if let Some(kept) = enumerated(
                    value,
                    &|text| thread_trusted(text, allow),
                    &mut masked,
                    &mut dropped,
                ) {
                    out.insert(key.clone(), kept);
                }
            }
            "native_capture_note" => match value {
                Value::Null => {}
                Value::String(text) => match trusted_note(text) {
                    Some(codes) => {
                        out.insert(key.clone(), json!(codes));
                    }
                    None => {
                        out.insert(key.clone(), json!(CONTENT_MARKER));
                        masked += 1;
                    }
                },
                _ => dropped += 1,
            },
            "location" => match value {
                Value::Null => {}
                Value::String(text) => match shown_position(text, allow) {
                    Some((position, path_masked)) => {
                        out.insert(key.clone(), json!(position));
                        masked += usize::from(path_masked);
                    }
                    None => {
                        out.insert(key.clone(), json!(CONTENT_MARKER));
                        masked += 1;
                    }
                },
                _ => dropped += 1,
            },
            "message" => match value {
                Value::String(text) => {
                    out.insert(key.clone(), json!(CONTENT_MARKER));
                    out.insert("message_chars".into(), json!(text.chars().count()));
                    masked += 1;
                }
                _ => dropped += 1,
            },
            "backtrace" => match value {
                Value::String(text) => {
                    let (frames, lines_dropped, replaced) = frames_only(text, allow);
                    backtrace_dropped = backtrace_dropped.saturating_add(lines_dropped);
                    frames_masked = frames_masked.saturating_add(replaced);
                    out.insert(key.clone(), json!(frames));
                }
                _ => dropped += 1,
            },
            "native_artifacts" => match value {
                Value::Array(items) => {
                    out.insert("native_artifact_count".into(), json!(items.len()));
                }
                _ => dropped += 1,
            },
            _ => dropped += 1,
        }
    }
    let mut value = Value::Object(out);
    counts.merge(&request.redaction.redact_value_counted(&mut value));
    counts.merge(&scrub_value_counted(&mut value));
    let bytes = serde_json::to_vec_pretty(&value).ok()?;
    stats.fields_masked = stats.fields_masked.saturating_add(masked);
    stats.fields_dropped = stats.fields_dropped.saturating_add(dropped);
    stats.backtrace_lines_dropped = stats
        .backtrace_lines_dropped
        .saturating_add(backtrace_dropped);
    stats.backtrace_frames_masked = stats.backtrace_frames_masked.saturating_add(frames_masked);
    Some(bytes)
}

/// Admit crash reports from `root` (the crash directory, resolved) and `root/submitted` into
/// `crash_staging` (the second folder into `crash_staging/submitted`), re-emitted by
/// [`sanitize_report`]. Newest first, at most `max_reports`, each read by [`read_contained`] under
/// `max_report_bytes`, all reads sharing one [`IoBudget`] of `max_report_total_bytes`: every byte
/// actually read is charged, a file later rejected included, and once the budget is spent the
/// remaining candidates are left out and counted. Every report that is not admitted is counted in
/// the returned stats; none is ever copied raw. Each admitted report is written as
/// `report-0001.json`, `report-0002.json`, ... (a separate sequence in `submitted/`); the on-disk
/// name is never written to the archive or the manifest.
fn admit_reports(
    root: &Path,
    crash_staging: &Path,
    request: &ExportRequest<'_>,
    staged: &mut Staged,
    counts: &mut RedactionCounts,
) -> Result<ReportStats, String> {
    let mut stats = ReportStats::default();
    let mut candidates = Vec::new();
    let mut truncated = false;
    scan_level(
        root,
        false,
        &is_report_name,
        &mut candidates,
        &mut stats.unreadable,
        &mut truncated,
    );
    if let Some(dir) = submitted_dir(root) {
        scan_level(
            &dir,
            true,
            &is_report_name,
            &mut candidates,
            &mut stats.unreadable,
            &mut truncated,
        );
    }
    stats.scan_truncated = truncated;
    newest_first(&mut candidates);
    let allowed = usize::try_from(request.limits.max_reports).unwrap_or(0);
    stats.over_count_cap = candidates.len().saturating_sub(allowed);
    candidates.truncate(allowed);
    let per_file = request.limits.max_report_bytes;
    let total = request.limits.max_report_total_bytes;
    let mut budget = IoBudget::new(total);
    let mut plain_seq = 0u32;
    let mut submitted_seq = 0u32;
    let candidate_count = candidates.len();
    for (position, candidate) in candidates.into_iter().enumerate() {
        if budget.is_spent() {
            stats.over_total_cap = stats
                .over_total_cap
                .saturating_add(candidate_count.saturating_sub(position));
            break;
        }
        let dir = if candidate.submitted {
            root.join(SUBMITTED_DIR)
        } else {
            root.to_path_buf()
        };
        let read = read_contained(root, &dir, &candidate.name, per_file, &mut budget);
        stats.bytes_read = budget.charged(total);
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(ReadFail::OverBudget) => {
                stats.over_total_cap = stats.over_total_cap.saturating_add(1);
                continue;
            }
            Err(ReadFail::TooLarge) => {
                stats.too_large = stats.too_large.saturating_add(1);
                continue;
            }
            Err(ReadFail::Unreadable) => {
                stats.unreadable = stats.unreadable.saturating_add(1);
                continue;
            }
        };
        let Some(clean) = sanitize_report(&bytes, request, counts, &mut stats) else {
            stats.unparseable = stats.unparseable.saturating_add(1);
            continue;
        };
        let (target_dir, sequence) = if candidate.submitted {
            (crash_staging.join(SUBMITTED_DIR), &mut submitted_seq)
        } else {
            (crash_staging.to_path_buf(), &mut plain_seq)
        };
        *sequence = sequence
            .checked_add(1)
            .ok_or_else(|| "too many export files".to_string())?;
        let file_name = format!("report-{:04}.json", *sequence);
        fs::create_dir_all(&target_dir)
            .map_err(|_| "the export staging could not be created".to_string())?;
        staged.count(clean.len())?;
        write_staged(&target_dir, &file_name, &clean)?;
        stats.included = stats.included.saturating_add(1);
    }
    Ok(stats)
}

fn remove_stale_staging(root: &Path, names: &ExportNames) {
    let prefix = format!(".{}-run-", names.staging_dir);
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    // Every attempt counts, an unreadable entry included, so the scan is bounded.
    for entry in entries.take(MAX_STALE_ENTRIES_SCANNED).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // A dead run, or the fixed-name staging folder an earlier release left behind.
        if !(name.starts_with(&prefix) || name == names.staging_dir) {
            continue;
        }
        // `DirEntry::file_type` does not follow a link; a link is never removed through.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let _ = remove_tree(
            &entry.path(),
            &RemoveOptions::new().min_age(STALE_RUN_AFTER),
        );
    }
}

fn publish(temporary: &Path, final_path: &Path) -> Result<(), String> {
    rightkit_fs::atomic::publish_file(temporary, final_path)
        .map_err(|_| "a diagnostics file could not be published".to_string())
}

/// Build the archive and return its path. See the module documentation for the contents.
///
/// `Local` returns the existing archive without rebuilding when the content fingerprint equals the
/// one saved by the last local export. Blocking; call from a worker thread. One call at a time per
/// `root` is not required: each call stages in its own directory and publishes atomically.
pub fn export_archive(request: &ExportRequest<'_>) -> Result<PathBuf, String> {
    request.validate()?;
    fs::create_dir_all(request.root)
        .map_err(|_| "the export directory could not be created".to_string())?;
    remove_stale_staging(request.root, request.names);

    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let run_dir = request.root.join(format!(
        ".{}-run-{}-{seq}",
        request.names.staging_dir,
        std::process::id()
    ));
    fs::create_dir(&run_dir).map_err(|_| "the export staging could not be created".to_string())?;
    let result = build(request, &run_dir, seq);
    let _ = remove_tree(&run_dir, &RemoveOptions::new());
    result
}

fn build(request: &ExportRequest<'_>, run_dir: &Path, seq: u64) -> Result<PathBuf, String> {
    let names = request.names;
    let upload = request.mode == ExportMode::Upload;
    let staging = run_dir.join(&names.staging_dir);
    fs::create_dir(&staging).map_err(|_| "the export staging could not be created".to_string())?;
    let zip_name = if upload {
        format!(
            "{}{}-{seq}.zip",
            names.upload_archive_prefix,
            std::process::id()
        )
    } else {
        names.archive.clone()
    };
    if !name_ok(&zip_name, 160, false) {
        return Err("export names are invalid".to_string());
    }
    let final_zip = request.root.join(&zip_name);
    let fingerprint_path = request.root.join(&names.fingerprint);
    let temporary_zip = run_dir.join("archive.tmp");
    let temporary_fingerprint = run_dir.join("fingerprint.tmp");

    let mut staged = Staged {
        included: Vec::new(),
        bytes: 0,
        max_bytes: request.limits.max_staged_bytes,
    };
    let mut counts = RedactionCounts::default();
    let mut dropped_lines = 0usize;

    for log in request.logs {
        for source in [&log.current, &log.rotated] {
            let Some(bytes) = read_tail(source, request.limits.log_tail_bytes)? else {
                continue;
            };
            let name = file_name_of(source)?;
            let (clean, dropped) = sanitize_log(&bytes, request, &mut counts);
            dropped_lines = dropped_lines.saturating_add(dropped);
            staged.add(&name, clean.len())?;
            write_staged(&staging, &name, &clean)?;
        }
    }

    let summary = serde_json::to_vec_pretty(request.summary)
        .map_err(|_| "the diagnostics summary could not be serialized".to_string())?;
    staged.add(request.summary_name, summary.len())?;
    write_staged(&staging, request.summary_name, &summary)?;

    let mut native_left_out = 0usize;
    let mut native_included = 0usize;
    let mut report_stats = ReportStats::default();
    // The crash directory is resolved once; every read below is contained to the resolved path.
    if let Some(root) = request
        .crash_dir
        .and_then(|dir| fs::canonicalize(dir).ok())
        .filter(|root| root.is_dir())
    {
        // Reports: every mode, always re-emitted redacted, never copied raw.
        report_stats = admit_reports(
            &root,
            &staging.join("crash"),
            request,
            &mut staged,
            &mut counts,
        )?;
        if request.include_native_dumps {
            (native_included, native_left_out) =
                admit_native_artifacts(&root, &staging.join("crash"), request, &mut staged)?;
        }
    }
    staged.included.sort();

    let (fingerprint, staged_total) = content_fingerprint(&staging)?;
    if staged_total > request.limits.max_staged_bytes {
        return Err("diagnostics exceed the staged size limit".to_string());
    }
    if !upload
        && final_zip.is_file()
        && read_archive_bounded(&fingerprint_path, MAX_FINGERPRINT_FILE_BYTES)
            .map(|saved| String::from_utf8_lossy(&saved).trim() == fingerprint)
            .unwrap_or(false)
    {
        return Ok(final_zip);
    }

    let mut manifest = request.manifest_fields.clone();
    manifest.insert("schema_version".into(), json!(1));
    manifest.insert("exported_at_ms".into(), json!(request.exported_at_ms));
    manifest.insert("included_files".into(), json!(staged.included));
    manifest.insert("content_fingerprint".into(), json!(fingerprint));
    manifest.insert(
        "redactions".into(),
        serde_json::to_value(counts).unwrap_or(Value::Null),
    );
    manifest.insert("log_lines_dropped".into(), json!(dropped_lines));
    manifest.insert("native_files_included".into(), json!(native_included));
    manifest.insert("native_files_left_out".into(), json!(native_left_out));
    manifest.insert("crash_reports".into(), report_stats.manifest_value());
    let manifest = serde_json::to_vec_pretty(&Value::Object(manifest))
        .map_err(|_| "the diagnostics manifest could not be serialized".to_string())?;
    write_staged(&staging, MANIFEST_NAME, &manifest)?;

    create_zip(&staging, &temporary_zip)?;
    let archive_length = fs::metadata(&temporary_zip)
        .map_err(|_| "the archive could not be read".to_string())?
        .len();
    if archive_length > request.limits.max_archive_bytes {
        return Err("the archive exceeds the size limit".to_string());
    }
    publish(&temporary_zip, &final_zip)?;
    if !upload {
        fs::write(&temporary_fingerprint, &fingerprint)
            .map_err(|_| "the fingerprint could not be written".to_string())?;
        publish(&temporary_fingerprint, &fingerprint_path)?;
    }
    Ok(final_zip)
}
