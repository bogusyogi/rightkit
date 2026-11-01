//! Export directory for the `attachment` method: the only place an agent
//! request can cause a file to be written. Layout `<root>/<agent id>/<file>`
//! under an app-owned root. No caller-supplied path or file name is used, and
//! no sender text reaches the disk name: a file is `attachment-<n><ext>` where
//! `<n>` is the next free number and `<ext>` is the original extension only
//! when it is on a short list of inert types (anything else is stored as
//! `.blocked`). The new file is created exclusively and atomically through a
//! descriptor-pinned `rightkit_fs::StateDir`, so a swapped or linked folder
//! cannot redirect it. Nothing is deleted automatically; the user clears the
//! directory in Settings.

use super::host::{host, FileScan};
use rightkit_fs::{canonicalize_plain, ensure_private_dir, EntryKind, FsError, StateDir};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Largest single exported file.
pub const FILE_CAP: u64 = 25 * 1024 * 1024;
/// Largest total of exported files per agent directory.
pub const DIR_CAP: u64 = 200 * 1024 * 1024;
/// Most files per agent directory.
pub const DIR_FILES_MAX: usize = 500;
/// Each file counts for at least this many bytes toward [`DIR_CAP`].
pub const MIN_CHARGE: u64 = 4096;
pub const DIR_FULL_MESSAGE: &str =
    "The export folder for this agent is full (200 MiB or 500 files). The user can clear it in the app's Settings, Agents.";
/// Extensions that are kept; every other one is stored as `.blocked`.
const INERT_EXT: &[&str] = &[
    "pdf", "txt", "csv", "png", "jpg", "jpeg", "gif", "webp", "heic", "tif", "tiff", "bmp", "docx",
    "xlsx", "pptx", "odt", "ods", "odp", "rtf", "md", "json", "xml", "eml", "ics", "vcf", "zip",
    "mp3", "wav", "m4a", "mp4", "mov",
];
/// Numbers tried before giving up (only a race can make one collide).
const NAME_TRIES: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportError {
    TooLarge,
    DirFull,
    /// Windows Defender flagged the file; it was removed.
    Blocked,
    /// The scan did not finish in time or could not run; the file was removed.
    ScanIncomplete,
    /// A path check failed (separator in an id, link or reparse escape).
    Unsafe,
    Io,
}

/// What a scan concluded about a written file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanVerdict {
    Clean,
    Threat,
    /// Not finished within the wait, or the scanner failed.
    Incomplete,
}

/// The app-owned root: `<data dir>/agent-exports`.
pub fn default_root() -> PathBuf {
    root_in(&host().data_dir())
}

/// The export folder under `data_dir`. The engine's Settings methods (`agents.import_file`,
/// `agents.clear_exports`) derive it from the data folder the core owns, so both halves of the
/// core name one place.
pub fn root_in(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("agent-exports")
}

pub fn agent_dir_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The extension a file is stored with, and whether it was replaced: the
/// original one (lower case) when it is on the inert list, else `.blocked`
/// with the original dropped. A name with no extension keeps none.
pub fn stored_extension(raw: &str) -> (String, bool) {
    let name = raw.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let Some(i) = name.rfind('.') else {
        return (String::new(), false);
    };
    let ext = &name[i + 1..];
    let lower = ext.to_ascii_lowercase();
    if ext.chars().all(|c| c.is_ascii_alphanumeric()) && INERT_EXT.contains(&lower.as_str()) {
        (format!(".{lower}"), false)
    } else {
        (".blocked".into(), true)
    }
}

/// What one pass over an agent directory found.
struct DirState {
    files: usize,
    charged: u64,
    /// One more than the highest `attachment-<n>` number present.
    next: u32,
}

/// The number in `attachment-<n>` or `attachment-<n>.<ext>`.
fn number_of(name: &str) -> Option<u32> {
    name.strip_prefix("attachment-")?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// How an unusable path maps to an [`ExportError`]: a link, a foreign owner,
/// open permissions or a bad name is `Unsafe`; everything else is `Io`.
fn classify(e: &FsError) -> ExportError {
    match e {
        FsError::Symlink(_)
        | FsError::NotADirectory(_)
        | FsError::NotAFile(_)
        | FsError::NotOwned(_)
        | FsError::TooPermissive(_)
        | FsError::UnsafeAcl(_)
        | FsError::IdentityChanged(_)
        | FsError::BadName(_) => ExportError::Unsafe,
        _ => ExportError::Io,
    }
}

/// Count the regular files of `dir` in one pass (links are neither followed
/// nor counted). A directory that cannot be listed is an error, never "empty".
fn scan_dir(dir: &StateDir) -> Result<DirState, ExportError> {
    let mut state = DirState {
        files: 0,
        charged: 0,
        next: 1,
    };
    // A folder with more entries than this is past the file cap anyway.
    let entries = dir
        .list_entries(DIR_FILES_MAX.saturating_mul(4))
        .map_err(|e| classify(&e))?;
    for e in entries {
        if e.kind != EntryKind::File {
            continue;
        }
        let Some(st) = dir.stat(&e.name).map_err(|e| classify(&e))? else {
            continue;
        };
        if st.kind != EntryKind::File {
            continue;
        }
        state.files += 1;
        state.charged += st.size.max(MIN_CHARGE);
        if let Some(n) = number_of(&e.name) {
            state.next = state.next.max(n.saturating_add(1));
        }
    }
    Ok(state)
}

fn has_room(state: &DirState, size: u64) -> bool {
    state.files < DIR_FILES_MAX && state.charged + size.max(MIN_CHARGE) <= DIR_CAP
}

/// Can `agent_id` still export a file of `size` bytes? True for an id that is
/// not a plain component, and for a folder that is a link or not ours (the
/// store refuses those later); false when the folder cannot be read.
pub fn has_room_for(root: &Path, agent_id: &str, size: u64) -> bool {
    if !agent_dir_ok(agent_id) {
        return true;
    }
    let Ok(canon_root) = canonicalize_plain(root) else {
        // No export folder yet: nothing is stored.
        return true;
    };
    match StateDir::open(&canon_root.join(agent_id)) {
        Ok(dir) => scan_dir(&dir).is_ok_and(|s| has_room(&s, size)),
        Err(e) if e.is_not_found() => true,
        Err(e) => classify(&e) == ExportError::Unsafe,
    }
}

/// The agent's directory, created owner-only if needed and pinned. The walk
/// from the root refuses a link in any component, so a link in place of the
/// agent folder is refused.
fn open_agent_dir(root: &Path, agent_id: &str) -> Result<StateDir, ExportError> {
    if !agent_dir_ok(agent_id) {
        return Err(ExportError::Unsafe);
    }
    ensure_private_dir(root).map_err(|e| classify(&e))?;
    let canon_root = canonicalize_plain(root).map_err(|_| ExportError::Io)?;
    StateDir::create(&canon_root.join(agent_id)).map_err(|e| classify(&e))
}

/// What was written: the path to hand to the caller, the byte count, and
/// whether the original extension was replaced by `.blocked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub path: PathBuf,
    pub size: u64,
    pub extension_blocked: bool,
}

/// Run `run` on its own thread and wait at most `wait` for the verdict. A
/// timeout, a panic or a thread that cannot start is `Incomplete`.
pub fn scan_within(
    wait: Duration,
    path: PathBuf,
    run: impl FnOnce(PathBuf) -> ScanVerdict + Send + 'static,
) -> ScanVerdict {
    if wait.is_zero() {
        return ScanVerdict::Incomplete;
    }
    let (tx, rx) = mpsc::channel();
    let started = std::thread::Builder::new().spawn(move || {
        let _ = tx.send(run(path));
    });
    if started.is_err() {
        return ScanVerdict::Incomplete;
    }
    rx.recv_timeout(wait).unwrap_or(ScanVerdict::Incomplete)
}

/// Scan with the host's file scanner, waiting at most `wait`. Where the scanner
/// cannot run on Windows the file counts as unscanned; other platforms host
/// no scanner and pass.
pub fn defender_within(path: &Path, wait: Duration) -> ScanVerdict {
    scan_within(wait, path.to_path_buf(), |p| match host().scan_file(&p) {
        FileScan::Clean => ScanVerdict::Clean,
        FileScan::Threat => ScanVerdict::Threat,
        FileScan::Unavailable if cfg!(windows) => ScanVerdict::Incomplete,
        FileScan::Unavailable => ScanVerdict::Clean,
    })
}

/// Write `bytes` as a new neutral-named file for `agent_id` under `root`,
/// then scan it with `scan`. Any failure leaves no file behind.
pub fn store_with(
    root: &Path,
    agent_id: &str,
    raw_name: &str,
    bytes: &[u8],
    scan: &dyn Fn(&Path) -> ScanVerdict,
) -> Result<Stored, ExportError> {
    let size = bytes.len() as u64;
    if size > FILE_CAP {
        return Err(ExportError::TooLarge);
    }
    let dir = open_agent_dir(root, agent_id)?;
    let state = scan_dir(&dir)?;
    if !has_room(&state, size) {
        return Err(ExportError::DirFull);
    }
    let (ext, extension_blocked) = stored_extension(raw_name);
    let mut n = state.next;
    for _ in 0..NAME_TRIES {
        let name = format!("attachment-{n}{ext}");
        match dir.write_new(&name, bytes) {
            Ok(()) => {
                // The file exists inside the pinned folder; a failed scan removes it.
                let discard = |e: ExportError| {
                    let _ = dir.remove(&name);
                    e
                };
                match scan(&dir.path().join(&name)) {
                    ScanVerdict::Clean => {}
                    ScanVerdict::Threat => return Err(discard(ExportError::Blocked)),
                    ScanVerdict::Incomplete => return Err(discard(ExportError::ScanIncomplete)),
                }
                return Ok(Stored {
                    path: root.join(agent_id).join(&name),
                    size,
                    extension_blocked,
                });
            }
            Err(FsError::AlreadyExists(_)) => n = n.saturating_add(1),
            Err(FsError::ReplacedButDirSyncFailed { .. }) => {
                // Published but not durable: do not keep a file nobody was told about.
                let _ = dir.remove(&name);
                return Err(ExportError::Io);
            }
            Err(_) => return Err(ExportError::Io),
        }
    }
    Err(ExportError::Io)
}

/// Delete the exported files of every agent under `root`. Returns how many
/// files were removed. Links are removed as links, never followed, and
/// sub-folders inside an agent folder are left alone. A file a draft is reading
/// right now is held open without delete sharing on Windows: that removal
/// fails, the file is not counted, and a later clear takes it.
pub fn clear(root: &Path) -> Result<usize, String> {
    let Ok(canon_root) = canonicalize_plain(root) else {
        return Ok(0);
    };
    let Ok(top) = StateDir::open(&canon_root) else {
        return Ok(0);
    };
    let Ok(agents) = top.list_entries(usize::MAX) else {
        return Ok(0);
    };
    let mut removed = 0;
    for agent in agents {
        if agent.kind != EntryKind::Dir {
            // A file, or a link where a folder belongs: drop it, not its target.
            if top.remove(&agent.name).unwrap_or(false) {
                removed += 1;
            }
            continue;
        }
        let Ok(dir) = top.child(&agent.name) else {
            continue;
        };
        let Ok(files) = dir.list_entries(usize::MAX) else {
            continue;
        };
        for f in files {
            if matches!(f.kind, EntryKind::File | EntryKind::Symlink)
                && dir.remove(&f.name).unwrap_or(false)
            {
                removed += 1;
            }
        }
        // The pinned handle must be closed before the folder can go (Windows).
        drop(dir);
        // An empty agent folder goes; one that still holds a file is left (the OS refuses).
        let _ = top.remove_dir(&agent.name);
    }
    Ok(removed)
}
