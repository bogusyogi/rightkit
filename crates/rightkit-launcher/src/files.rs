//! File-name search for the launcher, over the account home, the Start Menu, the volumes chosen
//! in the hub and the folders chosen for file search.
//!
//! The index holds names only: one list of directory paths and, per file, a row into one
//! string arena of names. It is built on a background thread, kept between panel opens and
//! rebuilt only when its scope (roots, excludes, `ignore files) changes or it is older than
//! `STALE_AFTER`. The walk never follows a reparse point (symlink or junction) and never leaves
//! the chosen roots.

use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// Deepest folder level walked below each root.
const MAX_DEPTH: u8 = 32;

/// Memory the index may hold: names, rows and folder paths.
const MEMORY_BUDGET: usize = 64 * 1024 * 1024;

/// Bytes one file row costs besides its name (directory index, offset, length, padding).
const ROW_BYTES: usize = 12;

/// Bytes one folder costs besides its path (the `String` header and the queue entry).
const DIR_BYTES: usize = 32;

/// An index older than this is rebuilt on the next panel open.
const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// Most lines read from one the ignore file, and the largest file read.
const MAX_IGNORE_LINES: usize = 256;
const MAX_IGNORE_BYTES: u64 = 64 * 1024;

/// The excludes used until the hub stores its own list (the manifest's Windows default for
/// `hubFileIndexExcludes`).
pub const DEFAULT_EXCLUDES: [&str; 12] = [
    "target",
    "node_modules",
    ".git\\objects",
    "build",
    "dist",
    ".cache",
    "__pycache__",
    "venv",
    ".venv",
    "AppData\\Local\\Temp",
    "$Recycle.Bin",
    "System Volume Information",
];

/// Queries shorter than this find nothing, as on the Mac.
const MIN_QUERY: usize = 2;

/// How many names a search reads between looks at its cancel flag.
const CANCEL_EVERY: usize = 2048;

/// Win32 file attributes that mark an entry as skipped.
#[cfg(windows)]
const ATTR_HIDDEN: u32 = 0x2;
#[cfg(windows)]
const ATTR_SYSTEM: u32 = 0x4;
#[cfg(windows)]
const ATTR_REPARSE_POINT: u32 = 0x400;
/// Cloud Files (OneDrive and other cfapi) placeholders: data is not local, so opening it downloads.
#[cfg(windows)]
const ATTR_OFFLINE: u32 = 0x1000;
#[cfg(windows)]
const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
#[cfg(windows)]
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
#[cfg(windows)]
const ATTR_CLOUD: u32 =
    ATTR_REPARSE_POINT | ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS;

/// One search hit.
#[derive(Debug, Clone)]
pub struct Found {
    /// File name with its extension.
    pub name: String,
    /// Full path to the file.
    pub path: String,
    /// The folder that holds the file.
    pub folder: String,
}

/// One root to walk and the exclude globs that apply under it.
#[derive(Debug, Clone)]
struct Root {
    path: PathBuf,
    /// Exclude patterns: lower-case, `/`-separated, split into path components.
    excludes: Vec<Vec<String>>,
}

/// What a build covers, and a key that changes when any of it does.
#[derive(Debug, Clone)]
struct Scope {
    roots: Vec<Root>,
    key: String,
    sig: String,
}

/// The built index; cheap to clone (the data is shared).
#[derive(Debug, Clone)]
pub struct Index {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Every folder walked, by path.
    dirs: Vec<String>,
    /// All file names, back to back.
    names: String,
    /// Each file as (index into `dirs`, offset into `names`, length).
    files: Vec<(u32, u32, u16)>,
    /// The walk stopped at the memory budget with more left unread, so a file may be missing.
    capped: bool,
    key: String,
    /// The file-system-free part of the scope (settings and chosen folders), for `cached`.
    sig: String,
    built: Instant,
}

/// The last finished index and the scope key of a build in progress.
struct Cache {
    ready: Option<Index>,
    building: Option<String>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    ready: None,
    building: None,
});

/// The kept index when it still fits the settings and is not stale; the launcher shows it at once.
pub fn cached(folders: &[String]) -> Option<Index> {
    // No file-system access here (this runs on the UI thread): the settings and chosen folders
    // are compared, and a changed the ignore file is picked up when the index goes stale.
    let sig = signature(folders);
    let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache
        .ready
        .as_ref()
        .filter(|index| index.inner.sig == sig && index.inner.built.elapsed() < STALE_AFTER)
        .cloned()
}

/// Runs a build on a new thread at below-normal priority. The caller keeps the handle and joins it
/// once it has finished; `None` when the thread cannot start. A build already running for the same
/// scope is waited for, not repeated, so closing and reopening the panel never starts a second walk.
pub fn build_in_background(folders: Vec<String>) -> Option<std::thread::JoinHandle<Index>> {
    std::thread::Builder::new()
        .name("launcher-files".into())
        .spawn(move || {
            lower_priority();
            let scope = scope(&folders);
            loop {
                {
                    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(index) = cache.ready.as_ref().filter(|index| {
                        index.inner.key == scope.key && index.inner.built.elapsed() < STALE_AFTER
                    }) {
                        return index.clone();
                    }
                    if cache.building.as_deref() != Some(scope.key.as_str()) {
                        cache.building = Some(scope.key.clone());
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            // Frees the "building" claim even when the walk panics, so waiters never hang.
            struct Claim;
            impl Drop for Claim {
                fn drop(&mut self) {
                    CACHE
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .building = None;
                }
            }
            let claim = Claim;
            let index = build(&scope);
            CACHE.lock().unwrap_or_else(PoisonError::into_inner).ready = Some(index.clone());
            drop(claim);
            index
        })
        .ok()
}

#[cfg(windows)]
fn lower_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: changes only the calling thread's own priority.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

#[cfg(not(windows))]
fn lower_priority() {}

/// The settings-and-folders part of the scope; reads no files.
fn signature(folders: &[String]) -> String {
    let (whole_disk, volumes, excludes) = crate::host::file_index_scope();
    format!(
        "{whole_disk}|{}|{}|{}",
        volumes.join("\u{1}"),
        excludes.map_or_else(|| "default".to_string(), |list| list.join("\u{1}")),
        folders.join("\u{1}")
    )
}

/// Runs `probe` on a helper thread and waits two seconds; `None` on timeout (an offline volume).
fn with_deadline<T: Send + 'static>(probe: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("launcher-files-probe".into())
        .spawn(move || {
            let _ = tx.send(probe());
        })
        .ok()?;
    rx.recv_timeout(Duration::from_secs(2)).ok()
}

/// The folders to walk and the key for them, from the hub's settings and `folders`
/// (`fileFolders`).
fn scope(folders: &[String]) -> Scope {
    let (whole_disk, volumes, excludes) = crate::host::file_index_scope();
    let excludes: Vec<String> = excludes.unwrap_or_else(|| {
        DEFAULT_EXCLUDES
            .iter()
            .map(|item| (*item).to_string())
            .collect()
    });
    let global: Vec<Vec<String>> = excludes.iter().filter_map(|item| pattern(item)).collect();
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        paths.push(PathBuf::from(home));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(Path::new(&appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(common) = std::env::var_os("ProgramData") {
        paths.push(Path::new(&common).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    let system = std::env::var_os("SystemDrive")
        .map(|drive| drive.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| "c:".to_string());
    for volume in &volumes {
        let path = PathBuf::from(volume.trim());
        let text = norm(&path);
        // The system drive as a whole and the Windows folder are never a chosen volume.
        if text.trim_end_matches('/') == system || text.starts_with(&format!("{system}/windows")) {
            continue;
        }
        paths.push(path);
    }
    for folder in folders {
        paths.push(PathBuf::from(folder));
    }
    if whole_disk {
        paths.extend(fixed_drives());
    }
    let windows_dir = std::env::var_os("SystemRoot").map(|dir| norm(Path::new(&dir)));
    // Existing, real directories only. A root inside another is kept (the outer walk may not reach
    // it: hidden folders, depth); `build` walks outer roots first and never lists a folder twice.
    let mut kept: Vec<(PathBuf, String)> = Vec::new();
    for path in paths {
        let text = norm(&path);
        if windows_dir.as_deref().is_some_and(|dir| within(&text, dir)) {
            continue;
        }
        if kept.iter().any(|(_, seen)| *seen == text) {
            continue;
        }
        kept.push((path, text));
    }
    kept.sort_by_key(|(_, text)| text.len());
    let mut key = format!("whole={whole_disk}");
    key.push_str(&format!("|ex={}", excludes.join("\u{1}")));
    let mut roots = Vec::new();
    for (path, text) in kept {
        let mut excludes = global.clone();
        // The Windows folder is never walked, whichever root reaches it.
        excludes.extend(windows_dir.as_deref().and_then(pattern));
        let probe_path = path.clone();
        // One deadline-guarded probe per root: exists as a real directory, and its ignore file.
        let probe = with_deadline(move || {
            if !real_dir(&probe_path) {
                return None;
            }
            let ignore = probe_path.join(crate::host::ignore_file_name());
            // symlink_metadata never follows or hydrates; a link or cloud placeholder is not read.
            let stamp = std::fs::symlink_metadata(&ignore)
                .ok()
                .filter(|meta| {
                    meta.is_file() && !hidden_attrs_reparse(meta) && meta.len() <= MAX_IGNORE_BYTES
                })
                .map(|meta| meta.modified().ok());
            let body = if stamp.is_some() {
                std::fs::read_to_string(&ignore).ok()
            } else {
                None
            };
            Some((stamp, body))
        })
        .flatten();
        // A root that timed out or is not a directory is unavailable.
        let Some((stamp, body)) = probe else {
            continue;
        };
        if let Some(body) = body {
            excludes.extend(
                body.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .take(MAX_IGNORE_LINES)
                    .filter_map(pattern),
            );
        }
        key.push_str(&format!(
            "|{text}@{}",
            stamp
                .flatten()
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |span| span.as_secs())
        ));
        roots.push(Root { path, excludes });
    }
    Scope {
        roots,
        key,
        sig: signature(folders),
    }
}

/// A pattern as lower-case components, `\` read as `/`. `None` for an empty pattern.
fn pattern(text: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = text
        .trim()
        .replace('\\', "/")
        .to_lowercase()
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// The path in lower case with `/` separators, the form patterns are matched against.
fn norm(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// True when `path` is `base` or below it (both from `norm`).
fn within(path: &str, base: &str) -> bool {
    let base = base.trim_end_matches('/');
    path.trim_end_matches('/') == base
        || path
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// True for a directory that is not itself a symlink or junction.
fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.is_dir() && !hidden_attrs_reparse(&meta))
        .unwrap_or(false)
}

#[cfg(windows)]
fn fixed_drives() -> Vec<PathBuf> {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows::core::PCWSTR;
    const DRIVE_FIXED: u32 = 3;
    // SAFETY: plain queries; the wide strings outlive each call.
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let root = format!("{}:\\", char::from(b'A' + index as u8));
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) } == DRIVE_FIXED {
            drives.push(PathBuf::from(root));
        }
    }
    drives
}

#[cfg(not(windows))]
fn fixed_drives() -> Vec<PathBuf> {
    Vec::new()
}

/// True when the last components of `path` (lower-case, `/`-separated) match any pattern. A
/// pattern with a drive (`:`) is a path prefix; otherwise its components are matched against the
/// same number of trailing components, each a glob with `*` and `?`.
fn excluded(path: &str, patterns: &[Vec<String>]) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    patterns.iter().any(|pattern| {
        if pattern[0].contains(':') {
            let joined = pattern.join("/");
            return within(path, &joined);
        }
        parts.len() >= pattern.len()
            && parts[parts.len() - pattern.len()..]
                .iter()
                .zip(pattern)
                .all(|(part, glob)| glob_match(glob.as_bytes(), part.as_bytes()))
    })
}

/// `*` (any run) and `?` (one byte) over lower-case text.
fn glob_match(glob: &[u8], text: &[u8]) -> bool {
    let (mut g, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if g < glob.len() && (glob[g] == b'?' || glob[g] == text[t]) {
            g += 1;
            t += 1;
        } else if g < glob.len() && glob[g] == b'*' {
            star = Some((g, t));
            g += 1;
        } else if let Some((sg, st)) = star {
            g = sg + 1;
            t = st + 1;
            star = Some((sg, st + 1));
        } else {
            return false;
        }
    }
    glob[g..].iter().all(|byte| *byte == b'*')
}

/// Builds the index for `scope` on the calling thread, breadth first so the cap leaves shallow
/// files rather than one deep tree.
fn build(scope: &Scope) -> Index {
    let mut dirs: Vec<String> = Vec::new();
    let mut names = String::new();
    let mut files: Vec<(u32, u32, u16)> = Vec::new();
    let mut used = 0usize;
    let mut capped = false;
    // Folders already walked (by hash of the normalised path), so nested roots never list twice.
    let mut seen: HashSet<u64> = HashSet::new();
    let mut mark = |path: &Path| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        norm(path).trim_end_matches('/').hash(&mut hasher);
        seen.insert(hasher.finish())
    };
    'roots: for root in &scope.roots {
        if !mark(&root.path) {
            continue;
        }
        let mut queue: VecDeque<(u32, u8)> = VecDeque::new();
        let start = root.path.to_string_lossy().into_owned();
        used += start.len() + DIR_BYTES;
        dirs.push(start);
        queue.push_back((dirs.len() as u32 - 1, MAX_DEPTH));
        while let Some((dir_index, depth)) = queue.pop_front() {
            let dir = PathBuf::from(&dirs[dir_index as usize]);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if used >= MEMORY_BUDGET {
                    capped = true;
                    break 'roots;
                }
                // DirEntry::metadata does not follow links, so symlinks and junctions show here.
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || meta.file_type().is_symlink() || hidden_attrs(&meta) {
                    continue;
                }
                if meta.is_dir() {
                    if depth == 0 {
                        continue;
                    }
                    let path = entry.path();
                    if excluded(&norm(&path), &root.excludes) || !mark(&path) {
                        continue;
                    }
                    let text = path.to_string_lossy().into_owned();
                    used += text.len() + DIR_BYTES;
                    dirs.push(text);
                    queue.push_back((dirs.len() as u32 - 1, depth - 1));
                } else if let Ok(len) = u16::try_from(name.len()) {
                    used += name.len() + ROW_BYTES;
                    files.push((dir_index, names.len() as u32, len));
                    names.push_str(&name);
                }
            }
        }
    }
    dirs.shrink_to_fit();
    names.shrink_to_fit();
    files.shrink_to_fit();
    Index {
        inner: Arc::new(Inner {
            dirs,
            names,
            files,
            capped,
            key: scope.key.clone(),
            sig: scope.sig.clone(),
            built: Instant::now(),
        }),
    }
}

/// True when Windows marks the entry hidden, system, or a reparse point.
#[cfg(windows)]
fn hidden_attrs(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    (meta.file_attributes() & (ATTR_HIDDEN | ATTR_SYSTEM | ATTR_CLOUD)) != 0
}

/// True when Windows marks the entry a reparse point (junction, symlink, mount point).
#[cfg(windows)]
fn hidden_attrs_reparse(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    (meta.file_attributes() & ATTR_CLOUD) != 0
}

/// Non-Windows builds have no such attributes; dot-names are skipped by the caller.
#[cfg(not(windows))]
fn hidden_attrs(_meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(not(windows))]
fn hidden_attrs_reparse(_meta: &std::fs::Metadata) -> bool {
    false
}

impl Index {
    /// Whether the walk stopped at the memory budget, so the index holds only the shallowest
    /// entries and a search can miss a file.
    pub fn capped(&self) -> bool {
        self.inner.capped
    }

    /// The best `limit` file names matching `query`, by `score` (higher is better,
    /// `None` is no match). Matches file names only. Queries under two characters
    /// find nothing. `cancelled` is asked every few thousand names; when it says yes the
    /// walk stops and the answer is `None` (a newer keystroke made this one worthless).
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        score: impl Fn(&str, &str) -> Option<i32>,
        cancelled: impl Fn() -> bool,
    ) -> Option<Vec<Found>> {
        let query = query.trim();
        if query.chars().count() < MIN_QUERY || limit == 0 {
            return Some(Vec::new());
        }
        let mut scored: Vec<(i32, usize)> = Vec::new();
        for i in 0..self.inner.files.len() {
            if i % CANCEL_EVERY == 0 && cancelled() {
                return None;
            }
            if let Some(points) = score(query, self.name(i)) {
                scored.push((points, i));
            }
        }
        // Best score first; ties keep index order so results are stable between searches.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(limit);
        Some(scored.into_iter().map(|(_, i)| self.found(i)).collect())
    }

    /// The file name of row `i`.
    fn name(&self, i: usize) -> &str {
        let (_, offset, len) = self.inner.files[i];
        &self.inner.names[offset as usize..offset as usize + len as usize]
    }

    /// Builds the public hit for the file at `i`.
    fn found(&self, i: usize) -> Found {
        let name = self.name(i);
        let folder = &self.inner.dirs[self.inner.files[i].0 as usize];
        Found {
            name: name.to_string(),
            path: Path::new(folder).join(name).to_string_lossy().into_owned(),
            folder: folder.clone(),
        }
    }
}
