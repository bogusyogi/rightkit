//! Cleanup findings and the Move-to-Trash flow for the hub.
//!
//! Rules live in `rules/cleanup.json` (this crate's copy of the Pulse pack) (data, not code). `scan` expands each
//! rule's path patterns on disk, measures what it finds and decides whether
//! each item may be moved. `apply` re-checks every item immediately before
//! the move and records what happened in an activity log so it can be put
//! back. Nothing here deletes anything: the only effect is a move to Trash
//! (supplied by the caller) and, for Restore, a move back.
//!
//! Unknown is never eligible: an item whose owner cannot be determined, or
//! whose owner is running, is reported but not offered.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::store::StateStore;

pub const PACK_JSON: &str = include_str!("../rules/cleanup.json");
pub const ACTIVITY_SCHEMA_VERSION: u32 = 1;
const MAX_ENTRIES_PER_ITEM: usize = 400_000;
/// Longest the Windows held-open walk of one item may take; past it the item is
/// unknown, never eligible.
#[cfg(windows)]
const HELD_OPEN_BUDGET: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleRisk {
    /// Regenerates on its own; preselected when eligible.
    Safe,
    /// Needs a look; never preselected.
    Review,
    /// Explanation only; never eligible.
    Info,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LivenessCheck {
    /// In use when any listed owner process is running.
    Owners,
    /// Owners, plus an owner guessed from the item's own name.
    Name,
    /// Not process-bound; age is the only check.
    Age,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Measurement {
    /// Allocated blocks, as `du` counts them.
    #[default]
    Allocated,
    /// Bytes no APFS clone shares: what deleting the item gives back.
    CloneAware,
}

/// The rule whose items are Chrome's leftover signing snapshots.
pub const CHROME_SNAPSHOT_RULE: &str = "chrome-signing-copies";
/// Most files whose private size is read in one scan; the rest stay unmeasured.
const CLONE_AWARE_BUDGET: usize = 600_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CleanupRule {
    pub id: String,
    pub name: String,
    pub category: String,
    pub risk: RuleRisk,
    pub paths: Vec<String>,
    #[serde(default)]
    pub exclude_names: Vec<String>,
    pub liveness: LivenessCheck,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub min_age_days: Option<u64>,
    /// For a `name` rule: days nothing inside the item may have been written.
    /// A process-name match is only evidence that an owner IS running; no match
    /// proves nothing, so a `name` item is eligible only when it is also quiet
    /// this long (recursive newest modified time). Unknown stays ineligible.
    #[serde(default)]
    pub quiet_days: Option<u64>,
    #[serde(default)]
    pub keep_newest: usize,
    pub reason: String,
    pub action: String,
    #[serde(default)]
    pub measurement: Measurement,
    /// Build outputs sit inside projects, so no fixed path names them. A
    /// rule with `discover` finds directories by name and project marker.
    #[serde(default)]
    pub discover: Option<Discover>,
    /// Operating systems the rule applies to: `"macos"`, `"windows"`,
    /// `"linux"`. Empty means every system except Windows (the original
    /// macOS pack), so a rule is never silently run on a platform whose
    /// paths and process names it was not written for.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Name each finding after its parent folder too ("Default · Cache"):
    /// many browser profiles hold a folder with the same name.
    #[serde(default)]
    pub label_parent: bool,
}

fn current_platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn applies_here(rule: &CleanupRule) -> bool {
    if rule.platforms.is_empty() {
        !cfg!(windows)
    } else {
        rule.platforms.iter().any(|p| p == current_platform())
    }
}

/// Find directories called `dir` next to one of the `markers` (for example
/// `node_modules` beside `package.json`).
///
/// The idea of treating project build outputs as findings comes from Petal's
/// `findings.rs` (MIT, <https://github.com/henrydennis/petal>): its walk stays
/// out of hidden folders, `Library`, `Applications` and app bundles because
/// those `node_modules` belong to apps and cannot be reinstalled. This is a
/// fresh implementation of that rule, not a copy of the code.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Discover {
    pub dir: String,
    pub markers: Vec<String>,
}

const DISCOVER_MAX_DEPTH: usize = 8;
const DISCOVER_MAX_ENTRIES: usize = 400_000;
const BUNDLE_EXTENSIONS: &[&str] = &[
    "app",
    "appex",
    "framework",
    "bundle",
    "plugin",
    "xpc",
    "kext",
    "photoslibrary",
    "musiclibrary",
    "fcpbundle",
    "pkg",
    "mpkg",
    "xcarchive",
];

/// Folders a project search never enters: where apps and the system keep
/// their own trees, and media libraries.
fn not_a_project_area(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "Library" | "Applications" | "Movies" | "Music" | "Pictures" | "AppData"
        )
        // Cloud-synced folders: deleting there deletes the cloud copy too.
        || name.starts_with("OneDrive")
        || name.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && BUNDLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
        })
}

fn has_marker(parent: &Path, d: &Discover) -> bool {
    d.markers.iter().any(|m| {
        if m.contains('*') {
            fs::read_dir(parent).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_str().is_some_and(|n| wild(m, n)))
            })
        } else {
            parent.join(m).symlink_metadata().is_ok()
        }
    })
}

fn discover_dirs(home: &Path, d: &Discover) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut stack: Vec<(PathBuf, usize)> = vec![(home.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > DISCOVER_MAX_ENTRIES {
                return found;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name == d.dir && has_marker(&dir, d) {
                found.push(path);
            } else if !not_a_project_area(&name)
                && name != "node_modules"
                && depth < DISCOVER_MAX_DEPTH
            {
                stack.push((path, depth + 1));
            }
        }
    }
    found
}

/// Whether `path` is something `discover_dirs` would report, checked again
/// at move time.
fn is_discovered(path: &Path, home: &Path, d: &Discover) -> bool {
    let Ok(rest) = path.strip_prefix(home) else {
        return false;
    };
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some((last, ancestors)) = parts.split_last() else {
        return false;
    };
    last == &d.dir
        && ancestors.len() <= DISCOVER_MAX_DEPTH + 1
        && !ancestors
            .iter()
            .any(|a| not_a_project_area(a) || a == "node_modules")
        && path.parent().is_some_and(|p| has_marker(p, d))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CleanupPack {
    pub schema_version: u32,
    pub pack_version: u32,
    pub rules: Vec<CleanupRule>,
}

pub fn load_pack() -> Result<CleanupPack, String> {
    let pack: CleanupPack = serde_json::from_str(PACK_JSON).map_err(|e| e.to_string())?;
    if pack.schema_version != 1 {
        return Err(format!(
            "unsupported cleanup pack schema {}",
            pack.schema_version
        ));
    }
    Ok(pack)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Finding {
    pub id: String,
    pub rule_id: String,
    /// The rule's own name: what a group of findings is called.
    #[serde(default)]
    pub rule_name: String,
    pub category: String,
    pub name: String,
    pub path: String,
    /// What moving it can give back. For clone-aware rules this is the
    /// unshared part only; 0 when that could not be measured.
    pub bytes: u64,
    /// Allocated size as `du` shows it, counting blocks shared with clones.
    /// Equals `bytes` for rules that are not clone-aware.
    #[serde(default)]
    pub apparent_bytes: u64,
    /// The size is a lower bound because the walk hit its entry limit.
    pub partial: bool,
    /// `bytes` is the most deleting this would free, not a promise: a file with
    /// more than one hard link, or a file whose APFS clone sharing was not
    /// measured, may free less. Absent in older findings, where it reads false.
    #[serde(default)]
    pub upper_bound: bool,
    pub risk: RuleRisk,
    pub eligible: bool,
    pub preselected: bool,
    pub reason: String,
    pub action: String,
    /// Decimal strings: inode numbers can exceed what a JS number holds.
    pub dev: String,
    pub ino: String,
    /// Plain-language kind of item ("App cache", "RustRight build target").
    /// Empty in findings saved before this field existed.
    #[serde(default)]
    pub kind: String,
    /// Plain-language safety ("rebuildable cache", "experiment output",
    /// "virtual machine", "old release build"). Empty when not described.
    #[serde(default)]
    pub safety: String,
    /// Who made the item: the owning app, tool or project. Empty when unknown.
    #[serde(default)]
    pub owner: String,
    /// Unix seconds the item was last written, or 0 when unknown.
    #[serde(default)]
    pub last_used: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Report {
    #[serde(default)]
    pub chrome_snapshots: Option<ChromeSnapshots>,
    pub findings: Vec<Finding>,
    pub safe_bytes: u64,
    pub review_bytes: u64,
    pub scanned_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ChromeSnapshots {
    pub count: u64,
    pub apparent_bytes: u64,
    /// Unshared bytes; only meaningful when `reclaimable_known`.
    pub reclaimable_bytes: u64,
    pub reclaimable_known: bool,
    /// A Chrome-family process is running, so none are offered.
    pub running: bool,
    /// Count and time of the earlier sample the change is measured from.
    pub since_at: Option<u64>,
    pub since_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CountSample {
    at: u64,
    count: u64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CountLog {
    #[serde(default)]
    samples: Vec<CountSample>,
}

const COUNT_SAMPLE_GAP_SECS: u64 = 86_400;
const COUNT_WINDOW_SECS: u64 = 30 * 86_400;
const COUNT_KEEP: usize = 60;

/// Name of the per-app folder (under `<data dir>/Orthic Labs/`) where the cleanup scan keeps its
/// own small state, such as the browser snapshot count log. Set once by the host app with
/// [`set_state_app_name`]; defaults to [`DEFAULT_STATE_APP_NAME`].
static STATE_APP_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The per-app folder name used when the host app has not called [`set_state_app_name`].
pub const DEFAULT_STATE_APP_NAME: &str = "RightKit";

/// Name the host app's per-app state folder (for example the product name it installs under).
/// Only the first call takes effect; returns `false` when a name was already set.
pub fn set_state_app_name(name: &str) -> bool {
    STATE_APP_NAME.set(name.to_owned()).is_ok()
}

fn state_app_name() -> &'static str {
    STATE_APP_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_STATE_APP_NAME)
}

/// Where the host app keeps its own state for this user: RightKit's per-app data root under `home`
/// (`<data dir>/Orthic Labs/<app>`), named without creating it (this scan is read-only).
fn state_dir(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("AppData")
            .join("Local")
            .join("Orthic Labs")
            .join(state_app_name())
    } else {
        home.join("Library/Application Support/Orthic Labs")
            .join(state_app_name())
    }
}

fn count_log_path(home: &Path) -> PathBuf {
    state_dir(home).join("chrome-snapshots.json")
}

/// Add a sample when the last one is a day old or more, and pick the baseline
/// the change is shown from: the oldest earlier sample inside 30 days.
fn track_count(log: &mut CountLog, count: u64, now: u64) -> (bool, Option<CountSample>) {
    let due = log
        .samples
        .last()
        .is_none_or(|last| now >= last.at.saturating_add(COUNT_SAMPLE_GAP_SECS));
    let baseline = log
        .samples
        .iter()
        .find(|s| s.at < now && now - s.at <= COUNT_WINDOW_SECS)
        .cloned();
    if due {
        log.samples.push(CountSample { at: now, count });
        let extra = log.samples.len().saturating_sub(COUNT_KEEP);
        log.samples.drain(..extra);
    }
    (due, baseline)
}

fn record_chrome_count(home: &Path, count: u64, now: u64) -> Option<CountSample> {
    let path = count_log_path(home);
    let mut log: CountLog = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let (due, baseline) = track_count(&mut log, count, now);
    if due && (count > 0 || log.samples.len() > 1) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(body) = serde_json::to_vec_pretty(&log) {
            let _ = rightkit_fs::atomic_write(&path, &body);
        }
    }
    baseline
}

/// Where `apply` gets the running-process list. It is called again for every item,
/// right before that item is re-checked, so a program started after the review is
/// seen. An `Err` is unknown liveness: the item is skipped, never moved.
pub type RunningProvider<'a> = dyn FnMut() -> Result<Vec<String>, String> + 'a;

/// The live process list as a [`RunningProvider`] answer: `Err` when the system
/// reports no process at all (the list could not be read), because an empty list
/// is not proof that nothing is running.
pub fn running_process_list() -> Result<Vec<String>, String> {
    let names = running_process_names();
    if names.is_empty() {
        return Err("Can't tell which programs are running".into());
    }
    Ok(names)
}

/// Names (lowercased) of running processes; the liveness evidence.
pub fn running_process_names() -> Vec<String> {
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .map(|p| {
            let name = p.name().to_string_lossy().to_lowercase();
            // Windows names carry ".exe"; owners are written without it.
            name.strip_suffix(".exe").map(str::to_owned).unwrap_or(name)
        })
        .collect()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `*` matches any run of characters inside one path segment.
fn wild(pattern: &str, value: &str) -> bool {
    // NTFS names are case-insensitive.
    let fold = |text: &str| -> Vec<char> {
        if cfg!(windows) {
            text.to_lowercase().chars().collect()
        } else {
            text.chars().collect()
        }
    };
    let p = fold(pattern);
    let v = fold(value);
    let (mut pi, mut vi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while vi < v.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if pi < p.len() && p[pi] == v[vi] {
            pi += 1;
            vi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// A Windows environment folder, kept inside `home`: the value of the
/// variable when it lies under the user's profile, else the standard
/// location under it. Findings therefore never leave the profile, and
/// `apply` re-resolves to the same place the scan did.
fn windows_var(name: &str, home: &Path) -> Option<String> {
    let default = match name.to_ascii_uppercase().as_str() {
        "USERPROFILE" => return Some(home.display().to_string()),
        "LOCALAPPDATA" => home.join("AppData").join("Local"),
        "APPDATA" => home.join("AppData").join("Roaming"),
        "TEMP" | "TMP" => home.join("AppData").join("Local").join("Temp"),
        _ => return None,
    };
    let from_env = std::env::var_os(name).map(PathBuf::from).filter(|value| {
        let value = value.to_string_lossy().to_lowercase().replace('\\', "/");
        let home = home.to_string_lossy().to_lowercase().replace('\\', "/");
        let home = home.trim_end_matches('/');
        value.starts_with(&format!("{home}/"))
    });
    Some(from_env.unwrap_or(default).display().to_string())
}

fn resolve_pattern(pattern: &str, home: &Path) -> String {
    let mut text = match pattern.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", home.display(), rest),
        None => pattern.to_string(),
    };
    // `%NAME%` environment folders (Windows rules only).
    while let Some(open) = text.find('%') {
        let Some(len) = text[open + 1..].find('%') else {
            break;
        };
        let name = &text[open + 1..open + 1 + len];
        let Some(value) = windows_var(name, home) else {
            break;
        };
        text.replace_range(open..open + len + 2, &value);
    }
    text
}

/// Root to start from and the path segments of a resolved pattern. On
/// Windows both separators are accepted and a leading `C:` becomes the root.
fn split_pattern(full: &str) -> (PathBuf, Vec<String>) {
    let normal = if cfg!(windows) {
        full.replace('\\', "/")
    } else {
        full.to_string()
    };
    let mut segments = normal
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .peekable();
    if cfg!(windows)
        && let Some(drive) = segments
            .peek()
            .filter(|s| s.len() == 2 && s.ends_with(':'))
            .cloned()
    {
        segments.next();
        return (PathBuf::from(format!("{drive}\\")), segments.collect());
    }
    (PathBuf::from("/"), segments.collect())
}

fn expand(pattern: &str, home: &Path) -> Vec<PathBuf> {
    let (root, segments) = split_pattern(&resolve_pattern(pattern, home));
    let mut current = vec![root];
    for segment in &segments {
        let mut next = Vec::new();
        for base in &current {
            if segment.contains('*') {
                if let Ok(entries) = fs::read_dir(base) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        if name.to_str().is_some_and(|n| wild(segment, n)) {
                            next.push(base.join(name));
                        }
                    }
                }
            } else {
                let path = base.join(segment);
                if path.symlink_metadata().is_ok() {
                    next.push(path);
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

fn path_matches(pattern: &str, path: &Path, home: &Path) -> bool {
    let (_, want) = split_pattern(&resolve_pattern(pattern, home));
    let have: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(|s| s.to_string()),
            _ => None,
        })
        .collect();
    want.len() == have.len() && want.iter().zip(have.iter()).all(|(p, v)| wild(p, v))
}

fn excluded(rule: &CleanupRule, name: &str) -> bool {
    name == ".DS_Store"
        || name == ".localized"
        || name.eq_ignore_ascii_case("desktop.ini")
        || rule.exclude_names.iter().any(|pattern| wild(pattern, name))
}

#[cfg(unix)]
fn identity(md: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(not(unix))]
fn identity(_md: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

#[cfg(unix)]
fn allocated(md: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(md: &fs::Metadata) -> u64 {
    md.len()
}

#[cfg(unix)]
fn link_count(md: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.nlink()
}

#[cfg(not(unix))]
fn link_count(_md: &fs::Metadata) -> u64 {
    1
}

/// Cloud placeholders (macOS "dataless" files) are never touched.
#[cfg(target_os = "macos")]
fn is_dataless(md: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    md.st_flags() & 0x4000_0000 != 0
}

/// Windows: offline, recall-on-open and recall-on-data-access items are cloud
/// placeholders; any other reparse point (junction, dedup, WIM) has semantics
/// the app does not know, so it is left alone too.
#[cfg(windows)]
fn is_dataless(md: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & (0x0400 | 0x1000 | 0x0004_0000 | 0x0040_0000) != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
fn is_dataless(_md: &fs::Metadata) -> bool {
    false
}

/// Windows: whether another program holds `path` open so that a move would
/// fail or break it. Asks for DELETE access without recalling anything; a
/// sharing violation means in use, and an answer that cannot be had (access
/// denied) counts as in use too, because unknown is never eligible.
#[cfg(windows)]
fn held_open_one(path: &Path) -> Option<String> {
    use std::os::windows::fs::OpenOptionsExt;
    const DELETE: u32 = 0x0001_0000;
    const READ_ATTRIBUTES: u32 = 0x0000_0080;
    const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const OPEN_NO_RECALL: u32 = 0x0010_0000;
    match fs::OpenOptions::new()
        .access_mode(DELETE | READ_ATTRIBUTES)
        .share_mode(SHARE_READ_WRITE)
        .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT | OPEN_NO_RECALL)
        .open(path)
    {
        Ok(_) => None,
        Err(error) => match error.raw_os_error() {
            // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND: gone, nothing to hold.
            Some(2) | Some(3) => None,
            // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
            Some(32) | Some(33) => Some("In use by another program".into()),
            _ => Some("Can't tell whether another program is using this".into()),
        },
    }
}

/// Windows: the held-open probe over the item and everything inside it. Any file
/// held open, any entry the probe cannot answer for, and a tree too big to walk
/// all count as in use: liveness is proven only by a complete, clean walk.
#[cfg(windows)]
fn held_open(path: &Path) -> Option<String> {
    let mut stack = vec![path.to_path_buf()];
    let mut seen = 0usize;
    while let Some(current) = stack.pop() {
        seen += 1;
        if seen > MAX_ENTRIES_PER_ITEM {
            return Some("Too many files to check whether any is in use".into());
        }
        if let Some(why) = held_open_one(&current) {
            return Some(why);
        }
        let Ok(md) = current.symlink_metadata() else {
            continue;
        };
        if md.is_dir() && !md.file_type().is_symlink() {
            match fs::read_dir(&current) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => stack.push(entry.path()),
                            Err(_) => {
                                return Some(
                                    "Can't tell whether another program is using this".into(),
                                );
                            }
                        }
                    }
                }
                Err(error)
                    if error.raw_os_error() == Some(2) || error.raw_os_error() == Some(3) => {}
                Err(_) => {
                    return Some("Can't tell whether another program is using this".into());
                }
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn held_open(_path: &Path) -> Option<String> {
    None
}

/// Newest modified time (Unix seconds) of `path` and everything inside it,
/// without following links. A folder's own time moves only when a direct child
/// changes, so a deep write is invisible to it. `None` when the walk could not
/// be completed (unreadable entry, more than `cap` entries): unknown, never old.
/// With `stop_after`, returns as soon as anything newer than it is seen.
pub fn newest_modified(path: &Path, cap: usize, stop_after: Option<u64>) -> Option<u64> {
    let mut newest = 0u64;
    let mut stack = vec![path.to_path_buf()];
    let mut seen = 0usize;
    while let Some(current) = stack.pop() {
        seen += 1;
        if seen > cap {
            return None;
        }
        let md = current.symlink_metadata().ok()?;
        let modified = md
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        newest = newest.max(modified);
        if stop_after.is_some_and(|limit| newest > limit) {
            return Some(newest);
        }
        if md.is_dir() && !md.file_type().is_symlink() {
            for entry in fs::read_dir(&current).ok()? {
                stack.push(entry.ok()?.path());
            }
        }
    }
    Some(newest)
}

/// Whole days since anything in the item was written. A file is its own modified
/// time; a folder is its newest entry. `None` is unknown.
fn deep_age_days(path: &Path, md: &fs::Metadata, at_least: u64) -> Option<u64> {
    if !md.is_dir() {
        return age_days(md);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let cutoff = now.saturating_sub(at_least.saturating_mul(86_400));
    let newest = newest_modified(path, MAX_ENTRIES_PER_ITEM, Some(cutoff))?;
    Some(now.saturating_sub(newest) / 86_400)
}

/// Beyond the process check: a `name` rule's item must also be quiet, because
/// no matching process is not proof that nothing owns it.
fn proven_idle(rule: &CleanupRule, path: &Path, md: &fs::Metadata) -> Result<(), String> {
    if rule.liveness != LivenessCheck::Name {
        return Ok(());
    }
    let unknown = || "Can't tell whether its app is using this".to_string();
    let quiet = rule.quiet_days.ok_or_else(unknown)?;
    match deep_age_days(path, md, quiet) {
        Some(age) if age >= quiet => Ok(()),
        Some(_) => Err("Written recently; may be in use".into()),
        None => Err(unknown()),
    }
}

/// Allocated bytes under `path` without following symlinks, and whether the
/// walk stopped early.
fn measure(path: &Path) -> (u64, bool) {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(md) = current.symlink_metadata() else {
            continue;
        };
        total = total.saturating_add(allocated(&md));
        seen += 1;
        if seen >= MAX_ENTRIES_PER_ITEM {
            return (total, true);
        }
        if md.is_dir()
            && let Ok(entries) = fs::read_dir(&current)
        {
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        }
    }
    (total, false)
}

/// Apparent (allocated) bytes under `path` and, when the file system can say,
/// the bytes no clone shares. The second value is `None` if any file could not
/// be asked or the budget ran out: a partial sum would understate, so the
/// answer is "unknown", never a smaller number. The third value is true when a
/// file in the tree has more than one hard link, so deleting it frees less
/// than its size.
fn measure_clone_aware(path: &Path, budget: &mut usize) -> (u64, Option<u64>, bool) {
    let mut apparent = 0u64;
    let mut linked = false;
    let mut unique = Some(0u64);
    let mut seen = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(md) = current.symlink_metadata() else {
            continue;
        };
        apparent = apparent.saturating_add(allocated(&md));
        seen += 1;
        if seen >= MAX_ENTRIES_PER_ITEM {
            unique = None;
            break;
        }
        if md.is_file() {
            linked |= link_count(&md) > 1;
            if *budget == 0 {
                unique = None;
            } else {
                *budget -= 1;
                match (unique, crate::platform::private_size(&current)) {
                    (Some(total), Some(bytes)) => unique = Some(total.saturating_add(bytes)),
                    _ => unique = None,
                }
            }
        } else if md.is_dir()
            && let Ok(entries) = fs::read_dir(&current)
        {
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        }
    }
    (apparent, unique, linked)
}

fn age_days(md: &fs::Metadata) -> Option<u64> {
    let modified = md.modified().ok()?;
    let elapsed = SystemTime::now().duration_since(modified).ok()?;
    Some(elapsed.as_secs() / 86_400)
}

fn name_matches(process: &str, owner: &str) -> bool {
    let owner = owner.to_lowercase();
    process == owner
        || process.starts_with(&format!("{owner} "))
        || process.starts_with(&format!("{owner}-"))
        || process.starts_with(&format!("{owner}("))
}

/// Some(process) when an owner is running, None when not, and Err when the
/// rule cannot say who owns the item (unknown).
fn in_use(rule: &CleanupRule, name: &str, running: &[String]) -> Result<Option<String>, String> {
    let mut owners: Vec<String> = rule.owners.clone();
    match rule.liveness {
        LivenessCheck::Age => {
            // Windows age rules name no owner and the held-open probe cannot say
            // which program will use a file next: old is not proof of idle, so
            // an ownerless Windows age item is reported and never offered.
            if owners.is_empty() && rule.platforms.iter().any(|p| p == "windows") {
                return Err("Can't tell which app owns this".into());
            }
        }
        LivenessCheck::Owners => {
            if owners.is_empty() {
                return Err("Can't tell which app owns this".into());
            }
        }
        LivenessCheck::Name => {
            owners.push(name.to_string());
            for token in name.split('.') {
                if token.len() >= 4 {
                    owners.push(token.to_string());
                }
            }
        }
    }
    for owner in &owners {
        if let Some(process) = running.iter().find(|p| name_matches(p, owner)) {
            return Ok(Some(process.clone()));
        }
    }
    Ok(None)
}

fn finding_id(rule_id: &str, path: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rule_id.hash(&mut hasher);
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

pub fn scan(home: &Path, running: &[String]) -> Result<Report, String> {
    let pack = load_pack()?;
    let mut report = Report {
        scanned_at: now_millis() / 1000,
        ..Report::default()
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut clone_budget = CLONE_AWARE_BUDGET;
    let mut snapshots = ChromeSnapshots {
        reclaimable_known: true,
        ..ChromeSnapshots::default()
    };
    let mut snapshots_seen = false;

    for rule in &pack.rules {
        if !applies_here(rule) {
            continue;
        }
        let groups: Vec<Vec<PathBuf>> = match &rule.discover {
            Some(d) => vec![discover_dirs(home, d)],
            None => rule.paths.iter().map(|p| expand(p, home)).collect(),
        };
        for group in groups {
            let mut matched: Vec<(PathBuf, fs::Metadata)> = Vec::new();
            for path in group {
                let name = display_name(&path);
                if excluded(rule, &name) || seen.contains(&path) {
                    continue;
                }
                let Ok(md) = path.symlink_metadata() else {
                    continue;
                };
                if md.file_type().is_symlink() {
                    continue;
                }
                matched.push((path, md));
            }
            if rule.keep_newest > 0 {
                matched.sort_by_key(|(_, md)| {
                    std::cmp::Reverse(md.modified().ok().unwrap_or(UNIX_EPOCH))
                });
                let _ = matched.drain(..rule.keep_newest.min(matched.len()));
            }
            if rule.id == CHROME_SNAPSHOT_RULE {
                snapshots_seen = true;
                snapshots.running = matches!(in_use(rule, "", running), Ok(Some(_)));
            }
            for (path, md) in matched {
                if let Some(min) = rule.min_age_days {
                    match deep_age_days(&path, &md, min) {
                        Some(age) if age >= min => {}
                        _ => continue,
                    }
                }
                let clone_aware = rule.measurement == Measurement::CloneAware;
                let (bytes, apparent_bytes, partial, upper_bound) = if clone_aware {
                    let (apparent, unique, linked) = measure_clone_aware(&path, &mut clone_budget);
                    if rule.id == CHROME_SNAPSHOT_RULE {
                        snapshots.count += 1;
                        snapshots.apparent_bytes += apparent;
                        match unique {
                            Some(u) => snapshots.reclaimable_bytes += u,
                            None => snapshots.reclaimable_known = false,
                        }
                    }
                    (
                        unique.unwrap_or(0),
                        apparent,
                        unique.is_none(),
                        unique.is_none() || linked,
                    )
                } else {
                    // Allocated size only: clone sharing and hard links were not
                    // looked at, so this is an upper bound.
                    let (b, p) = measure(&path);
                    (b, b, p, true)
                };
                if bytes == 0 && !clone_aware && rule.risk != RuleRisk::Info {
                    continue;
                }
                let name = display_name(&path);
                let (dev, ino) = identity(&md);
                let (eligible, reason) = judge(rule, &name, &path, &md, running);
                let (kind, safety) = describe(rule);
                seen.insert(path.clone());
                let path_text = path.to_string_lossy().into_owned();
                if eligible {
                    match rule.risk {
                        RuleRisk::Safe => report.safe_bytes += bytes,
                        _ => report.review_bytes += bytes,
                    }
                }
                report.findings.push(Finding {
                    id: finding_id(&rule.id, &path_text),
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    category: rule.category.clone(),
                    name: if rule.label_parent && !name.is_empty() {
                        match path.parent().map(display_name) {
                            Some(parent) if !parent.is_empty() => format!("{parent} · {name}"),
                            _ => name,
                        }
                    } else if rule.discover.is_some() {
                        let project = path.parent().map(display_name).unwrap_or_default();
                        format!("{} · {}", rule.name, project)
                    } else if rule.risk == RuleRisk::Info || name.is_empty() {
                        rule.name.clone()
                    } else {
                        name
                    },
                    path: path_text,
                    bytes,
                    apparent_bytes,
                    partial,
                    upper_bound,
                    risk: rule.risk,
                    eligible,
                    preselected: eligible && rule.risk == RuleRisk::Safe,
                    reason,
                    action: rule.action.clone(),
                    dev: dev.to_string(),
                    ino: ino.to_string(),
                    kind,
                    safety,
                    owner: if rule.owners.is_empty() {
                        owner_of(rule, &path)
                    } else {
                        rule.owners[0].clone()
                    },
                    last_used: md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs()),
                });
            }
        }
    }
    if snapshots_seen {
        let now = report.scanned_at;
        let baseline = record_chrome_count(home, snapshots.count, now);
        snapshots.since_at = baseline.as_ref().map(|b| b.at);
        snapshots.since_count = baseline.map(|b| b.count);
        if snapshots.count > 0 {
            report.chrome_snapshots = Some(snapshots);
        }
    }
    report
        .findings
        .sort_by(|a, b| a.category.cmp(&b.category).then(b.bytes.cmp(&a.bytes)));
    Ok(report)
}

/// Plain-language kind and safety for a rule's items. The pack's own rule ids
/// get their named words; any other rule falls back to its category and risk.
pub fn describe(rule: &CleanupRule) -> (String, String) {
    let (kind, safety) = match rule.id.as_str() {
        "user-caches" => ("App cache", "rebuildable cache"),
        "xcode-derived-data" => ("Xcode build data", "rebuildable cache"),
        "xcode-archives" => ("Xcode archive", "old release build"),
        "device-support" => ("Device support files", "rebuildable cache"),
        "simulator-caches" => ("Xcode simulator cache", "rebuildable cache"),
        "chrome-signing-copies" => ("Chrome update copy", "old copy, not in use"),
        "user-logs" | "win-crash-dumps" | "win-error-reports" | "win-browser-crash-reports" => {
            ("Logs and crash reports", "log files")
        }
        "download-installers"
        | "old-downloads"
        | "win-download-installers"
        | "win-old-downloads" => ("Download", "your file, needs a look"),
        "node-modules" | "cargo-target" | "swiftpm-build" | "win-dotnet-obj" | "win-dotnet-bin" => {
            ("Project build output", "rebuildable cache")
        }
        "simulators-info" => ("Simulator devices", "virtual machine"),
        "trash-info" => ("Trash", "already in Trash"),
        _ => {
            let safety = match rule.risk {
                RuleRisk::Safe => "rebuildable cache",
                RuleRisk::Review => "needs a look",
                RuleRisk::Info => "for information",
            };
            return (rule.category.clone(), safety.to_string());
        }
    };
    (kind.to_string(), safety.to_string())
}

/// Who made an item: the rule's first owner, or for a discovered build output
/// the project folder, otherwise the item's own name.
fn owner_of(rule: &CleanupRule, path: &Path) -> String {
    if rule.discover.is_some() {
        path.parent().map(display_name).unwrap_or_default()
    } else {
        display_name(path)
    }
}

/// Allocated bytes under `path`, counted as [`scan`](crate::scan::scan) counts them. For callers
/// outside the pack (the hub's own sources).
pub fn size_of(path: &Path) -> u64 {
    measure(path).0
}

fn judge(
    rule: &CleanupRule,
    name: &str,
    path: &Path,
    md: &fs::Metadata,
    running: &[String],
) -> (bool, String) {
    if rule.risk == RuleRisk::Info {
        return (false, rule.reason.clone());
    }
    if is_dataless(md) {
        return (false, "Stored in the cloud; not touched".into());
    }
    match in_use(rule, name, running) {
        Err(why) => (false, why),
        Ok(Some(process)) => (false, format!("In use: {process} is running")),
        Ok(None) => {
            if let Err(why) = proven_idle(rule, path, md) {
                return (false, why);
            }
            match held_open(path) {
                Some(why) => (false, why),
                None => (true, rule.reason.clone()),
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    pub rule_id: String,
    pub path: String,
    pub dev: String,
    pub ino: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActivityItem {
    pub path: String,
    pub trash_path: Option<String>,
    pub bytes: u64,
    #[serde(default)]
    pub restored: bool,
    /// Device and inode at apply time (macOS; absent where the platform has none).
    /// Restore moves back only the item that still has this identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<(u64, u64)>,
    /// True for a folder, false for a file, as seen just before the move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_dir: Option<bool>,
    /// A file's length in bytes just before the move (files only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    /// Modified time in Unix seconds just before the move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    /// Unix seconds just before and just after the move to the trash; a
    /// Recycle Bin entry's deletion time must fall inside it (plus a margin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash_window: Option<(u64, u64)>,
    /// Set when a crash left this item's move or restore unresolved and the
    /// real world could not settle it. Undo refuses the item and says why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<String>,
}

/// Margin around `trash_window` when comparing a Recycle Bin deletion time.
const BIN_TIME_MARGIN_SECS: i64 = 5;

/// Whether a Recycle Bin entry with the same original path can be this item:
/// its deletion time lies in the recorded window and, for a file whose bin
/// size is known, the size equals the recorded length. Items journalled before
/// these fields existed have no window or size and pass here; the caller then
/// accepts them only when they are the single entry with that path.
pub fn bin_entry_matches(item: &ActivityItem, deleted_at: i64, bin_size: Option<u64>) -> bool {
    if let Some((from, to)) = item.trash_window {
        let low = (from as i64).saturating_sub(BIN_TIME_MARGIN_SECS);
        let high = (to as i64).saturating_add(BIN_TIME_MARGIN_SECS);
        if deleted_at < low || deleted_at > high {
            return false;
        }
    }
    if item.is_dir == Some(false)
        && let (Some(want), Some(got)) = (item.file_size, bin_size)
    {
        return want == got;
    }
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Activity {
    pub id: String,
    /// Unix seconds.
    pub at: u64,
    pub bytes: u64,
    pub items: Vec<ActivityItem>,
    /// Items the run skipped (their move failed, or a crash showed it had not
    /// happened). Absent in older history, where it reads as 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped: usize,
    /// Items whose outcome a crash left unknown; each is also in `items` with
    /// `interrupted` set. Absent in older history, where it reads as 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unknown: usize,
    /// True when a crash interrupted this run and it was settled afterwards.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recovered: bool,
    /// Observed free space on each drive this run moved items off, read just
    /// before the first move and just after the last. Empty when it could not
    /// be read, and in older history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub free_space: Vec<VolumeFree>,
}

/// What the drive itself reported as available before and after a cleanup.
/// Observed, not computed from item sizes; other activity on the drive moves it
/// too. Items moved to the Trash stay on the same drive, so this normally does
/// not change until the Trash is emptied.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VolumeFree {
    /// The drive's name as a person would say it.
    pub volume: String,
    pub before_available: u64,
    pub after_available: u64,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ApplyResult {
    pub moved_items: usize,
    pub moved_bytes: u64,
    pub skipped: Vec<Skipped>,
    pub activity_id: Option<String>,
    /// Things that went wrong after an item was already moved (for example the
    /// history entry could not be saved); the journal still recovers them.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Observed change in free space per drive; see [`VolumeFree`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub free_space: Vec<VolumeFree>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RestoreResult {
    pub restored_items: usize,
    pub restored_bytes: u64,
    pub skipped: Vec<Skipped>,
    /// Earlier interrupted cleanups that could not be settled before this
    /// Undo; they are left as they were.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ActivityLog {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    actions: Vec<Activity>,
}

fn log_path(home: &Path) -> PathBuf {
    state_dir(home).join("cleanup-activity.json")
}

/// The history file. A missing file is an empty history; a file that cannot be
/// read or understood is an error, so nothing is ever written over it.
fn read_log(home: &Path) -> Result<ActivityLog, String> {
    let text = match fs::read_to_string(log_path(home)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ActivityLog::default());
        }
        Err(e) => return Err(format!("The cleanup history file could not be read ({e})")),
    };
    serde_json::from_str(&text).map_err(|e| format!("The cleanup history file is damaged ({e})"))
}

fn write_log(home: &Path, log: &ActivityLog) -> Result<(), String> {
    let path = log_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_vec_pretty(log).map_err(|e| e.to_string())?;
    rightkit_fs::atomic_write(&path, &body).map_err(|e| e.to_string())
}

/// Held while the history file is read, changed and written back, so two
/// processes (the CLI and the hub) never write over each other's entries. The
/// system releases it when the process ends.
struct LogLock {
    _lock: rightkit_fs::FileLock,
}

/// How long to wait for the history lock, and how often to try.
const LOG_LOCK_WAIT: Duration = Duration::from_secs(5);
const LOG_LOCK_STEP: Duration = Duration::from_millis(50);

/// Lock order everywhere: a run's lock first, then this one. Never take a run
/// lock while holding it.
fn lock_log(home: &Path) -> Result<LogLock, String> {
    let path = state_dir(home).join("cleanup-activity.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let options = rightkit_fs::LockOptions {
        poll: LOG_LOCK_STEP,
        ..rightkit_fs::LockOptions::wait(LOG_LOCK_WAIT)
    };
    match rightkit_fs::FileLock::acquire_kind(&path, rightkit_fs::LockKind::Flock, &options) {
        Ok(lock) => Ok(LogLock { _lock: lock }),
        Err(rightkit_fs::FsError::LockTimeout { .. }) => Err(
            "The cleanup history is busy: another app window is saving to \
                    it. Try again in a moment."
                .to_string(),
        ),
        Err(e) => Err(format!("The cleanup history could not be locked ({e})")),
    }
}

/// Change the history under its lock: read the newest file, let `change` edit
/// it (it returns whether anything changed), and write it back only then. The
/// file is always re-read inside the lock, so a change is merged into what
/// other processes saved, never put over it. `change` must not take a lock.
fn update_log(
    home: &Path,
    change: impl FnOnce(&mut ActivityLog) -> Result<bool, String>,
) -> Result<(), String> {
    let _lock = lock_log(home)?;
    let mut log = read_log(home)?;
    if change(&mut log)? {
        write_log(home, &log)?;
    }
    Ok(())
}

/// Past actions, newest first. Runs a crash left unresolved are settled first.
pub fn history(home: &Path) -> Result<Vec<Activity>, String> {
    history_with(home, &mut |item| trash_lookup(home, item))
}

/// Like [`history`], with the caller's way of finding an item in the
/// Recycle Bin (the core can only look in `~/.Trash`).
pub fn history_with(
    home: &Path,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
) -> Result<Vec<Activity>, String> {
    history_warned(home, lookup).map(|(actions, _)| actions)
}

/// [`history_with`] plus a plain-words warning for each earlier cleanup that
/// could not be settled (the CLI shows them).
pub fn history_warned(
    home: &Path,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
) -> Result<(Vec<Activity>, Vec<String>), String> {
    let warnings = match reconcile(home, lookup) {
        Ok(settled) => settled.warnings,
        Err(why) => vec![unsettled_warning(&why)],
    };
    let mut actions = read_log(home)?.actions;
    actions.reverse();
    Ok((actions, warnings))
}

// ---------------------------------------------------------------------------
// Write-ahead journal
//
// Every apply is one run, kept in a `StateStore` namespace under the app's state
// folder (versioned records, file and folder fsync, errors propagated). A run
// is split into chunk records of at most `CHUNK_ITEMS` items: chunk 0 is
// `run-<id>` (the one-time claim of the run id) and chunk k is `run-<id>-c<k>`.
// A chunk is rewritten as a new version before and after each of its items'
// effect, so a record is on disk when the call returns; once the new version
// is durable the chunk's older versions are removed, so a run costs space in
// proportion to its items, not to their square:
//
//   intent          written before the item is moved (path, identity, size,
//                   kind, modified time, start of the trash window)
//   moved           written after the move (trash path, end of the window)
//   skipped         the move failed or was found not to have happened
//   unknown         a crash left it unresolved and the real world cannot show
//                   what happened; Undo refuses it and never guesses
//   restore_intent  written before an Undo moves the item back
//   restored        written after it is back
//
// The run id is the activity id shown in history. `cleanup-activity.json`
// stays the readable history; it is derived from the journal and a run that
// crashed is put back into it by `reconcile`.
//
// Retention: after a successful apply (never during recovery) the journal
// records of runs beyond the newest `KEPT_SETTLED_RUNS` fully settled ones are
// removed, oldest first, but only runs with no item still in the Trash; the
// history file has no retention of its own.
//
// The process working on a run (an apply, or an Undo of it) holds an OS lock
// on `cleanup-locks/run-<id>.lock` for as long as it works; the system drops
// it when the process dies. `reconcile` never touches a run whose lock is
// held, however old its newest record is.
// ---------------------------------------------------------------------------

const JOURNAL_NAMESPACE: &str = "cleanup-journal";
/// The id of the list of runs that may still have unresolved items.
const OPEN_RUNS: &str = "open-runs";
/// Items per journal chunk record.
const CHUNK_ITEMS: usize = 64;
/// Besides a free lock, a run is left alone while its newest record is younger
/// than this.
pub const INTERRUPTED_AFTER_SECS: u64 = 600;
/// How long after its intent record a Recycle Bin deletion still counts as
/// that item's, when the move's own end time was never recorded.
pub const INTERRUPTED_WINDOW_SECS: u64 = 120;
/// A saved plan older than this is refused; an unresolved item that cannot be
/// checked for this long is marked unknown.
pub const PLAN_EXPIRY_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ItemState {
    Intent,
    Moved,
    Skipped,
    Unknown,
    RestoreIntent,
    Restored,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RunItem {
    item: ActivityItem,
    state: ItemState,
    #[serde(default)]
    reason: Option<String>,
    /// Unix seconds when `state` was written.
    #[serde(default)]
    at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RunRecord {
    run_id: String,
    plan_id: Option<String>,
    /// Unix seconds when the run started.
    at: u64,
    items: Vec<RunItem>,
}

impl RunRecord {
    fn unresolved(&self) -> bool {
        self.items
            .iter()
            .any(|entry| matches!(entry.state, ItemState::Intent | ItemState::RestoreIntent))
    }

    /// Unix seconds of the newest write to this run.
    fn last_write(&self) -> u64 {
        self.items
            .iter()
            .map(|entry| entry.at)
            .max()
            .unwrap_or(0)
            .max(self.at)
    }

    /// The history entry this run implies: every item that is, or was, in the Trash.
    fn activity(&self) -> Activity {
        let mut bytes = 0;
        let mut items = Vec::new();
        for entry in &self.items {
            if matches!(
                entry.state,
                ItemState::Moved | ItemState::RestoreIntent | ItemState::Restored
            ) {
                bytes += entry.item.bytes;
            } else if entry.state != ItemState::Unknown {
                continue;
            }
            let mut item = entry.item.clone();
            item.restored = entry.state == ItemState::Restored;
            items.push(item);
        }
        let count = |state: ItemState| {
            self.items
                .iter()
                .filter(|entry| entry.state == state)
                .count()
        };
        Activity {
            id: self.run_id.clone(),
            at: self.at,
            bytes,
            items,
            skipped: count(ItemState::Skipped),
            unknown: count(ItemState::Unknown),
            recovered: false,
            free_space: Vec::new(),
        }
    }
}

/// Where a journalled item stands in the Trash or Recycle Bin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BinLookup {
    /// Exactly one entry is this item. `trash_path` is its place in `~/.Trash`
    /// (none on Windows); `deleted_at` is Unix seconds.
    Found {
        trash_path: Option<String>,
        deleted_at: u64,
    },
    /// The Trash was read and holds no such item.
    Missing,
    /// It could not be read, or more than one entry could be the item.
    Unknown,
}

/// The macOS check: look for the item by device and inode in `~/.Trash`.
/// Elsewhere the caller supplies its own lookup, and this says unknown.
pub fn trash_lookup(home: &Path, item: &ActivityItem) -> BinLookup {
    if cfg!(windows) {
        return BinLookup::Unknown;
    }
    let Some(wanted) = item.identity else {
        return BinLookup::Unknown;
    };
    let deleted_at = item.trash_window.map_or(0, |window| window.0);
    if let Some(recorded) = &item.trash_path {
        match fs::symlink_metadata(recorded) {
            Ok(md) if identity(&md) == wanted => {
                return BinLookup::Found {
                    trash_path: Some(recorded.clone()),
                    deleted_at,
                };
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return BinLookup::Unknown,
        }
    }
    let Ok(entries) = fs::read_dir(home.join(".Trash")) else {
        return BinLookup::Unknown;
    };
    let mut found = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return BinLookup::Unknown;
        };
        let path = entry.path();
        if let Ok(md) = path.symlink_metadata()
            && identity(&md) == wanted
        {
            found.push(path);
        }
    }
    match found.as_slice() {
        [] => BinLookup::Missing,
        [only] => BinLookup::Found {
            trash_path: Some(only.to_string_lossy().into_owned()),
            deleted_at,
        },
        _ => BinLookup::Unknown,
    }
}

enum Original {
    Same,
    Absent,
    Other,
}

/// Whether the item's own metadata still matches what was recorded. With a
/// recorded identity that decides; without one (Windows) kind, modified time
/// and, for a file, length must all have been recorded and all agree.
fn same_item(item: &ActivityItem, md: &fs::Metadata) -> bool {
    if let Some(recorded) = item.identity {
        return identity(md) == recorded;
    }
    let modified = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    item.is_dir == Some(md.is_dir())
        && item.modified.is_some()
        && item.modified == modified
        && (md.is_dir() || item.file_size == Some(md.len()))
}

fn original_state(item: &ActivityItem) -> Original {
    match fs::symlink_metadata(&item.path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Original::Absent,
        Err(_) => Original::Other,
        Ok(md) if same_item(item, &md) => Original::Same,
        Ok(_) => Original::Other,
    }
}

fn window_start(item: &ActivityItem) -> Option<u64> {
    item.trash_window.map(|window| window.0)
}

/// Whether two records describe the same journalled item.
fn same_entry(a: &ActivityItem, b: &ActivityItem) -> bool {
    a.path == b.path && window_start(a) == window_start(b)
}

type Settled = (ItemState, ActivityItem, Option<String>);

fn unknown(item: &ActivityItem, why: &str) -> Option<Settled> {
    let mut item = item.clone();
    item.interrupted = Some(why.to_string());
    Some((ItemState::Unknown, item, Some(why.to_string())))
}

/// What a move that has an intent but no outcome really did. `None` leaves it
/// open to be checked again (the Trash could not be read yet).
fn settle_move(
    entry: &RunItem,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
    give_up: bool,
) -> Option<Settled> {
    let item = &entry.item;
    match original_state(item) {
        Original::Same => Some((
            ItemState::Skipped,
            item.clone(),
            Some("Interrupted before the move; the item was not moved".to_string()),
        )),
        Original::Other => unknown(
            item,
            "Interrupted; something else is at the original place, so it cannot be told \
             whether the item was moved",
        ),
        Original::Absent => match lookup(item) {
            BinLookup::Found {
                trash_path,
                deleted_at,
            } => {
                let mut moved = item.clone();
                let start = window_start(item).unwrap_or(deleted_at);
                moved.trash_path = trash_path;
                moved.trash_window = Some((start, deleted_at.max(start)));
                Some((ItemState::Moved, moved, None))
            }
            BinLookup::Missing => unknown(
                item,
                "Interrupted; the item is gone from its place and was not found in the Trash",
            ),
            BinLookup::Unknown if give_up => unknown(
                item,
                "Interrupted; the Trash could not be checked, or held more than one match",
            ),
            BinLookup::Unknown => None,
        },
    }
}

/// What a restore that has an intent but no outcome really did.
fn settle_restore(
    entry: &RunItem,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
    give_up: bool,
) -> Option<Settled> {
    let item = &entry.item;
    match original_state(item) {
        Original::Same => Some((ItemState::Restored, item.clone(), None)),
        // Something else is at the original place. The item may still be in
        // the Trash (an Undo that crashed after its intent, or whose outcome
        // could not be written, and then the name was taken again): only a
        // Trash that does not hold exactly this item makes it unknown.
        Original::Other => match lookup(item) {
            BinLookup::Found { .. } => Some((ItemState::Moved, item.clone(), None)),
            BinLookup::Missing => unknown(
                item,
                "Interrupted while restoring; something else is at the original place",
            ),
            BinLookup::Unknown if give_up => unknown(
                item,
                "Interrupted while restoring; something else is at the original place, \
                 and the Trash could not be checked",
            ),
            BinLookup::Unknown => None,
        },
        Original::Absent => match lookup(item) {
            BinLookup::Found { .. } => Some((ItemState::Moved, item.clone(), None)),
            BinLookup::Missing => unknown(
                item,
                "Interrupted while restoring; the item is in neither place",
            ),
            BinLookup::Unknown if give_up => unknown(
                item,
                "Interrupted while restoring; the Trash could not be checked",
            ),
            BinLookup::Unknown => None,
        },
    }
}

fn open_store(home: &Path) -> std::io::Result<StateStore> {
    StateStore::open(&state_dir(home), JOURNAL_NAMESPACE)
}

/// Chunk 0 of a run, which is also the run's one-time claim.
fn run_key(run_id: &str) -> String {
    format!("run-{run_id}")
}

fn chunk_key(run_id: &str, chunk: usize) -> String {
    if chunk == 0 {
        run_key(run_id)
    } else {
        format!("run-{run_id}-c{chunk}")
    }
}

/// Run ids are Unix milliseconds. They end up in record and lock file names,
/// so anything else is refused.
fn valid_run_id(run_id: &str) -> bool {
    !run_id.is_empty() && run_id.len() <= 20 && run_id.bytes().all(|b| b.is_ascii_digit())
}

/// Held by the process that is applying or restoring a run, for as long as it
/// works. The system releases it when the process ends, so a lock that is
/// free means nobody is working on the run.
struct RunLock {
    _lock: rightkit_fs::FileLock,
}

impl RunLock {
    /// Take the lock without waiting. `None`: another process or thread holds it.
    fn try_take(home: &Path, run_id: &str) -> std::io::Result<Option<Self>> {
        if !valid_run_id(run_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a run id",
            ));
        }
        let path = state_dir(home)
            .join("cleanup-locks")
            .join(format!("run-{run_id}.lock"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match rightkit_fs::FileLock::try_lock_with(
            &path,
            rightkit_fs::LockKind::Flock,
            Duration::ZERO,
        ) {
            Ok(lock) => Ok(Some(Self { _lock: lock })),
            Err(rightkit_fs::FsError::Locked { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// How often, and how long apart, [`take_waiting`] tries: at most 300 ms of waiting.
const LOCK_TRIES: u32 = 7;
const LOCK_STEP: Duration = Duration::from_millis(50);

/// [`RunLock::try_take`], tried a few times so a holder that is only finishing
/// a write does not make an Undo refuse. `None`: still held after the last try.
fn take_waiting(home: &Path, run_id: &str) -> std::io::Result<Option<RunLock>> {
    for attempt in 0..LOCK_TRIES {
        if attempt > 0 {
            std::thread::sleep(LOCK_STEP);
        }
        if let Some(lock) = RunLock::try_take(home, run_id)? {
            return Ok(Some(lock));
        }
    }
    Ok(None)
}

/// Change the list of runs with unresolved items. Another process may publish
/// the same version first; the change is then made again on the newer list.
/// Superseded versions of the list are removed once the new one is durable.
fn update_open_runs(store: &StateStore, change: impl Fn(&mut Vec<String>)) -> std::io::Result<()> {
    for _ in 0..8 {
        let loaded = match store.load::<Vec<String>>(OPEN_RUNS) {
            // Another process just removed the version being read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            other => other?,
        };
        let (version, mut runs) = match loaded {
            Some(record) => (record.version, record.state),
            None => (0, Vec::new()),
        };
        let before = runs.clone();
        change(&mut runs);
        if runs == before {
            return Ok(());
        }
        match store.publish(OPEN_RUNS, version + 1, &runs) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
            Ok(_) => {
                let _ = store.remove_older(OPEN_RUNS, version + 1);
                return Ok(());
            }
        }
    }
    Err(std::io::Error::other("the list of open runs is busy"))
}

fn add_open_run(store: &StateStore, run_id: &str) -> std::io::Result<()> {
    update_open_runs(store, |runs| {
        if !runs.iter().any(|id| id == run_id) {
            runs.push(run_id.to_string());
        }
    })
}

fn remove_open_run(store: &StateStore, run_id: &str) -> std::io::Result<()> {
    update_open_runs(store, |runs| runs.retain(|id| id != run_id))
}

/// Read a run's chunks and join them. `None`: the journal has no such run.
/// Also returns the newest version of each chunk.
fn read_run(store: &StateStore, run_id: &str) -> std::io::Result<Option<(RunRecord, Vec<u64>)>> {
    let mut run: Option<RunRecord> = None;
    let mut versions = Vec::new();
    for chunk in 0.. {
        let Some(record) = store.load::<RunRecord>(&chunk_key(run_id, chunk))? else {
            break;
        };
        versions.push(record.version);
        match &mut run {
            None => run = Some(record.state),
            Some(whole) => {
                // Every chunk but the last is full; item numbers rely on it.
                if whole.items.len() != chunk * CHUNK_ITEMS {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "a journal chunk is missing items",
                    ));
                }
                whole.items.extend(record.state.items);
            }
        }
    }
    Ok(run.map(|run| (run, versions)))
}

/// One run's journal. `run` holds every item; each chunk is published and
/// pruned on its own.
struct RunJournal {
    store: StateStore,
    run: RunRecord,
    /// The newest published version of each chunk.
    versions: Vec<u64>,
    _lock: RunLock,
}

impl RunJournal {
    /// Publish version 1 of chunk 0 of a new run. This is the one-time claim:
    /// an id that already has a record is refused. The run joins the open list
    /// before any item is touched.
    fn start(home: &Path, run_id: &str, plan_id: Option<&str>) -> std::io::Result<Self> {
        let lock = RunLock::try_take(home, run_id)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "the run is held by another process",
            )
        })?;
        let store = open_store(home)?;
        // Older versions of a chunk are removed, so version 1 alone no longer
        // proves the id is taken; the lock makes this check and the publish
        // below one step against any other starter of the same id.
        if store.load::<RunRecord>(&run_key(run_id))?.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "the run already has a record",
            ));
        }
        let run = RunRecord {
            run_id: run_id.to_string(),
            plan_id: plan_id.map(str::to_owned),
            at: now_millis() / 1000,
            items: Vec::new(),
        };
        store.publish(&run_key(run_id), 1, &run)?;
        add_open_run(&store, run_id)?;
        Ok(Self {
            store,
            run,
            versions: vec![1],
            _lock: lock,
        })
    }

    /// Open an existing run for an Undo and hold its lock. `Ok(None)`: the
    /// journal has no such run (a history entry from before the journal
    /// existed). A record that cannot be read, or a run another process is
    /// working on, is an error.
    fn load(home: &Path, run_id: &str) -> Result<Option<Self>, String> {
        if !valid_run_id(run_id) || !state_dir(home).join(JOURNAL_NAMESPACE).is_dir() {
            return Ok(None);
        }
        let unreadable = |e: std::io::Error| {
            format!(
                "Could not read the safety record of this cleanup, so it was not \
                 restored: {e}"
            )
        };
        let store = open_store(home).map_err(unreadable)?;
        // The lock comes before the first read, so a prune by a live apply
        // cannot remove the run between the reads. A holder that is only
        // finishing a write is waited for a moment.
        let lock = match take_waiting(home, run_id) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                return Err(
                    "This cleanup is being worked on by another app window right \
                     now, so it was not restored. Try again in a moment."
                        .to_string(),
                );
            }
            Err(e) => return Err(unreadable(e)),
        };
        let Some((run, versions)) = read_run(&store, run_id).map_err(unreadable)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            store,
            run,
            versions,
            _lock: lock,
        }))
    }

    /// The items of one chunk, as they are in memory.
    fn chunk_items(&self, chunk: usize) -> Vec<RunItem> {
        self.run
            .items
            .iter()
            .skip(chunk * CHUNK_ITEMS)
            .take(CHUNK_ITEMS)
            .cloned()
            .collect()
    }

    /// Publish the next version of one chunk with these items. The in-memory
    /// copy changes only when the version reached the disk; then the chunk's
    /// older versions are removed (a failure there is harmless).
    fn write_chunk(&mut self, chunk: usize, items: Vec<RunItem>) -> std::io::Result<()> {
        let key = chunk_key(&self.run.run_id, chunk);
        let version = self.versions.get(chunk).map_or(1, |v| v + 1);
        let record = RunRecord {
            run_id: self.run.run_id.clone(),
            plan_id: self.run.plan_id.clone(),
            at: self.run.at,
            items,
        };
        self.store.publish(&key, version, &record)?;
        match self.versions.get_mut(chunk) {
            Some(slot) => *slot = version,
            None => self.versions.push(version),
        }
        for (offset, item) in record.items.into_iter().enumerate() {
            let index = chunk * CHUNK_ITEMS + offset;
            match self.run.items.get_mut(index) {
                Some(slot) => *slot = item,
                None => self.run.items.push(item),
            }
        }
        if version > 1 {
            let _ = self.store.remove_older(&key, version);
        }
        Ok(())
    }

    /// Write the intent for an item that is about to move; returns its index.
    /// The run is put back on the open list if it is missing from it.
    fn add_intent(&mut self, item: ActivityItem) -> std::io::Result<usize> {
        add_open_run(&self.store, &self.run.run_id)?;
        let index = self.run.items.len();
        let chunk = index / CHUNK_ITEMS;
        let mut items = self.chunk_items(chunk);
        items.push(RunItem {
            item,
            state: ItemState::Intent,
            reason: None,
            at: now_millis() / 1000,
        });
        self.write_chunk(chunk, items)?;
        Ok(index)
    }

    fn record(
        &mut self,
        index: usize,
        state: ItemState,
        item: Option<ActivityItem>,
        reason: Option<String>,
    ) -> std::io::Result<()> {
        let chunk = index / CHUNK_ITEMS;
        let mut items = self.chunk_items(chunk);
        let entry = items
            .get_mut(index % CHUNK_ITEMS)
            .ok_or_else(|| std::io::Error::other("no such journal entry"))?;
        if let Some(item) = item {
            entry.item = item;
        }
        entry.state = state;
        entry.reason = reason;
        entry.at = now_millis() / 1000;
        self.write_chunk(chunk, items)
    }

    /// Take the run off the open list once nothing in it is unresolved.
    fn close(&self) {
        if !self.run.unresolved() {
            let _ = remove_open_run(&self.store, &self.run.run_id);
        }
    }
}

/// How many fully settled runs keep their journal records.
const KEPT_SETTLED_RUNS: usize = 200;

/// The run id of a chunk-0 key `run-<id>`, and the run and chunk of any chunk key.
fn parse_chunk_key(key: &str) -> Option<(&str, usize)> {
    let rest = key.strip_prefix("run-")?;
    let (id, chunk) = match rest.split_once("-c") {
        Some((id, chunk)) => (id, chunk.parse().ok().filter(|n| *n > 0)?),
        None => (rest, 0),
    };
    valid_run_id(id).then_some((id, chunk))
}

/// Remove the journal records of old runs that are done with. A run is fully
/// settled when none of its items is in intent or restore_intent and its lock
/// is free. The newest `KEPT_SETTLED_RUNS` settled runs are kept. The history
/// file has no retention of its own (it keeps every entry), so a run with an
/// item still in the Trash (state moved) is never pruned, however old: only
/// runs whose items are all restored, skipped or unknown go, oldest first.
/// Each run is checked again under its own lock right before its chunks are
/// removed, last chunk first. Never called during recovery. Returns how many
/// runs were removed.
fn prune_settled_runs(home: &Path) -> Result<usize, String> {
    if !state_dir(home).join(JOURNAL_NAMESPACE).is_dir() {
        return Ok(0);
    }
    let store = open_store(home).map_err(|e| e.to_string())?;
    let ids = store.ids().map_err(|e| e.to_string())?;
    let mut runs: Vec<&str> = ids
        .iter()
        .filter_map(|key| parse_chunk_key(key))
        .filter(|(_, chunk)| *chunk == 0)
        .map(|(id, _)| id)
        .collect();
    // Run ids are Unix milliseconds: longer is newer, then by digits.
    runs.sort_by_key(|id| (id.len(), id.to_string()));
    let settled = |run: &RunRecord| !run.unresolved();
    let mut kept = 0;
    let mut doomed = Vec::new();
    for id in runs.iter().rev() {
        let Ok(Some(_lock)) = RunLock::try_take(home, id) else {
            continue;
        };
        let Ok(Some((run, _))) = read_run(&store, id) else {
            continue;
        };
        if !settled(&run) {
            continue;
        }
        kept += 1;
        if kept > KEPT_SETTLED_RUNS && !run.items.iter().any(|e| e.state == ItemState::Moved) {
            doomed.push(id.to_string());
        }
    }
    let mut removed = 0;
    // `doomed` is newest first; the oldest go first.
    for id in doomed.iter().rev() {
        let Some(lock) = RunLock::try_take(home, id).map_err(|e| e.to_string())? else {
            continue;
        };
        let Some((run, _)) = read_run(&store, id).map_err(|e| e.to_string())? else {
            continue;
        };
        if !settled(&run) || run.items.iter().any(|e| e.state == ItemState::Moved) {
            continue;
        }
        let last_chunk = run.items.len().saturating_sub(1) / CHUNK_ITEMS;
        for chunk in (0..=last_chunk).rev() {
            store
                .remove_id(&chunk_key(id, chunk))
                .map_err(|e| e.to_string())?;
        }
        drop(lock);
        // The lock file is only a name; a failure to remove it is harmless.
        let _ = fs::remove_file(
            state_dir(home)
                .join("cleanup-locks")
                .join(format!("run-{id}.lock")),
        );
        removed += 1;
    }
    Ok(removed)
}

fn begin_run(
    home: &Path,
    requested: Option<&str>,
    plan_id: Option<&str>,
) -> Result<RunJournal, String> {
    let taken = "This cleanup run was already started, so nothing was moved".to_string();
    if let Some(id) = requested {
        return RunJournal::start(home, id, plan_id).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                taken
            } else {
                format!("Could not save the safety record, so nothing was moved: {e}")
            }
        });
    }
    let mut id = now_millis();
    for _ in 0..8 {
        match RunJournal::start(home, &id.to_string(), plan_id) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => id += 1,
            Err(e) => {
                return Err(format!(
                    "Could not save the safety record, so nothing was moved: {e}"
                ));
            }
            Ok(journal) => return Ok(journal),
        }
    }
    Err(taken)
}

/// Make the history agree with a run's journal: add what is missing, mark
/// what the journal shows as restored, note what is unresolved.
fn merge_into_log(home: &Path, run: &RunRecord) -> Result<(), String> {
    // Only reconcile merges, and it runs only for a run that no live process
    // holds and that has been idle, so every run merged here was interrupted,
    // and each gets an entry even when nothing was moved (its skipped count
    // still tells the user about the interruption).
    let mut view = run.activity();
    view.recovered = true;
    update_log(home, |log| {
        let mut dirty = false;
        match log.actions.iter_mut().find(|a| a.id == view.id) {
            None => {
                log.actions.push(view);
                dirty = true;
            }
            Some(existing) => {
                if existing.skipped != view.skipped
                    || existing.unknown != view.unknown
                    || existing.recovered != view.recovered
                {
                    existing.skipped = view.skipped;
                    existing.unknown = view.unknown;
                    existing.recovered = view.recovered;
                    dirty = true;
                }
                for want in view.items {
                    match existing.items.iter_mut().find(|i| same_entry(i, &want)) {
                        Some(have) => {
                            if want.restored && !have.restored {
                                have.restored = true;
                                dirty = true;
                            }
                            if have.interrupted != want.interrupted {
                                have.interrupted = want.interrupted;
                                dirty = true;
                            }
                        }
                        None => {
                            existing.items.push(want);
                            dirty = true;
                        }
                    }
                }
            }
        }
        if dirty {
            log.schema_version = ACTIVITY_SCHEMA_VERSION;
        }
        Ok(dirty)
    })
}

/// What [`reconcile`] did.
#[derive(Debug, Default)]
pub struct Reconciled {
    /// How many runs changed.
    pub changed: usize,
    /// One plain-words line for each run that could not be settled; the run is
    /// left as it was and looked at again next time.
    pub warnings: Vec<String>,
}

/// The line shown when reconciling could not even start.
fn unsettled_warning(why: &str) -> String {
    format!(
        "Earlier interrupted cleanups could not be checked ({why}). They are left as \
         they are and checked again next time."
    )
}

fn run_warning(run_id: &str, why: &str) -> String {
    format!(
        "An earlier cleanup (run {run_id}) could not be settled after an interruption \
         ({why}). It is left as it is and checked again next time."
    )
}

/// Settle every run a crash left unresolved by looking at the real world, and
/// bring the history in line with the journal. Safe to run twice: a settled
/// item is never looked at again. A run whose lock is held belongs to a live
/// process and is never touched, whatever its age; a run is also left alone
/// while its newest record is younger than [`INTERRUPTED_AFTER_SECS`]. One run
/// that cannot be read or settled does not stop the others: it becomes a
/// warning. An error is returned only when the list of open runs itself cannot
/// be read.
pub fn reconcile(
    home: &Path,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
) -> Result<Reconciled, String> {
    let mut done = Reconciled::default();
    if !state_dir(home).join(JOURNAL_NAMESPACE).is_dir() {
        return Ok(done);
    }
    let store = open_store(home).map_err(|e| e.to_string())?;
    let Some(open) = store
        .load::<Vec<String>>(OPEN_RUNS)
        .map_err(|e| e.to_string())?
    else {
        return Ok(done);
    };
    let now = now_millis() / 1000;
    for run_id in open.state {
        if !valid_run_id(&run_id) {
            continue;
        }
        match reconcile_run(home, &store, &run_id, now, lookup) {
            Ok(true) => done.changed += 1,
            Ok(false) => {}
            Err(why) => done.warnings.push(run_warning(&run_id, &why)),
        }
    }
    Ok(done)
}

/// Settle one open run. `Ok(true)`: the run changed.
fn reconcile_run(
    home: &Path,
    store: &StateStore,
    run_id: &str,
    now: u64,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
) -> Result<bool, String> {
    let lock = match RunLock::try_take(home, run_id) {
        Ok(Some(lock)) => lock,
        // Another process is working on it right now.
        Ok(None) => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    let found = read_run(store, run_id).map_err(|e| e.to_string())?;
    let Some((run, versions)) = found else {
        let _ = remove_open_run(store, run_id);
        return Ok(false);
    };
    if now.saturating_sub(run.last_write()) < INTERRUPTED_AFTER_SECS {
        return Ok(false);
    }
    let mut journal = RunJournal {
        store: store.clone(),
        run,
        versions,
        _lock: lock,
    };
    let mut touched = false;
    for index in 0..journal.run.items.len() {
        let entry = &journal.run.items[index];
        let give_up = now.saturating_sub(entry.at) > PLAN_EXPIRY_SECS;
        let settled = match entry.state {
            ItemState::Intent => settle_move(entry, lookup, give_up),
            ItemState::RestoreIntent => settle_restore(entry, lookup, give_up),
            _ => None,
        };
        let Some((state, item, reason)) = settled else {
            continue;
        };
        match journal.record(index, state, Some(item), reason) {
            Ok(()) => touched = true,
            // Another process settled it first; its version wins.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(e) => return Err(e.to_string()),
        }
    }
    merge_into_log(home, &journal.run)?;
    if !journal.run.unresolved() {
        let _ = remove_open_run(store, run_id);
    }
    Ok(touched)
}

/// The journal side of an Undo, for callers that do the move themselves (the
/// hub on Windows). A history entry from before the journal existed has no
/// run; its restore is then recorded in the history alone, as before.
pub struct RestoreJournal(Option<RunJournal>);

impl RestoreJournal {
    /// Open the run's journal and hold its lock until this value is dropped.
    /// A history entry with no journal opens as an empty journal. A journal
    /// that cannot be read, or a run another process is working on, is an
    /// error: the Undo must not go ahead without its record.
    pub fn open(home: &Path, activity_id: &str) -> Result<Self, String> {
        RunJournal::load(home, activity_id).map(Self)
    }

    /// Write the intent to move `item` back. An error means do not move it.
    pub fn begin(&mut self, item: &ActivityItem) -> Result<(), String> {
        let Some(journal) = &mut self.0 else {
            return Ok(());
        };
        let Some(index) = journal.run.items.iter().position(|entry| {
            matches!(entry.state, ItemState::Moved | ItemState::RestoreIntent)
                && same_entry(&entry.item, item)
        }) else {
            return Ok(());
        };
        let failed = |e: std::io::Error| {
            format!("Could not save the safety record, so it was not restored: {e}")
        };
        add_open_run(&journal.store, &journal.run.run_id).map_err(failed)?;
        journal
            .record(index, ItemState::RestoreIntent, None, None)
            .map_err(failed)
    }

    /// Write what the move back did. A failed write is not fatal: the intent
    /// stays and the next reconcile reads the real world.
    pub fn finish(&mut self, item: &ActivityItem, restored: bool) {
        let Some(journal) = &mut self.0 else { return };
        if let Some(index) = journal.run.items.iter().position(|entry| {
            entry.state == ItemState::RestoreIntent && same_entry(&entry.item, item)
        }) {
            let state = if restored {
                ItemState::Restored
            } else {
                ItemState::Moved
            };
            let _ = journal.record(index, state, None, None);
        }
    }

    /// Call after the history has been saved.
    pub fn close(&self) {
        if let Some(journal) = &self.0 {
            journal.close();
        }
    }
}

/// Mark items restored in the history, saved durably. The newest history is
/// read under its lock and the items are marked one by one, so entries other
/// processes added meanwhile are kept.
pub fn mark_restored(
    home: &Path,
    activity_id: &str,
    restored: &[ActivityItem],
) -> Result<(), String> {
    update_log(home, |log| {
        let activity = log
            .actions
            .iter_mut()
            .find(|a| a.id == activity_id)
            .ok_or("No such action")?;
        for done in restored {
            if let Some(item) = activity.items.iter_mut().find(|i| same_entry(i, done)) {
                item.restored = true;
            }
        }
        Ok(true)
    })
}

// ---------------------------------------------------------------------------
// One-time claim of a saved plan (the CLI)
// ---------------------------------------------------------------------------

/// A new run id (also the activity id): Unix milliseconds.
pub fn mint_run_id() -> String {
    now_millis().to_string()
}

#[derive(Debug, Eq, PartialEq)]
pub enum PlanClaim {
    Claimed,
    /// Someone claimed it first; the run id is known when the claim record says.
    AlreadyApplied(Option<String>),
}

/// The run that claimed a plan file, read from the claim record's name
/// `<plan>.claimed-<run id>.json` beside it.
pub fn plan_claim_run(plan: &Path) -> Option<String> {
    let name = plan.file_name()?.to_str()?.strip_suffix(".json")?;
    let prefix = format!("{name}.claimed-");
    fs::read_dir(plan.parent()?)
        .ok()?
        .flatten()
        .find_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let run = file.strip_prefix(&prefix)?.strip_suffix(".json")?;
            Some(run.to_string())
        })
}

/// Whether the journal has a record of this run, that is, whether the run that
/// claimed a plan ever started. A journal that cannot be read counts as
/// started: nothing is claimed about what it did not show.
pub fn run_started(home: &Path, run_id: &str) -> bool {
    if !valid_run_id(run_id) {
        return true;
    }
    if !state_dir(home).join(JOURNAL_NAMESPACE).is_dir() {
        return false;
    }
    match open_store(home).and_then(|store| store.load::<RunRecord>(&run_key(run_id))) {
        Ok(record) => record.is_some(),
        Err(_) => true,
    }
}

/// Claim a plan before any effect: rename `<plan>.json` to
/// `<plan>.claimed-<run id>.json` without replacing anything. Only one caller
/// can win; the others learn the run that did. The record is kept as
/// evidence, never deleted by apply.
pub fn claim_plan(plan: &Path, run_id: &str) -> Result<PlanClaim, String> {
    let name = plan
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".json"))
        .ok_or("Not a plan file")?;
    let claimed = plan.with_file_name(format!("{name}.claimed-{run_id}.json"));
    match rightkit_fs::rename_exclusive(plan, &claimed).map_err(std::io::Error::from) {
        Ok(()) => {
            if let Some(dir) = plan.parent() {
                rightkit_fs::try_sync_dir(dir)
                    .map_err(|e| format!("Could not save the plan claim: {e}"))?;
            }
            Ok(PlanClaim::Claimed)
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::AlreadyExists
            ) =>
        {
            Ok(PlanClaim::AlreadyApplied(plan_claim_run(plan)))
        }
        Err(e) => Err(format!("Could not claim the plan: {e}")),
    }
}

fn listing(dir: &Path) -> HashSet<OsString> {
    fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default()
}

/// Check one request against the rule pack and the disk, right now.
fn revalidate(
    request: &Request,
    pack: &CleanupPack,
    home: &Path,
    running: &[String],
) -> Result<(PathBuf, u64, (u64, u64)), String> {
    let wanted: (u64, u64) = match (request.dev.parse(), request.ino.parse()) {
        (Ok(dev), Ok(ino)) => (dev, ino),
        _ => return Err("Bad identity".into()),
    };
    let rule = pack
        .rules
        .iter()
        .find(|r| r.id == request.rule_id)
        .ok_or("Unknown rule")?;
    if rule.risk == RuleRisk::Info {
        return Err("Not something to move".into());
    }
    if !applies_here(rule) {
        return Err("Not a rule for this system".into());
    }
    let path = PathBuf::from(&request.path);
    let covered = match &rule.discover {
        Some(d) => is_discovered(&path, home, d),
        None => rule.paths.iter().any(|p| path_matches(p, &path, home)),
    };
    if !covered {
        return Err("Not a location this rule covers".into());
    }
    if excluded(rule, &display_name(&path)) {
        return Err("Excluded by the rule".into());
    }
    let md = path
        .symlink_metadata()
        .map_err(|_| "Already gone".to_string())?;
    if md.file_type().is_symlink() {
        return Err("Is now a link".into());
    }
    if identity(&md) != wanted {
        return Err("Changed since the scan".into());
    }
    if is_dataless(&md) {
        return Err("Stored in the cloud".into());
    }
    if let Some(min) = rule.min_age_days {
        match deep_age_days(&path, &md, min) {
            Some(age) if age >= min => {}
            _ => return Err("Modified since the scan".into()),
        }
    }
    match in_use(rule, &display_name(&path), running)? {
        Some(process) => Err(format!("In use: {process} is running")),
        None => {
            proven_idle(rule, &path, &md)?;
            if let Some(why) = held_open(&path) {
                return Err(why);
            }
            let bytes = match rule.measurement {
                Measurement::Allocated => measure(&path).0,
                Measurement::CloneAware => {
                    let mut budget = CLONE_AWARE_BUDGET;
                    measure_clone_aware(&path, &mut budget).1.unwrap_or(0)
                }
            };
            Ok((path, bytes, wanted))
        }
    }
}

/// Move each requested item to Trash through `trasher`, re-checking it first.
/// `trasher` must only move to Trash; it is the one place an effect happens.
pub fn apply(
    home: &Path,
    requests: &[Request],
    running: &mut RunningProvider<'_>,
    trasher: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<ApplyResult, String> {
    apply_with(home, requests, running, trasher, &mut |item| {
        trash_lookup(home, item)
    })
}

/// [`apply`] with the caller's way of finding an item in the Recycle Bin, used
/// to settle a run an earlier interruption left open (the core can only look
/// in `~/.Trash`).
pub fn apply_with(
    home: &Path,
    requests: &[Request],
    running: &mut RunningProvider<'_>,
    trasher: &mut dyn FnMut(&Path) -> Result<(), String>,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
) -> Result<ApplyResult, String> {
    apply_run(home, requests, running, None, None, trasher, lookup)
}

/// [`apply`] as a journalled run. The run's record is written before the first
/// item is touched; each item's intent is on disk before it moves and its
/// outcome right after. `run_id` is the id a claimed plan already carries
/// (a fresh one when `None`); a run id can start only once.
pub fn apply_run(
    home: &Path,
    requests: &[Request],
    running: &mut RunningProvider<'_>,
    plan_id: Option<&str>,
    run_id: Option<&str>,
    trasher: &mut dyn FnMut(&Path) -> Result<(), String>,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
) -> Result<ApplyResult, String> {
    apply_run_with_rules(
        home,
        requests,
        running,
        &[],
        plan_id,
        run_id,
        trasher,
        lookup,
    )
}

/// [`apply_run`] with extra rules for this run only: the hub's own sources
/// (RustRight targets, agent scratch) build one literal rule per item, so the
/// same identity, in-use and held-open checks, journal and Undo apply to them.
/// The extra rules are never written to the pack.
#[allow(clippy::too_many_arguments)]
pub fn apply_run_with_rules(
    home: &Path,
    requests: &[Request],
    running: &mut RunningProvider<'_>,
    extra: &[CleanupRule],
    plan_id: Option<&str>,
    run_id: Option<&str>,
    trasher: &mut dyn FnMut(&Path) -> Result<(), String>,
    lookup: &mut dyn FnMut(&ActivityItem) -> BinLookup,
) -> Result<ApplyResult, String> {
    let mut pack = load_pack()?;
    pack.rules.extend(extra.iter().cloned());
    let trash_dir = home.join(".Trash");
    let mut result = ApplyResult::default();
    if requests.is_empty() {
        return Ok(result);
    }
    // Without a readable history nothing is moved, and the file is left as it is.
    read_log(home).map_err(|why| {
        format!("{why}. Nothing was moved, and the history file was left as it is.")
    })?;
    // Settle what an earlier crash left open before adding to it.
    match reconcile(home, lookup) {
        Ok(settled) => result.warnings = settled.warnings,
        Err(why) => result.warnings.push(unsettled_warning(&why)),
    }
    let mut journal = begin_run(home, run_id, plan_id)?;
    let mut items: Vec<ActivityItem> = Vec::new();
    let mut skipped_in_journal = 0;
    // Drives items are moved off: free space before the first move, and whether
    // anything moved. Read on the existing worker thread, each read time-limited.
    let mut watched: Vec<(DriveKey, String, Option<u64>, bool)> = Vec::new();

    // The process list is taken again for every item, right before it is
    // re-checked: a program started during review or mid-run is seen. A list that
    // cannot be had is unknown liveness, so that item is skipped.
    for request in requests {
        let live = match running() {
            Ok(live) => live,
            Err(why) => {
                result.skipped.push(Skipped {
                    path: request.path.clone(),
                    reason: format!("Can't tell which programs are running: {why}"),
                });
                continue;
            }
        };
        let (path, bytes, wanted) = match revalidate(request, &pack, home, &live) {
            Ok(ok) => ok,
            Err(reason) => {
                result.skipped.push(Skipped {
                    path: request.path.clone(),
                    reason,
                });
                continue;
            }
        };
        let drive = drive_of(&path);
        let volume = drive.as_ref().map(|(key, _)| key.clone());
        if let Some((key, label)) = drive
            && !watched.iter().any(|w| w.0 == key)
        {
            let free = available_bytes(&key);
            watched.push((key, label, free, false));
        }
        let before = listing(&trash_dir);
        let seen = path.symlink_metadata().ok();
        let started = now_millis() / 1000;
        let mut item = ActivityItem {
            path: request.path.clone(),
            trash_path: None,
            bytes,
            restored: false,
            identity: Some(wanted).filter(|id| *id != (0, 0)),
            is_dir: seen.as_ref().map(|md| md.is_dir()),
            file_size: seen.as_ref().filter(|md| md.is_file()).map(|md| md.len()),
            modified: seen
                .as_ref()
                .and_then(|md| md.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            trash_window: Some((started, started)),
            interrupted: None,
        };
        // Write ahead: no intent on disk, no move.
        let index = match journal.add_intent(item.clone()) {
            Ok(index) => index,
            Err(error) => {
                result.skipped.push(Skipped {
                    path: request.path.clone(),
                    reason: format!(
                        "Could not save the safety record, so it was not moved: {error}"
                    ),
                });
                continue;
            }
        };
        if let Err(reason) = trasher(&path) {
            let _ = journal.record(index, ItemState::Skipped, None, Some(reason.clone()));
            skipped_in_journal += 1;
            result.skipped.push(Skipped {
                path: request.path.clone(),
                reason,
            });
            continue;
        }
        // A rename keeps the inode, so the new Trash entry that carries the
        // item's identity is where it went.
        item.trash_path = listing(&trash_dir)
            .into_iter()
            .filter(|name| !before.contains(name))
            .map(|name| trash_dir.join(name))
            .find(|candidate| {
                candidate
                    .symlink_metadata()
                    .map(|md| identity(&md) == wanted)
                    .unwrap_or(false)
            })
            .map(|p| p.to_string_lossy().into_owned());
        item.trash_window = Some((started, now_millis() / 1000));
        if let Err(error) = journal.record(index, ItemState::Moved, Some(item.clone()), None) {
            result.warnings.push(format!(
                "{} was moved to the Trash, but its record could not be saved ({error}). \
                 the app will settle it from its safety record the next time it opens the \
                 history, once this run has been idle for {} minutes.",
                request.path,
                INTERRUPTED_AFTER_SECS / 60
            ));
        }
        result.moved_items += 1;
        result.moved_bytes += bytes;
        items.push(item);
        if let Some(entry) = watched.iter_mut().find(|w| Some(&w.0) == volume.as_ref()) {
            entry.3 = true;
        }
    }

    for (volume, label, before, moved) in &watched {
        if let (true, Some(before), Some(after)) = (*moved, before, available_bytes(volume)) {
            result.free_space.push(VolumeFree {
                volume: label.clone(),
                before_available: *before,
                after_available: after,
            });
        }
    }

    let mut logged = true;
    if !items.is_empty() {
        let at = now_millis() / 1000;
        let activity = Activity {
            id: journal.run.run_id.clone(),
            at,
            bytes: result.moved_bytes,
            items,
            skipped: skipped_in_journal,
            unknown: 0,
            recovered: false,
            free_space: result.free_space.clone(),
        };
        let id = activity.id.clone();
        match upsert_activity(home, activity) {
            Ok(()) => result.activity_id = Some(id),
            Err(error) => {
                logged = false;
                result.warnings.push(format!(
                    "Items were moved to the Trash, but the history entry could not be saved \
                     ({error}). the app will add it from its safety record the next time it \
                     opens the history, once this run has been idle for {} minutes.",
                    INTERRUPTED_AFTER_SECS / 60
                ));
            }
        }
    }
    if logged {
        journal.close();
    }
    // Release this run's lock first: pruning leaves any run whose lock is held.
    drop(journal);
    if let Err(error) = prune_settled_runs(home) {
        result.warnings.push(format!(
            "Old safety records of finished cleanups could not be cleared ({error}). \
             They are tidied up the next time a cleanup finishes."
        ));
    }
    Ok(result)
}

/// What identifies a drive for the free-space record: the volume identity on
/// the Mac and Linux, the normalised volume root (such as `C:\`) on Windows.
#[cfg(windows)]
type DriveKey = String;
#[cfg(not(windows))]
type DriveKey = crate::model::VolumeIdentity;

/// The drive a path lives on and its name for the history, or `None` when the
/// platform cannot say.
#[cfg(not(windows))]
fn drive_of(path: &Path) -> Option<(DriveKey, String)> {
    crate::platform::volume_identity_for_path(path).map(|(volume, _)| (volume, volume_label(path)))
}

/// The volume root of a path (a mounted folder yields its mount path), with an
/// upper-case drive letter and one trailing backslash. The label drops the
/// trailing backslash, so a drive reads `C:`.
#[cfg(windows)]
fn drive_of(path: &Path) -> Option<(DriveKey, String)> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buffer = [0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `buffer` is a live, writable slice
    // that the call fills at most to its length.
    unsafe {
        windows::Win32::Storage::FileSystem::GetVolumePathNameW(
            windows::core::PCWSTR(wide.as_ptr()),
            &mut buffer,
        )
    }
    .ok()?;
    let end = buffer.iter().position(|unit| *unit == 0)?;
    let mut root = String::from_utf16(&buffer[..end]).ok()?;
    if let Some(rest) = root.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        root = rest.to_string();
    }
    let trimmed = root.trim_end_matches('\\');
    if trimmed.is_empty() {
        return None;
    }
    let mut key = trimmed.to_string();
    if key.as_bytes().get(1) == Some(&b':') {
        key[..1].make_ascii_uppercase();
    }
    let label = key.clone();
    key.push('\\');
    Some((key, label))
}

/// Physically free bytes of the drive (`statvfs` `f_bavail` on Unix, the free bytes available to
/// the caller on Windows; never macOS's larger important-usage figure that adds reclaimable
/// space), or `None` when it did not answer in three seconds or the platform
/// cannot say. The read runs on its own thread so a drive that hangs cannot
/// hold up the cleanup.
fn available_bytes(drive: &DriveKey) -> Option<u64> {
    let (tx, rx) = std::sync::mpsc::channel();
    let drive = drive.clone();
    std::thread::Builder::new()
        .name("rightkit-free-space".into())
        .spawn(move || {
            let _ = tx.send(drive_available(&drive));
        })
        .ok()?;
    rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

#[cfg(not(windows))]
fn drive_available(volume: &DriveKey) -> Option<u64> {
    crate::platform::volume_usage(volume)
        .ok()
        .and_then(|usage| usage.available_bytes)
}

/// Free bytes available to the caller on the drive's root.
#[cfg(windows)]
fn drive_available(root: &DriveKey) -> Option<u64> {
    let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    let mut available = 0u64;
    // SAFETY: `wide` is NUL-terminated and `available` outlives the call; the
    // other out-parameters are omitted.
    unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            windows::core::PCWSTR(wide.as_ptr()),
            Some(&mut available),
            None,
            None,
        )
    }
    .ok()?;
    Some(available)
}

/// A drive's name for the history: the folder under /Volumes for an external
/// drive, otherwise the main drive.
#[cfg(not(windows))]
fn volume_label(path: &Path) -> String {
    path.strip_prefix("/Volumes")
        .ok()
        .and_then(|rest| rest.components().next())
        .map(|name| name.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "the main drive".to_string())
}

/// Put an apply's history entry in the log. An entry with the same id (the
/// reconcile of another process may have added one from the journal) is merged
/// item by item, never replaced, so nothing already marked restored is lost.
fn upsert_activity(home: &Path, activity: Activity) -> Result<(), String> {
    update_log(home, |log| {
        log.schema_version = ACTIVITY_SCHEMA_VERSION;
        match log.actions.iter_mut().find(|a| a.id == activity.id) {
            Some(existing) => {
                for want in activity.items {
                    match existing.items.iter_mut().find(|i| same_entry(i, &want)) {
                        Some(have) if !have.restored => *have = want,
                        Some(_) => {}
                        None => existing.items.push(want),
                    }
                }
                existing.bytes = existing.bytes.max(activity.bytes);
                existing.skipped = existing.skipped.max(activity.skipped);
                existing.unknown = existing.unknown.max(activity.unknown);
                existing.recovered |= activity.recovered;
                if existing.free_space.is_empty() {
                    existing.free_space = activity.free_space;
                }
            }
            None => log.actions.push(activity),
        }
        Ok(true)
    })
}

/// The Move-to-Trash step for callers without a platform trash API (the CLI on macOS):
/// a rename into the user's own `~/.Trash`. It never deletes, never overwrites (a taken
/// name gets a numeric suffix), refuses anything outside `home` or already in the Trash,
/// and a rename across volumes fails and leaves the item where it was. `apply` has
/// already re-validated the item against its rule, identity and liveness.
pub fn trash_by_rename(home: &Path, path: &Path) -> Result<(), String> {
    let trash = home.join(".Trash");
    if !path.starts_with(home) || path == home || path.starts_with(&trash) {
        return Err("Not inside the home folder, or already in the Trash".into());
    }
    let name = path
        .file_name()
        .ok_or("No file name")?
        .to_string_lossy()
        .into_owned();
    // The rename itself refuses to replace anything (not only the check before it), so a
    // name taken in between is retried under the next suffix, never overwritten.
    let mut target = trash.join(&name);
    let mut n = 2;
    loop {
        match rightkit_fs::rename_exclusive(path, &target).map_err(std::io::Error::from) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => {
                target = trash.join(format!("{name} {n}"));
                n += 1;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    if path.symlink_metadata().is_ok() {
        return Err("Still in its original place after moving to Trash".into());
    }
    Ok(())
}

/// Windows restores go through the hub, which finds items in the Recycle Bin
/// and restores them there. The move below is a plain rename that would replace
/// anything created at the original place in the meantime, so it is not
/// offered on Windows.
#[cfg(windows)]
pub fn restore(_home: &Path, _activity_id: &str) -> Result<RestoreResult, String> {
    Err("Internal error: restore on Windows goes through the hub".to_string())
}

/// Put an action's items back where they came from. An item goes back only if
/// it is still in the Trash, still has the identity recorded when it
/// was moved (items journalled without one are checked by path only; a Trash path
/// is unique) and its original path is free.
#[cfg(not(windows))]
pub fn restore(home: &Path, activity_id: &str) -> Result<RestoreResult, String> {
    let mut result = RestoreResult::default();
    match reconcile(home, &mut |item| trash_lookup(home, item)) {
        Ok(settled) => result.warnings = settled.warnings,
        Err(why) => result.warnings.push(unsettled_warning(&why)),
    }
    let trash_dir = home.join(".Trash");
    let mut journal = RestoreJournal::open(home, activity_id)?;
    // The history is read here only to know what to restore; what is marked
    // restored is merged into the newest history under its lock at the end.
    let mut items = read_log(home)?
        .actions
        .into_iter()
        .find(|a| a.id == activity_id)
        .ok_or("No such action")?
        .items;
    let mut restored = Vec::new();
    for item in items.iter_mut() {
        if item.restored {
            continue;
        }
        let skip = |reason: &str| Skipped {
            path: item.path.clone(),
            reason: reason.to_string(),
        };
        if let Some(why) = item.interrupted.clone() {
            result.skipped.push(skip(&why));
            continue;
        }
        let Some(trash_path) = item.trash_path.clone() else {
            result.skipped.push(skip("Trash location wasn't recorded"));
            continue;
        };
        let from = PathBuf::from(&trash_path);
        let to = PathBuf::from(&item.path);
        let allowed_target = to.starts_with(home) || to.starts_with("/private/var/folders");
        if !from.starts_with(&trash_dir) || !allowed_target {
            result.skipped.push(skip("Location not allowed"));
            continue;
        }
        let Ok(now_there) = from.symlink_metadata() else {
            result.skipped.push(skip("No longer in the Trash"));
            continue;
        };
        if let Some(recorded) = item.identity
            && identity(&now_there) != recorded
        {
            result
                .skipped
                .push(skip("A different item is at that Trash location now"));
            continue;
        }
        if to.symlink_metadata().is_ok() {
            result
                .skipped
                .push(skip("Something is already at the original location"));
            continue;
        }
        if let Some(parent) = to.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            result.skipped.push(skip(&e.to_string()));
            continue;
        }
        // Write ahead: no intent on disk, no move.
        if let Err(why) = journal.begin(item) {
            result.skipped.push(skip(&why));
            continue;
        }
        // The move itself refuses to replace anything: a name taken since the
        // check above is left alone.
        match rightkit_fs::rename_exclusive(&from, &to).map_err(std::io::Error::from) {
            Ok(()) => {
                journal.finish(item, true);
                item.restored = true;
                restored.push(item.clone());
                result.restored_items += 1;
                result.restored_bytes += item.bytes;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                journal.finish(item, false);
                result
                    .skipped
                    .push(skip("Something is already at the original location"));
            }
            Err(e) => {
                journal.finish(item, false);
                result.skipped.push(skip(&e.to_string()));
            }
        }
    }
    if !restored.is_empty() {
        mark_restored(home, activity_id, &restored)?;
    }
    journal.close();
    Ok(result)
}
