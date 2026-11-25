//! Single-item actions from a storage browser: read one item's facts, preview an image, and move or
//! trash one item. Every mutating action checks the item again right before it acts: it must still
//! exist and still be the same item (device and inode) the person chose. A move never overwrites,
//! and across drives it copies first and trashes the original only after the copy is complete.
//! Moved out of Pulse's `files.rs`; the host passes what only it knows (which folders were scanned,
//! its home folder, how an item reaches the Trash) as arguments.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rightkit_fs::FsError;
use rightkit_fs::atomic::rename_exclusive;
use serde::Serialize;

/// How the host moves one item to the Trash (the host owns that effect).
pub type TrashFn<'a> = &'a dyn Fn(&Path) -> Result<(), String>;

#[derive(Serialize)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub is_dir: bool,
}

#[derive(Serialize)]
pub struct MovePlan {
    /// Where the item would land: the chosen folder plus its name.
    pub target: String,
    /// False when the destination is on another drive, so a move needs a copy.
    pub same_volume: bool,
}

#[derive(Serialize)]
pub struct MoveResult {
    pub target: String,
    /// True when the item was copied across drives and the original trashed.
    pub copied: bool,
}

/// What an inspector reads for one item, beyond what a scan holds. A folder's logical size is not
/// added up here, and its counts cover only its direct children. Anything that cannot be read is
/// null, never zero.
#[derive(Serialize)]
pub struct Inspect {
    pub is_dir: bool,
    /// A symbolic link: its target is never followed, so it has no size here.
    pub is_link: bool,
    /// The file's own length; null for folders, links and unreadable items.
    pub logical_bytes: Option<u64>,
    /// Unix seconds.
    pub created_at: Option<u64>,
    /// Unix seconds.
    pub modified_at: Option<u64>,
    /// Direct files of a folder (a link counts as a file).
    pub files: Option<u64>,
    /// Direct subfolders of a folder.
    pub folders: Option<u64>,
}

/// `path` as an absolute path, or the reason it is refused.
pub fn absolute(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("Paths must be absolute.".into());
    }
    Ok(path)
}

/// A link is judged as the link itself, never its target.
pub fn stat(path: &Path) -> Result<std::fs::Metadata, String> {
    std::fs::symlink_metadata(path).map_err(|_| "That item is no longer there.".to_string())
}

/// Most entries a folder's direct children are counted over, and the most time spent counting.
/// Past either, the counts are null.
pub const INSPECT_ENTRIES: usize = 20_000;
pub const INSPECT_TIME: Duration = Duration::from_secs(2);

fn unix_seconds(time: std::io::Result<SystemTime>) -> Option<u64> {
    time.ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// (files, folders) directly inside `path`, without following links. None when the folder cannot
/// be read, an entry cannot be read, or the limits are passed.
fn direct_children(path: &Path) -> Option<(u64, u64)> {
    let started = Instant::now();
    let mut files = 0u64;
    let mut folders = 0u64;
    for (seen, entry) in std::fs::read_dir(path).ok()?.enumerate() {
        if seen >= INSPECT_ENTRIES || started.elapsed() > INSPECT_TIME {
            return None;
        }
        // DirEntry::file_type does not follow a link.
        if entry.ok()?.file_type().ok()?.is_dir() {
            folders += 1;
        } else {
            files += 1;
        }
    }
    Some((files, folders))
}

/// No part of `path` below `root` is a link or a relative step, so nothing is read through a link.
pub fn links_free(root: &Path, path: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(root) else {
        return false;
    };
    let mut at = root.to_path_buf();
    for part in rest.components() {
        let Component::Normal(name) = part else {
            return false;
        };
        at.push(name);
        if std::fs::symlink_metadata(&at).map_or(true, |m| m.file_type().is_symlink()) {
            return false;
        }
    }
    true
}

/// Read one item's facts the scan does not hold: its logical size (files), its created and
/// modified times, and a folder's direct item counts. It stats this one path only, never recurses,
/// follows no link, and refuses any path outside the scanned folders (`root_of` names the scanned
/// folder holding a path). Read-only.
pub fn inspect_item(
    path: &str,
    root_of: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<Inspect, String> {
    let path = absolute(path)?;
    let root = root_of(&path).ok_or("That item is outside the scanned folders.")?;
    if !links_free(&root, &path) {
        return Err("That item is behind a link or outside the scan, so it is not read.".into());
    }
    let metadata = stat(&path)?;
    let is_dir = metadata.is_dir();
    let is_link = metadata.file_type().is_symlink();
    let counts = if is_dir && !is_link {
        direct_children(&path)
    } else {
        None
    };
    Ok(Inspect {
        is_dir,
        is_link,
        logical_bytes: (!is_dir && !is_link).then_some(metadata.len()),
        created_at: unix_seconds(metadata.created()),
        modified_at: unix_seconds(metadata.modified()),
        files: counts.map(|c| c.0),
        folders: counts.map(|c| c.1),
    })
}

/// The largest image read for a preview.
pub const PREVIEW_BYTES: u64 = 20 * 1024 * 1024;
/// Image extensions previewed, lower case. HEIC, BMP and TIFF are not.
pub const PREVIEW_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// The bytes of one image for a preview. Only a regular file (a link is refused), inside the
/// scanned folders (`root_of`), with a previewed extension and at most 20 MB. Anything else
/// returns the reason. Read-only.
pub fn preview_bytes(
    path: &str,
    root_of: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let path = absolute(path)?;
    let root = root_of(&path).ok_or("That item is outside the scanned folders.")?;
    if !links_free(&root, &path) {
        return Err("That item is behind a link or outside the scan, so it has no preview.".into());
    }
    let metadata = stat(&path)?;
    if !metadata.file_type().is_file() {
        return Err("Only image files have a preview.".into());
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !PREVIEW_EXTENSIONS.contains(&extension.as_str()) {
        return Err("This file type has no preview.".into());
    }
    if metadata.len() > PREVIEW_BYTES {
        return Err("This image is over 20 MB, so it has no preview.".into());
    }
    let file =
        std::fs::File::open(&path).map_err(|_| "That item is no longer there.".to_string())?;
    // One byte past the limit is read, so a file that grew since the check is refused, not read whole.
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(PREVIEW_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Could not read the image: {e}"))?;
    if bytes.len() as u64 > PREVIEW_BYTES {
        return Err("This image is over 20 MB, so it has no preview.".into());
    }
    Ok(bytes)
}

/// The item's (device, inode) pair. On Windows these are stable hashes of the volume serial and
/// the NTFS file id, kept under 2^53 so they survive the trip through a page's numbers.
#[cfg(unix)]
pub fn ids(_path: &Path, metadata: &std::fs::Metadata) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
pub fn ids(path: &Path, metadata: &std::fs::Metadata) -> Result<(u64, u64), String> {
    fn fold(text: &str) -> u64 {
        // FNV-1a, masked to 53 bits.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash & ((1 << 53) - 1)
    }
    let info = crate::platform::inspect(path, metadata);
    let id = info
        .file_id
        .ok_or_else(|| "This drive does not give files a stable identity.".to_string())?;
    Ok((fold(&id.volume.id), fold(&id.id)))
}

#[cfg(not(any(unix, windows)))]
pub fn ids(_path: &Path, _metadata: &std::fs::Metadata) -> Result<(u64, u64), String> {
    Err("This system does not give files a stable identity.".to_string())
}

/// Whether two items are on the same drive.
#[cfg(unix)]
fn same_drive(
    _a: (&Path, &std::fs::Metadata),
    _b: (&Path, &std::fs::Metadata),
    dev_a: u64,
    dev_b: u64,
) -> bool {
    dev_a == dev_b
}

#[cfg(windows)]
fn same_drive(
    a: (&Path, &std::fs::Metadata),
    b: (&Path, &std::fs::Metadata),
    _dev_a: u64,
    _dev_b: u64,
) -> bool {
    let first = crate::platform::inspect(a.0, a.1);
    let second = crate::platform::inspect(b.0, b.1);
    first.volume == second.volume
}

#[cfg(not(any(unix, windows)))]
fn same_drive(
    _a: (&Path, &std::fs::Metadata),
    _b: (&Path, &std::fs::Metadata),
    dev_a: u64,
    dev_b: u64,
) -> bool {
    dev_a == dev_b
}

/// The item at `path` is still the one the person chose: same device and inode.
pub fn same_item(path: &Path, dev: u64, ino: u64) -> Result<std::fs::Metadata, String> {
    let metadata = stat(path)?;
    if ids(path, &metadata)? != (dev, ino) {
        return Err("That item changed since you chose it. Nothing was changed.".into());
    }
    Ok(metadata)
}

/// The item's device and inode, read when the person asks for an action on it.
pub fn identity_of(path: &str) -> Result<Identity, String> {
    let path = absolute(path)?;
    let metadata = stat(&path)?;
    let (dev, ino) = ids(&path, &metadata)?;
    Ok(Identity {
        dev,
        ino,
        is_dir: metadata.is_dir(),
    })
}

/// What a move would do, checked now: the item is still the same, the destination is a folder,
/// the name is free there, and a folder is not moved into itself. Changes nothing.
fn plan_move(
    path: &Path,
    dev: u64,
    ino: u64,
    destination: &Path,
) -> Result<(PathBuf, bool), String> {
    let metadata = same_item(path, dev, ino)?;
    let folder =
        std::fs::metadata(destination).map_err(|_| "That folder is not there.".to_string())?;
    if !folder.is_dir() {
        return Err("Choose a folder to move into.".into());
    }
    if path.parent() == Some(destination) {
        return Err("It is already in that folder.".into());
    }
    if metadata.is_dir() && destination.starts_with(path) {
        return Err("A folder cannot be moved into itself.".into());
    }
    let name = path.file_name().ok_or("That item has no name.")?;
    let target = destination.join(name);
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(format!(
            "{} already exists there. Nothing was moved.",
            name.to_string_lossy()
        ));
    }
    let (folder_dev, _) = ids(destination, &folder).unwrap_or((0, 0));
    let (item_dev, _) = ids(path, &metadata)?;
    Ok((
        target,
        same_drive(
            (destination, &folder),
            (path, &metadata),
            folder_dev,
            item_dev,
        ),
    ))
}

/// Where a move would land and whether it stays on one drive. Changes nothing.
pub fn move_plan(path: &str, dev: u64, ino: u64, destination: &str) -> Result<MovePlan, String> {
    let path = absolute(path)?;
    let destination = absolute(destination)?;
    let (target, same_volume) = plan_move(&path, dev, ino, &destination)?;
    Ok(MovePlan {
        target: target.to_string_lossy().into_owned(),
        same_volume,
    })
}

/// Move an item into another folder. Within one drive it is a rename, which refuses to replace an
/// existing name. Across drives it needs `copy_across_volumes`: the copy is made first and the
/// original goes to the Trash (`trash`) only after that.
pub fn move_item(
    path: &str,
    dev: u64,
    ino: u64,
    destination: &str,
    copy_across_volumes: bool,
    trash: TrashFn<'_>,
) -> Result<MoveResult, String> {
    let path = absolute(path)?;
    let destination = absolute(destination)?;
    let (target, same_volume) = plan_move(&path, dev, ino, &destination)?;
    if same_volume {
        rename_no_replace(&path, &target)?;
        return Ok(MoveResult {
            target: target.to_string_lossy().into_owned(),
            copied: false,
        });
    }
    if !copy_across_volumes {
        return Err("That folder is on another drive. Confirm to copy it there and move the original to the Trash.".into());
    }
    copy_then_trash(&path, dev, ino, &target, trash)
}

/// rename(2) that never replaces an existing name: the kernel's exclusive rename where there is one.
fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
    match rename_exclusive(from, to) {
        Ok(()) => Ok(()),
        Err(FsError::AlreadyExists(_)) => {
            Err("That name already exists there. Nothing was moved.".into())
        }
        Err(FsError::Io { source, .. }) if source.raw_os_error() == Some(18) => {
            Err("That folder is on another drive. Confirm to copy it there instead.".into())
        }
        Err(error) => Err(format!("Could not move it: {error}")),
    }
}

/// Copy the item to `target` (which must not exist), then move the original to the Trash. A failed
/// copy is taken back out to the Trash and the original is left as it was. If the original cannot
/// be trashed, both copies stay and the message says where the copy is.
fn copy_then_trash(
    path: &Path,
    dev: u64,
    ino: u64,
    target: &Path,
    trash: TrashFn<'_>,
) -> Result<MoveResult, String> {
    // RightKit's tree copy: nothing is overwritten, links are recreated as links, special files
    // are refused rather than read.
    if let Err(error) =
        rightkit_fs::copy_tree(path, target, rightkit_fs::CopyOptions::new(true, true))
    {
        if std::fs::symlink_metadata(target).is_ok() {
            let _ = trash(target);
        }
        return Err(format!("Copy failed; the original is unchanged: {error}"));
    }
    if let Err(reason) = same_item(path, dev, ino) {
        return Err(format!(
            "{reason} The copy is at {}; the original was left in place.",
            target.display()
        ));
    }
    trash(path).map_err(|reason| {
        format!(
            "Copied to {}, but the original could not go to the Trash: {reason}",
            target.display()
        )
    })?;
    Ok(MoveResult {
        target: target.to_string_lossy().into_owned(),
        copied: true,
    })
}

/// Move one item to the Trash (`trash`) after checking it is still the one chosen. The host's home
/// folder (`home`) and a filesystem root are never trashed from here.
pub fn trash_item(
    path: &str,
    dev: u64,
    ino: u64,
    home: &Path,
    trash: TrashFn<'_>,
) -> Result<(), String> {
    let path = absolute(path)?;
    if path.parent().is_none() || path == home {
        return Err(
            "The home folder and the startup disk cannot be moved to the Trash from here.".into(),
        );
    }
    same_item(&path, dev, ino)?;
    trash(&path)
}
