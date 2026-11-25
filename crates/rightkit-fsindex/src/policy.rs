//! Host-independent policy for a file-name index: what to leave out, how to read an ignore file,
//! how many crawl workers one crawl may use, how health reasons settle into a state, and the
//! one-line description of what is being indexed. Nothing here opens the index; a host calls these
//! with its own settings and file names. Moved out of Pulse's `disk_index.rs`.

use std::path::{Path, PathBuf};

/// Excluded folders when the host has no setting of its own: build output, dependency folders,
/// caches and tool scratch. They come in addition to the crate's built-in set.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "target",
    "node_modules",
    ".git/objects",
    "DerivedData",
    ".build",
    "build",
    "dist",
    "Library/Caches",
    "Library/Logs",
    ".Trash",
    ".gradle",
    ".cache",
    "__pycache__",
    "venv",
    ".venv",
    ".rightkit-managed",
    ".rustright",
    ".build-home",
];

/// Whether `name` has one of `exts` (lowercase, no dot) and is not a bare dot-file.
pub fn extension_is(name: &str, exts: &[String]) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, found)| {
        !stem.is_empty() && exts.iter().any(|ext| found.eq_ignore_ascii_case(ext))
    })
}

/// Moves a health record to the state `next` describes. The record is `state` ("healthy" or
/// "unhealthy"), its `reasons` (sorted, no repeats, stable text with no changing numbers) and
/// `since` (Unix seconds, when the state last changed). `since` changes only when the state does,
/// so it stays the start of an unhealthy period while its reasons change. True when the state or
/// the reason set changed.
pub fn settle_health(
    state: &mut String,
    reasons: &mut Vec<String>,
    since: &mut i64,
    mut next: Vec<String>,
    now: i64,
) -> bool {
    next.sort();
    next.dedup();
    if next == *reasons {
        return false;
    }
    let wanted = if next.is_empty() {
        "healthy"
    } else {
        "unhealthy"
    };
    if wanted != state.as_str() {
        *state = wanted.into();
        *since = now;
    }
    *reasons = next;
    true
}

/// Workers for one crawl: what the crate asked for, at most 8 and at most half the CPUs. The crate
/// draws its own crawl threads from a process-wide budget of 8 that it does not expose, so a pool
/// outside the crate applies the cap itself.
pub fn crawl_threads(asked: usize) -> usize {
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    asked.clamp(1, (cpus / 2).clamp(1, 8))
}

/// Largest ignore file read.
pub const IGNORE_FILE_MAX: u64 = 256 * 1024;

#[cfg(all(unix, any(target_os = "macos", target_os = "ios")))]
const O_NONBLOCK_NOFOLLOW: i32 = 0x0004 | 0x0100;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
const O_NONBLOCK_NOFOLLOW: i32 = 0o4000 | 0o400000;

/// The outcome of reading an ignore file in a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreRead {
    /// No such file.
    Absent,
    /// The file was there and was not read; the text says why (for one log event).
    Skipped(&'static str),
    /// The file's text.
    Text(String),
}

/// Reads `root/<file_name>`: only a regular, non-symlink file of at most [`IGNORE_FILE_MAX`] bytes
/// is read. On Unix it is opened without following links or blocking, and the read is capped, so a
/// FIFO, device or huge file can neither hang nor grow memory.
pub fn read_ignore_file(root: &Path, file_name: &str) -> IgnoreRead {
    use std::io::Read;
    let path = root.join(file_name);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => return IgnoreRead::Absent,
    };
    if !meta.file_type().is_file() {
        return IgnoreRead::Skipped("not a regular file");
    }
    if meta.len() > IGNORE_FILE_MAX {
        return IgnoreRead::Skipped("larger than 256 KiB");
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(O_NONBLOCK_NOFOLLOW);
    }
    let Ok(file) = options.open(&path) else {
        return IgnoreRead::Skipped("cannot open");
    };
    if !file.metadata().is_ok_and(|m| m.file_type().is_file()) {
        return IgnoreRead::Skipped("not a regular file");
    }
    let mut bytes = Vec::new();
    if file.take(IGNORE_FILE_MAX).read_to_end(&mut bytes).is_err() {
        return IgnoreRead::Skipped("read failed");
    }
    IgnoreRead::Text(String::from_utf8_lossy(&bytes).into_owned())
}

/// Ignore-file lines as exclude globs: `#` comments and blank lines skipped, a leading `/` and a
/// trailing `/` removed, `**/` dropped (a glob matches wherever its components appear). A `!`
/// negation is not supported and is skipped.
pub fn ignore_patterns(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .map(|l| {
            l.trim_start_matches('/')
                .trim_end_matches('/')
                .replace("**/", "")
                .replace('\\', "/")
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// Exclude globs with `\` written as `/`. `setting` is the host's list, or `None` for
/// [`DEFAULT_EXCLUDES`]; `ignore_text` is the text of the root's ignore file, if one was read.
pub fn exclude_patterns(setting: Option<Vec<String>>, ignore_text: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = match setting {
        Some(list) => list,
        None => DEFAULT_EXCLUDES.iter().map(|s| (*s).to_string()).collect(),
    };
    for line in out.iter_mut() {
        *line = line.replace('\\', "/");
    }
    if let Some(text) = ignore_text {
        out.extend(ignore_patterns(text));
    }
    out
}

/// The status line for an index scope: "Indexing: home, /Applications, /Volumes/D; excluding 12
/// patterns (...)". `excluded` is the number of editable exclude patterns, `whole_disk` whether the
/// scope is the whole disk, and `is_home` names the roots shown as "home".
pub fn scope_label(
    roots: &[PathBuf],
    excluded: usize,
    whole_disk: bool,
    is_home: &dyn Fn(&Path) -> bool,
) -> String {
    if whole_disk {
        let extra = roots.iter().filter(|r| r.as_os_str() != "/").count();
        return if extra == 0 {
            format!("Indexing: whole disk; excluding {excluded} patterns")
        } else {
            format!("Indexing: whole disk (/ and {extra} more); excluding {excluded} patterns")
        };
    }
    let names: Vec<String> = roots
        .iter()
        .map(|r| {
            if is_home(r) {
                "home".to_string()
            } else {
                r.to_string_lossy().into_owned()
            }
        })
        .collect();
    format!(
        "Indexing: {}; excluding {excluded} patterns (the built-in excludes always apply)",
        names.join(", ")
    )
}
