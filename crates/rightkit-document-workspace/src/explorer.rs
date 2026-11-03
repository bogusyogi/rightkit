//! Backend for the explorer sidebar: system locations, one-level directory listings,
//! per-folder change watches, and OS-level reveal.
//!
//! SECURITY MODEL (mirrors `list_sibling_images` in `document_io.rs`): the renderer may
//! enumerate directory *metadata* freely — names, kinds, sizes, mtimes — but file
//! *content* only ever flows through a `PathBroker` grant. Listing a folder grants every
//! *supported* file inside it: opening that row is exactly what the sidebar exists for,
//! and the grant is scoped to files the user can see and click. Unsupported files come
//! back as `kind: "other"` with a bare canonical path and NO grant, so they can be
//! revealed or have their path copied but can never be read by `read_document`. Every
//! entry is canonicalized and dropped if its real path escapes the listed directory, so
//! a symlink can never smuggle a grant to a file outside the folder the user opened.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::document_io::natural_cmp;
use crate::documents::DocumentBackend;
use crate::error::WorkspaceError;
use crate::grants::PathGrants;
use crate::watch::WorkspaceWatcher;

/// The display spelling of a path: lossy UTF-8, without the Windows verbatim (`\\?\`) prefix.
pub fn path_to_string(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{stripped}");
        }
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    raw.into_owned()
}

/// Max entries returned per folder; `truncated` tells the UI the listing was cut rather
/// than that the folder simply ended.
const LIST_DIR_CAP: usize = 5000;

/// Names that are never listed (any case): Finder/Explorer droppings and the recycle
/// bin. Dot-prefixed names are filtered separately — they're already hidden by
/// convention — but `$RECYCLE.BIN` and `desktop.ini` don't start with a dot on Windows.
const JUNK_NAMES: [&str; 3] = [".ds_store", "desktop.ini", "$recycle.bin"];

#[derive(Debug, Serialize, Clone)]
pub struct ExplorerLocation {
    /// Stable, unique id ("home", "desktop", "volume:/Volumes/X", "root", ...).
    pub id: String,
    pub label: String,
    /// "home" | "desktop" | "documents" | "downloads" | "icloud" | "onedrive" |
    /// "volume" | "root".
    pub kind: String,
    /// Display path of the location's root directory.
    pub dir: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ExplorerEntry {
    /// The name as it appears in the directory (a symlink keeps its own name even though
    /// `path` is the resolved target).
    pub name: String,
    /// "dir" | a supported format id from `DocumentBackend::format_of` | "other".
    pub kind: String,
    /// Unix epoch millis of the target's mtime; null when the FS won't report one.
    pub modified: Option<i64>,
    /// Byte size for files; null for directories.
    pub size: Option<i64>,
    /// Dirs: canonical dir path. Supported files: broker-granted canonical path.
    /// "other" files: bare canonical path with NO grant (see module doc).
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ExplorerListing {
    pub entries: Vec<ExplorerEntry>,
    pub truncated: bool,
}

/// Map a filesystem error on `path` to the typed envelope: ENOENT → `FileNotFound`,
/// EACCES → `PermissionDenied` (with the TCC-style hint the UI surfaces), else `Io`.
/// `app_name` names the product in the permission hint. Public so `library::toggle_pin_dir`
/// reports identical error classes.
pub fn fs_io_error(app_name: &str, path: &str, e: std::io::Error) -> WorkspaceError {
    match e.kind() {
        std::io::ErrorKind::NotFound => {
            WorkspaceError::FileNotFound(format!("folder not found: {path}"))
        }
        std::io::ErrorKind::PermissionDenied => {
            WorkspaceError::PermissionDenied(permission_denied_message(app_name, path))
        }
        _ => WorkspaceError::Io(e.to_string()),
    }
}

/// User-facing permission hint naming the folder. On macOS an EACCES here is usually a
/// TCC (Files and Folders) denial, so the message points at the System Settings pane
/// where the user grants it; other platforms get a permissions phrasing.
fn permission_denied_message(app_name: &str, path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.to_string());
    #[cfg(target_os = "macos")]
    {
        format!("{app_name} needs access to {name} — allow in System Settings ▸ Privacy & Security ▸ Files and Folders")
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!("{app_name} needs access to {name} — check the folder's permissions")
    }
}

/// A sidebar location the OS offers that isn't currently on disk is skipped — the list
/// only advertises folders that actually exist (an empty %OneDrive% var, an unmounted
/// share, a missing ~/Desktop on a server account all drop out here).
fn push_location(
    out: &mut Vec<ExplorerLocation>,
    id: String,
    label: &str,
    kind: &str,
    dir: PathBuf,
) {
    if dir.is_dir() {
        out.push(ExplorerLocation {
            id,
            label: label.to_string(),
            kind: kind.to_string(),
            dir: path_to_string(&dir),
        });
    }
}

/// The home folder a listing starts from, as the host resolves it (so test and QA builds never
/// list the real home).
#[derive(Debug, Clone, Default)]
pub struct HomeLocation {
    /// The home directory, when known.
    pub home: Option<PathBuf>,
    /// True when `home` is an isolated stand-in; the real known folders (Desktop, Documents,
    /// Downloads) are then never asked for, since they would point at the user's own folders.
    pub isolated: bool,
}

/// The sidebar locations the OS offers that exist on disk.
pub fn explorer_locations(location: &HomeLocation) -> Vec<ExplorerLocation> {
    collect_locations(location)
}

fn collect_locations(location: &HomeLocation) -> Vec<ExplorerLocation> {
    let mut out = Vec::new();
    let home = location.home.clone();
    if let Some(home) = &home {
        push_location(&mut out, "home".into(), "Home", "home", home.clone());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // dirs::* resolves the platform-correct (possibly localized/redirected)
        // folder; the conventional subfolder under home is the fallback when it doesn't
        // answer (e.g. a Windows account with no known-folder mapping).
        // An isolated home (tests, real-app QA) never asks for the real known folders: they
        // would point back at the user's own Desktop, Documents and Downloads.
        let known = |real: fn() -> Option<PathBuf>| {
            if location.isolated {
                None
            } else {
                real()
            }
        };
        for (id, label, resolved, fallback) in [
            ("desktop", "Desktop", known(dirs::desktop_dir), "Desktop"),
            (
                "documents",
                "Documents",
                known(dirs::document_dir),
                "Documents",
            ),
            (
                "downloads",
                "Downloads",
                known(dirs::download_dir),
                "Downloads",
            ),
        ] {
            let dir = resolved.or_else(|| home.as_ref().map(|h| h.join(fallback)));
            if let Some(dir) = dir {
                push_location(&mut out, id.into(), label, id, dir);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // iCloud Drive only exists once the user signs in / enables Desktop & Documents
        // sync — `push_location` drops it otherwise.
        if let Some(home) = &home {
            let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
            push_location(&mut out, "icloud".into(), "iCloud Drive", "icloud", icloud);
        }
        // Every real volume mounted under /Volumes. The boot volume appears here as a
        // symlink to "/" (e.g. "Macintosh HD" -> /) — skip it: Home already covers the
        // startup disk, and a second entry pointing at "/" would duplicate the whole
        // drive in the tree.
        if let Ok(volumes) = std::fs::read_dir("/Volumes") {
            for entry in volumes.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(canon) = std::fs::canonicalize(entry.path()) else {
                    continue;
                };
                if !canon.is_dir() || canon == Path::new("/") || mac_volume_not_browsable(&canon) {
                    continue;
                }
                push_location(
                    &mut out,
                    format!("volume:{}", path_to_string(&canon)),
                    &name,
                    "volume",
                    canon,
                );
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(one) = std::env::var_os("OneDrive").map(PathBuf::from) {
            push_location(&mut out, "onedrive".into(), "OneDrive", "onedrive", one);
        }
        // GetLogicalDrives bitmask + GetDriveTypeW avoid probing every letter with
        // `is_dir()`, which can stall on disconnected network drives. Network drives
        // (DRIVE_REMOTE) are intentionally not listed as locations.
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
        const DRIVE_NO_ROOT_DIR: u32 = 1;
        const DRIVE_REMOTE: u32 = 4;
        // SAFETY: GetLogicalDrives takes no arguments and has no preconditions.
        let mask = unsafe { GetLogicalDrives() };
        for (bit, letter) in (b'A'..=b'Z').enumerate() {
            if mask & (1u32 << bit) == 0 {
                continue;
            }
            let spec = format!("{}:\\", letter as char);
            let wide: Vec<u16> = spec.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
            let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
            if kind == DRIVE_NO_ROOT_DIR || kind == DRIVE_REMOTE {
                continue;
            }
            let dir = PathBuf::from(&spec);
            if dir.is_dir() {
                push_location(&mut out, format!("volume:{spec}"), &spec, "volume", dir);
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        push_location(&mut out, "root".into(), "/", "root", PathBuf::from("/"));
    }

    out
}

/// Volumes macOS mounts with `nobrowse` (Recovery, Time Machine local snapshots, installer
/// and helper disk images) are hidden from Finder's sidebar; hide them here too.
#[cfg(target_os = "macos")]
fn mac_volume_not_browsable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    const MNT_DONTBROWSE: u32 = 0x0010_0000;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and `info` is a valid out-pointer for statfs.
    if unsafe { libc::statfs(c_path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return true;
    }
    // SAFETY: statfs returned 0, so it fully initialized `info`.
    let info = unsafe { info.assume_init() };
    info.f_flags & MNT_DONTBROWSE != 0
}

/// Everything [`scan_dir`] knows about one directory entry, pre-grant. `canon` is the
/// resolved (symlink-free) path — the only form the broker and the renderer ever see.
struct RawEntry {
    name: String,
    is_dir: bool,
    canon: PathBuf,
    modified: Option<i64>,
    size: Option<i64>,
}

/// A `keep` value is one child NAME. Empty values, and values that could name a path (a
/// separator, or `..`), are ignored, so a kept name can only select an entry the listing
/// already has after every filter.
fn child_name(keep: Option<&str>) -> Option<&str> {
    keep.filter(|n| !n.is_empty() && !n.contains("..") && !n.contains(['/', '\\']))
}

/// Read ONE level of `dir` (canonicalized first): hidden names and OS junk skipped,
/// dirs-then-files each natural-sorted, capped at `cap` entries (`truncated` reports the
/// cut). `keep` names one child that stays listed even when the cap would cut it (the
/// sidebar passes the next segment of the file it is revealing). Entries whose canonical
/// path escapes the listed directory — symlinks pointing outside it — are dropped here so
/// nothing downstream can grant them (the `list_sibling_images` containment rule).
fn scan_dir_keeping(
    app_name: &str,
    dir: &str,
    cap: usize,
    keep: Option<&str>,
) -> Result<(Vec<RawEntry>, bool), WorkspaceError> {
    let canon_dir = std::fs::canonicalize(dir).map_err(|e| fs_io_error(app_name, dir, e))?;
    if !canon_dir.is_dir() {
        return Err(WorkspaceError::FileNotFound(format!("not a folder: {dir}")));
    }
    let read = std::fs::read_dir(&canon_dir).map_err(|e| fs_io_error(app_name, dir, e))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || JUNK_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        // Canonical containment: read_dir + canonicalize follow symlinks, so without
        // this check a symlink in the folder pointing elsewhere on disk would surface —
        // and for supported types, get a broker grant — for a file outside the folder
        // the user actually listed. Dropped entries never reach `finish_listing`, so
        // they can never receive a grant.
        let Ok(canon) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if !canon.starts_with(&canon_dir) {
            continue;
        }
        // A link that resolves to the listed folder itself (Windows' legacy
        // `AppData\Local\Application Data` junction, or `ln -s . loop`) would be returned
        // with the folder's own path: an expanded folder would then contain itself, and
        // the tree would recurse without end.
        if canon == canon_dir {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&canon) else {
            continue;
        };
        let is_dir = meta.is_dir();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        let raw = RawEntry {
            name,
            is_dir,
            canon,
            modified,
            size: (!is_dir).then_some(meta.len() as i64),
        };
        if is_dir {
            dirs.push(raw);
        } else {
            files.push(raw);
        }
    }
    dirs.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    files.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    let truncated = dirs.len() + files.len() > cap;
    let mut all = dirs;
    all.extend(files);
    // The kept child is appended after the cut rather than swapped in for an entry. Every
    // entry that survives the cut sorts before it (its index is past `cap`), so the result
    // stays in listing order; a dir can only sit past the cap when every kept entry is a dir,
    // so the dirs-then-files grouping holds too. At most `cap + 1` entries are returned, and
    // without a kept child the cut is unchanged.
    let kept = child_name(keep)
        .and_then(|name| all.iter().position(|e| e.name == name))
        .filter(|&i| i >= cap)
        .map(|i| all.remove(i));
    all.truncate(cap);
    all.extend(kept);
    Ok((all, truncated))
}

/// Turn raw entries into the wire shape. Dirs keep their canonical path; supported
/// files are broker-granted (a grant race — file deleted between scan and grant — drops
/// the row); unsupported files report `kind: "other"` with a bare canonical path and
/// are never granted, per the module's security model.
fn finish_listing(
    raws: Vec<RawEntry>,
    truncated: bool,
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
) -> ExplorerListing {
    let mut entries = Vec::with_capacity(raws.len());
    for raw in raws {
        let kind = if raw.is_dir {
            "dir"
        } else {
            match docs.format_of(&path_to_string(&raw.canon)) {
                "unknown" | "other" => "other",
                supported => supported,
            }
        };
        let path = if raw.is_dir || kind == "other" {
            path_to_string(&raw.canon)
        } else {
            match grants.grant_existing_file_path(&raw.canon) {
                Ok(granted) => granted,
                Err(_) => continue,
            }
        };
        entries.push(ExplorerEntry {
            name: raw.name,
            kind: kind.to_string(),
            modified: raw.modified,
            size: raw.size,
            path,
        });
    }
    ExplorerListing { entries, truncated }
}

/// List one level of a directory for the explorer tree. The directory itself needs no grant (it
/// is not file content) while the supported files inside are granted so the renderer can open
/// them (see the module doc for the full model). `keep` is the NAME of one child to list even
/// past the cap (see `scan_dir_keeping`).
///
/// `on_timing` receives the JSON detail record for the listing (entry count, truncation and
/// duration) so the host can log it.
pub fn explorer_list_dir(
    app_name: &str,
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    dir: &str,
    keep: Option<&str>,
    on_timing: &dyn Fn(&str),
) -> Result<ExplorerListing, WorkspaceError> {
    let started = std::time::Instant::now();
    let (raws, truncated) = scan_dir_keeping(app_name, dir, LIST_DIR_CAP, child_name(keep))?;
    let listing = finish_listing(raws, truncated, grants, docs);
    on_timing(
        &serde_json::json!({
            "entries": listing.entries.len(),
            "truncated": listing.truncated,
            "durationMs": started.elapsed().as_millis() as u64,
        })
        .to_string(),
    );
    Ok(listing)
}

/// Watch a folder for changes; the host's watcher emits the change event carrying the string
/// passed here after its debounce. Idempotent on the canonical dir; the host caps concurrent
/// watches.
pub fn explorer_watch_dir(
    app_name: &str,
    watcher: &dyn WorkspaceWatcher,
    dir: &str,
) -> Result<(), WorkspaceError> {
    let canon = std::fs::canonicalize(dir).map_err(|e| fs_io_error(app_name, dir, e))?;
    if !canon.is_dir() {
        return Err(WorkspaceError::FileNotFound(format!("not a folder: {dir}")));
    }
    watcher
        .watch_dir(&canon, dir)
        .map_err(WorkspaceError::Watch)
}

/// Stop watching a folder. Idempotent: unwatching an unwatched (or deleted) folder is a no-op, so
/// the frontend can fire this on collapse without tracking state.
pub fn explorer_unwatch_dir(watcher: &dyn WorkspaceWatcher, dir: &str) {
    // The watch is keyed by the canonical dir string, but the folder may have been deleted since
    // it was armed: pass the raw string too so the entry is found either way.
    let mut keys = vec![dir.to_string()];
    if let Ok(canon) = std::fs::canonicalize(dir) {
        keys.insert(0, path_to_string(&canon));
    }
    watcher.unwatch_dirs(&keys);
}

/// Reveal a folder or an unsupported file in the OS file manager. No grant is required: this only
/// *displays* the path in Finder/Explorer, it reads no file content. Canonicalize still runs so
/// the revealed path is the real, unambiguous one.
pub fn explorer_reveal(app_name: &str, path: &str) -> Result<(), WorkspaceError> {
    let canon = std::fs::canonicalize(path).map_err(|e| fs_io_error(app_name, path, e))?;
    // Selects the item in its parent window (a folder too); the scope is the one path.
    let scope = rightkit_child_process::reveal::PathScope::new([&canon])
        .map_err(|e| WorkspaceError::Other(e.to_string()))?;
    rightkit_child_process::reveal::reveal(&canon, &scope)
        .map_err(|e| WorkspaceError::Other(e.to_string()))
}
