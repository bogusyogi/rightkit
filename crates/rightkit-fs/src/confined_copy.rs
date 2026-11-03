//! A tree copy that refuses every link and stays inside two named roots.
//!
//! [`crate::tree::copy_tree`] keeps or follows links as its options say. This module is for a copy
//! where a planted link must never redirect anything: moving a library to a new root, copying a
//! folder a user or another program can write into.
//!
//! [`copy_tree_confined`] copies the file or directory at `src` to `dst` and guarantees:
//!
//! * `src` lies inside `confine.src_root` and `dst` inside `confine.dst_root` (lexically, no `..`
//!   in either), and every component from a root down to the path is a real directory: a symlink,
//!   junction or other reparse point there is [`FsError::Symlink`], an existing one or not.
//!   Each root itself may be reached through links in its own parent chain (the caller named it).
//! * Nothing in the source tree is a link: the tree is walked through pinned directory handles
//!   (`O_NOFOLLOW`, fd-relative on Unix, handle-relative on Windows, the [`crate::StateDir`]
//!   machinery), so a link swapped in during the walk is refused, never followed, and every read
//!   stays under `src`. A socket, FIFO or device is refused.
//! * `dst` must not exist: an existing destination entry of any kind is
//!   [`FsError::AlreadyExists`] and is never merged into or replaced.
//! * Nothing partial is left at `dst`. The tree is built in a hidden sibling staging directory of
//!   `dst` and published with one exclusive rename; on any failure the staging directory is
//!   removed. Missing parent directories of `dst` below `dst_root` are created (plain
//!   `create_dir`, one component at a time, after each existing component was checked).
//!
//! Residual: the staging directory and its files are made by path inside a fresh directory this call
//! created; a hostile process that can write to the parent of `dst` while the copy runs could still
//! interfere with the destination side. Use a destination parent only this user can write to. Source
//! directories must be owned by the current user (the pinned-directory rule), otherwise the copy is
//! refused.

use crate::error::{FsError, Result};
use crate::handle::ReadOptions;
use crate::state::{EntryKind, StateDir};
use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The two roots a confined copy must stay inside.
#[derive(Clone, Copy, Debug)]
pub struct ConfinedCopy<'a> {
    /// Every source path must be this folder or below it.
    pub src_root: &'a Path,
    /// The destination must be this folder or below it.
    pub dst_root: &'a Path,
}

impl<'a> ConfinedCopy<'a> {
    /// Name both roots.
    pub const fn new(src_root: &'a Path, dst_root: &'a Path) -> Self {
        Self { src_root, dst_root }
    }
}

/// Copy `src` to `dst` under the rules in the module docs. See [`ConfinedCopy`].
pub fn copy_tree_confined(src: &Path, dst: &Path, confine: &ConfinedCopy<'_>) -> Result<()> {
    check_below(confine.src_root, src)?;
    check_below(confine.dst_root, dst)?;
    // The source and the destination must not overlap: a copy of a folder into itself.
    let real_src_root = canonical(confine.src_root)?;
    let src_rel = src.strip_prefix(confine.src_root).unwrap_or(Path::new(""));
    let real_src = real_src_root.join(src_rel);
    let real_dst_root = canonical(confine.dst_root)?;
    let dst_rel = dst.strip_prefix(confine.dst_root).unwrap_or(Path::new(""));
    let real_dst = real_dst_root.join(dst_rel);
    if real_dst.starts_with(&real_src) {
        return Err(FsError::io(
            dst,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the destination is the source or inside it",
            ),
        ));
    }

    // Walk each side from its root, refusing a link at every existing component.
    let src_meta = refuse_links_below(&real_src_root, src_rel, false)?
        .ok_or_else(|| FsError::io(src, io::Error::from(io::ErrorKind::NotFound)))?;
    if refuse_links_below(&real_dst_root, dst_rel, true)?.is_some() {
        return Err(FsError::AlreadyExists(dst.to_path_buf()));
    }
    let dst_name = real_dst
        .file_name()
        .ok_or_else(|| FsError::BadName(dst.display().to_string()))?
        .to_os_string();
    let dst_parent = real_dst
        .parent()
        .ok_or_else(|| FsError::BadName(dst.display().to_string()))?
        .to_path_buf();
    create_missing_parents(&real_dst_root, &dst_parent)?;

    // Pin the source's parent and find the entry through it.
    let src_name = real_src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| FsError::BadName(src.display().to_string()))?
        .to_string();
    let src_parent = real_src
        .parent()
        .ok_or_else(|| FsError::BadName(src.display().to_string()))?;
    let pinned_parent = StateDir::open_shared(src_parent)?;
    let staging = staging_path(&dst_parent, &dst_name);
    // Set once this call created the staging entry, so a failure removes only what is ours.
    let created = Cell::new(false);
    let published = (|| -> Result<()> {
        if src_meta.is_dir() {
            fs::create_dir(&staging).map_err(|e| FsError::io(&staging, e))?;
            created.set(true);
            let source = pinned_parent.raw_open_child(&src_name)?;
            copy_dir_contents(&source, &staging)?;
            set_mode_from(&real_src, &staging)?;
        } else {
            copy_one_file(&pinned_parent, &src_name, &staging, &created)?;
        }
        crate::atomic::rename_exclusive(&staging, &real_dst)
    })();
    if published.is_err() && created.get() {
        remove_staging(&staging);
    }
    published
}

/// `path` is `root` or below it with no `..`; `root` and `path` are spelled the same way.
fn check_below(root: &Path, path: &Path) -> Result<()> {
    let outside = || {
        FsError::io(
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the path is not inside the root {}", root.display()),
            ),
        )
    };
    let rel = path.strip_prefix(root).map_err(|_| outside())?;
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(outside());
    }
    Ok(())
}

fn canonical(path: &Path) -> Result<PathBuf> {
    crate::plain::canonicalize_plain(path).map_err(|e| FsError::io(path, e))
}

/// A symlink, junction or any Windows reparse point.
fn is_link_like(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Walk `rel` from `root` one component at a time. A link at any component is
/// [`FsError::Symlink`]. Returns the last component's metadata, or `None` when a component is
/// missing (`missing_is_ok` false makes that an error only at the caller). A non-directory in the
/// middle of the path is [`FsError::NotADirectory`].
fn refuse_links_below(
    root: &Path,
    rel: &Path,
    missing_is_ok: bool,
) -> Result<Option<fs::Metadata>> {
    let mut current = root.to_path_buf();
    let mut last = None;
    let components: Vec<_> = rel
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if is_link_like(&meta) => return Err(FsError::Symlink(current)),
            Ok(meta) => {
                if index + 1 < components.len() && !meta.is_dir() {
                    return Err(FsError::NotADirectory(current));
                }
                last = Some(meta);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if missing_is_ok {
                    return Ok(None);
                }
                return Err(FsError::io(&current, e));
            }
            Err(e) => return Err(FsError::io(&current, e)),
        }
    }
    if components.is_empty() {
        // The root itself.
        return fs::symlink_metadata(root)
            .map(Some)
            .map_err(|e| FsError::io(root, e));
    }
    Ok(last)
}

/// Create the missing directories between `root` and `parent`, one at a time.
fn create_missing_parents(root: &Path, parent: &Path) -> Result<()> {
    let rel = parent.strip_prefix(root).unwrap_or(Path::new(""));
    let mut current = root.to_path_buf();
    for component in rel.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if is_link_like(&meta) => return Err(FsError::Symlink(current)),
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(FsError::NotADirectory(current)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&current).map_err(|e| FsError::io(&current, e))?;
                }
                Err(e) => return Err(FsError::io(&current, e)),
            }
        }
    }
    Ok(())
}

fn staging_path(parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name: String = name.to_string_lossy().chars().take(100).collect();
    parent.join(format!(
        ".{name}.rk-confine-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Copy every entry of the pinned `source` directory into the existing directory `target`.
fn copy_dir_contents(source: &StateDir, target: &Path) -> Result<()> {
    for entry in source.raw_entries()? {
        let name = entry.name.as_str();
        let dst = target.join(name);
        match entry.kind {
            EntryKind::Dir => {
                fs::create_dir(&dst).map_err(|e| FsError::io(&dst, e))?;
                let child = source.raw_open_child(name)?;
                copy_dir_contents(&child, &dst)?;
                // Permissions last, so a read-only directory does not stop its own contents.
                set_mode_from(&source.path().join(name), &dst)?;
            }
            EntryKind::File => copy_one_file(source, name, &dst, &Cell::new(false))?,
            EntryKind::Symlink => return Err(FsError::Symlink(source.path().join(name))),
            EntryKind::Other => return Err(FsError::NotAFile(source.path().join(name))),
        }
    }
    Ok(())
}

/// Copy one regular file read through the pinned `source` (never through a link) to the new name
/// `dst`, created exclusively with the source's permission bits. `created` is set once `dst`
/// exists.
fn copy_one_file(source: &StateDir, name: &str, dst: &Path, created: &Cell<bool>) -> Result<()> {
    let mut input = source.open_read(name, &ReadOptions::new())?;
    let meta = input
        .metadata()
        .map_err(|e| FsError::io(source.path().join(name), e))?;
    if !meta.is_file() {
        return Err(FsError::NotAFile(source.path().join(name)));
    }
    let mut open = OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        open.mode(meta.permissions().mode() & 0o7777);
    }
    let mut output: File = open.open(dst).map_err(|e| FsError::io(dst, e))?;
    created.set(true);
    io::copy(&mut input, &mut output).map_err(|e| FsError::io(dst, e))?;
    output
        .set_permissions(meta.permissions())
        .map_err(|e| FsError::io(dst, e))?;
    Ok(())
}

/// Give the directory `dst` the permissions of the directory at `src_path`.
fn set_mode_from(src_path: &Path, dst: &Path) -> Result<()> {
    let perms = fs::metadata(src_path)
        .map_err(|e| FsError::io(src_path, e))?
        .permissions();
    fs::set_permissions(dst, perms).map_err(|e| FsError::io(dst, e))
}

/// Remove the staging entry after a failed copy. It may hold read-only directories, so make the
/// directories writable first, never following a link.
fn remove_staging(staging: &Path) {
    fn make_writable(dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let path = entry.path();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o700));
                }
                make_writable(&path);
            }
        }
    }
    // A link is never chmod-ed through: only a real directory is made writable.
    if fs::symlink_metadata(staging).is_ok_and(|m| m.is_dir() && !is_link_like(&m)) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(staging, fs::Permissions::from_mode(0o700));
        }
        make_writable(staging);
    }
    let _ = crate::tree::remove_tree(staging, &crate::tree::RemoveOptions::new());
}
