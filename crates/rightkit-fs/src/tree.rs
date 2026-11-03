//! Recursive directory copy that refuses to overwrite by default and keeps symlinks as symlinks.
//!
//! [`copy_tree`] copies a file, a symlink or a directory tree to a destination that does not exist
//! yet. It is not atomic as a whole: a failure part-way leaves the entries already copied in place and
//! returns the error, so a caller that needs all-or-nothing copies into a fresh staging directory and
//! publishes that with [`crate::rename_exclusive`] or [`crate::swap_paths`].
//!
//! Each file is written to an exclusive (`O_EXCL`) sibling temporary file and then renamed onto its
//! destination, so a destination that is a symlink or a hard link is replaced as an entry and never
//! written through: its target (or the source, when the destination is a hard link to it) is not
//! truncated. A copy whose destination resolves to the source or below it is refused before anything
//! is created.

use crate::error::{FsError, Result};
use crate::file_key::FileKey;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How [`copy_tree`] treats existing destinations and symlinks. Both fields are explicit: there is
/// no default, because either choice can lose or silently keep data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyOptions {
    /// When true, an entry that already exists at its destination stops the copy with
    /// [`FsError::AlreadyExists`]; nothing existing is ever written over. When false, an existing
    /// destination file or symlink is replaced, and an existing destination directory is merged
    /// into.
    pub no_overwrite: bool,
    /// When true, a symlink is recreated as a symlink with the same target text, whatever it points
    /// at. When false, a symlink to a file is copied as that file's bytes, and a symlink to a
    /// directory is refused (it is not followed, so a link cycle cannot make the copy run forever).
    pub keep_symlinks: bool,
}

impl CopyOptions {
    /// Options with both choices named, for a call site that reads clearly.
    pub const fn new(no_overwrite: bool, keep_symlinks: bool) -> Self {
        Self {
            no_overwrite,
            keep_symlinks,
        }
    }
}

/// Copy the file, symlink or directory tree at `src` to `dst`.
///
/// Directories are created before their contents; a file keeps its permission bits; a symlink is
/// handled as [`CopyOptions::keep_symlinks`] says. Entries are copied in directory order, not
/// sorted. `dst` must not be the source or inside it: both are resolved through every existing
/// ancestor (relative or absolute spelling, symlinked parents, case) before anything is created, and a
/// path that cannot be resolved is refused. `src` must exist. Errors carry the path they concern.
pub fn copy_tree(src: &Path, dst: &Path, options: CopyOptions) -> Result<()> {
    let inside = || {
        FsError::io(
            dst,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the destination is inside the source tree",
            ),
        )
    };
    // Lexical first (cheap, and it holds even if resolution later fails).
    if dst.starts_with(src) {
        return Err(inside());
    }
    let src_meta = fs::symlink_metadata(src).map_err(|e| FsError::io(src, e))?;
    if src_meta.is_dir() {
        let absolute = |p: &Path| std::path::absolute(p).map_err(|e| FsError::io(p, e));
        let (real_src, abs_dst) = (absolute(src)?, absolute(dst)?);
        match crate::contain::resolved_within(&real_src, &abs_dst) {
            Ok(false) => {}
            Ok(true) => return Err(inside()),
            Err(e) => return Err(FsError::io(dst, io::Error::from(e))),
        }
    }
    copy_entry(src, dst, options)
}

fn copy_entry(src: &Path, dst: &Path, options: CopyOptions) -> Result<()> {
    let meta = fs::symlink_metadata(src).map_err(|e| FsError::io(src, e))?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        if options.keep_symlinks {
            return copy_link(src, dst, options.no_overwrite);
        }
        let followed = fs::metadata(src).map_err(|e| FsError::io(src, e))?;
        if followed.is_dir() {
            return Err(FsError::io(
                src,
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "a symlink to a directory is not followed by copy_tree",
                ),
            ));
        }
        return copy_file(src, dst, options.no_overwrite, &followed);
    }
    if kind.is_dir() {
        return copy_dir(src, dst, options);
    }
    if kind.is_file() {
        return copy_file(src, dst, options.no_overwrite, &meta);
    }
    Err(FsError::NotAFile(src.to_path_buf()))
}

fn copy_dir(src: &Path, dst: &Path, options: CopyOptions) -> Result<()> {
    let created = match fs::create_dir(dst) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if options.no_overwrite {
                return Err(FsError::AlreadyExists(dst.to_path_buf()));
            }
            let existing = fs::symlink_metadata(dst).map_err(|e| FsError::io(dst, e))?;
            if !existing.is_dir() {
                return Err(FsError::io(
                    dst,
                    io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "a non-directory exists where the directory should go",
                    ),
                ));
            }
            false
        }
        Err(e) => return Err(FsError::io(dst, e)),
    };
    let entries = fs::read_dir(src).map_err(|e| FsError::io(src, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| FsError::io(src, e))?;
        copy_entry(&entry.path(), &dst.join(entry.file_name()), options)?;
    }
    // Permissions last: a read-only source directory must not stop its own contents being written.
    // Only a directory this call created takes the source's permissions.
    if created {
        let perms = fs::metadata(src)
            .map_err(|e| FsError::io(src, e))?
            .permissions();
        fs::set_permissions(dst, perms).map_err(|e| FsError::io(dst, e))?;
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path, no_overwrite: bool, meta: &fs::Metadata) -> Result<()> {
    let mut input = File::open(src).map_err(|e| FsError::io(src, e))?;
    // A destination that is the very file being read (a hard link to it) is refused, not replaced
    // by a lookalike. A symlink at `dst` is not an alias of its own: it is replaced as an entry.
    if let Ok(existing) = fs::symlink_metadata(dst) {
        if no_overwrite {
            return Err(FsError::AlreadyExists(dst.to_path_buf()));
        }
        if !existing.file_type().is_symlink() {
            if let (Ok(a), Ok(b)) = (FileKey::of_file(&input, src), FileKey::of_path(dst)) {
                if a == b {
                    return Err(FsError::io(
                        dst,
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "the destination is the source file",
                        ),
                    ));
                }
            }
        }
    }
    let (temp, mut output) = create_sibling_temp(dst, meta)?;
    let result = (|| -> Result<()> {
        io::copy(&mut input, &mut output).map_err(|e| FsError::io(&temp, e))?;
        output
            .set_permissions(meta.permissions())
            .map_err(|e| FsError::io(&temp, e))?;
        drop(output);
        if no_overwrite {
            crate::atomic::rename_exclusive(&temp, dst)
        } else {
            fs::rename(&temp, dst).map_err(|e| FsError::io(dst, e))
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A fresh name next to `dst` (same directory, so a rename onto `dst` stays on one volume).
fn sibling_temp_name(dst: &Path, tag: &str) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = match dst.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = dst
        .file_name()
        .ok_or_else(|| FsError::BadName(dst.display().to_string()))?
        .to_string_lossy()
        .chars()
        .take(100)
        .collect::<String>();
    Ok(dir.join(format!(
        ".{name}.rk-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )))
}

/// Create a fresh file next to `dst` (same directory, so a rename onto `dst` stays on one volume)
/// with `create_new`, which never follows a link and never reuses a name. On Unix it is created with
/// the source's permission bits so it is never wider than the file it copies.
fn create_sibling_temp(dst: &Path, meta: &fs::Metadata) -> Result<(PathBuf, File)> {
    let mut last = None;
    for _ in 0..16 {
        let temp = sibling_temp_name(dst, "copy")?;
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            open.mode(meta.permissions().mode() & 0o7777);
        }
        #[cfg(not(unix))]
        let _ = meta;
        match open.open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(FsError::io(&temp, e)),
        }
    }
    Err(FsError::io(
        dst,
        last.unwrap_or_else(|| io::Error::other("no free temporary name")),
    ))
}

fn copy_link(src: &Path, dst: &Path, no_overwrite: bool) -> Result<()> {
    let target = fs::read_link(src).map_err(|e| FsError::io(src, e))?;
    // Whether the link points at a directory decides the Windows link kind; a dangling link is a
    // file link.
    let target_is_dir = fs::metadata(src).map(|m| m.is_dir()).unwrap_or(false);
    if no_overwrite {
        // Creating a link never follows or replaces what is at `dst`.
        return create_link(&target, dst, target_is_dir).map_err(|e| map_exists(dst, e, true));
    }
    if let Ok(existing) = fs::symlink_metadata(dst) {
        if existing.is_dir() && !existing.file_type().is_symlink() {
            return Err(FsError::io(
                dst,
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a directory exists where the link should go",
                ),
            ));
        }
    }
    // Build the link under an exclusive temporary name, then rename it over `dst` (replacing the
    // entry, never what an existing link at `dst` points to).
    let mut attempts = 0;
    let temp = loop {
        let temp = sibling_temp_name(dst, "link")?;
        match create_link(&target, &temp, target_is_dir) {
            Ok(()) => break temp,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 16 => attempts += 1,
            Err(e) => return Err(FsError::io(&temp, e)),
        }
    };
    if let Err(e) = fs::rename(&temp, dst) {
        let _ = fs::remove_file(&temp);
        return Err(FsError::io(dst, e));
    }
    Ok(())
}

#[cfg(unix)]
fn create_link(target: &Path, dst: &Path, _target_is_dir: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, dst)
}

#[cfg(windows)]
fn create_link(target: &Path, dst: &Path, target_is_dir: bool) -> io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, dst)
    } else {
        std::os::windows::fs::symlink_file(target, dst)
    }
}

#[cfg(not(any(unix, windows)))]
fn create_link(_target: &Path, dst: &Path, _target_is_dir: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("symlinks are not supported for {}", dst.display()),
    ))
}

/// An `AlreadyExists` from a create at `dst` becomes [`FsError::AlreadyExists`] when the caller
/// asked for no overwrite; every other error keeps its path and kind.
fn map_exists(dst: &Path, error: io::Error, no_overwrite: bool) -> FsError {
    if no_overwrite && error.kind() == io::ErrorKind::AlreadyExists {
        FsError::AlreadyExists(dst.to_path_buf())
    } else {
        FsError::io(dst, error)
    }
}
