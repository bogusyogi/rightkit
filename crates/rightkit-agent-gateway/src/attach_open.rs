//! Opening the file an attach card names, with every check made on the opened
//! handle. A path that is checked and then opened again can be redirected in
//! between (a swapped parent folder, a junction, a link); a handle cannot.
//!
//! [`open_plain`] opens `path` with `rightkit_fs::open_read_nofollow` (no final
//! link or reparse point followed, a regular file only), reads the metadata
//! from that handle (no bytes), and returns the file's identity in
//! [`FileMeta::file_id`]: a digest of the volume and file index (`FileKey`:
//! Windows `GetFileInformationByHandle`; unix device and inode) and of the
//! handle's final path. It refuses (`NotPlain`) when the handle is not a
//! regular file, is a reparse point, or when its final path is not the
//! normalized path the card shows, which is what a link or junction anywhere
//! in the path produces.
//!
//! The final path is read from the open handle (`rightkit_fs::final_path`:
//! macOS `F_GETPATH`, Linux `/proc/self/fd/N`, Windows
//! `GetFinalPathNameByHandleW`), never from a path lookup after the open, so a
//! parent link flipped in the gap cannot change it. A platform with no handle
//! path refuses (`NotPlain`). The identity also comes from the handle and must
//! equal the one the card bound.

use super::attach_path::{FileError, FileMeta};
use rightkit_fs::digest::Sha256Hasher;
use rightkit_fs::{FileKey, FsError, ReadOptions};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

fn io_kind(e: &std::io::Error) -> FileError {
    if e.kind() == std::io::ErrorKind::NotFound {
        FileError::Missing
    } else {
        FileError::Io
    }
}

/// Why the no-follow open failed. A link, or anything that is not a regular
/// file (a folder is refused by the Windows open itself), is not a plain file.
fn open_error(path: &Path, e: FsError) -> FileError {
    match e {
        FsError::Symlink(_) | FsError::NotAFile(_) => FileError::NotPlain,
        FsError::Io { source, .. } => {
            if source.kind() == std::io::ErrorKind::NotFound {
                return FileError::Missing;
            }
            match fs::symlink_metadata(path) {
                Ok(m) if m.file_type().is_symlink() || !m.is_file() => FileError::NotPlain,
                _ => io_kind(&source),
            }
        }
        _ => FileError::Io,
    }
}

/// Size and modified time (milliseconds since the Unix epoch) of a handle.
fn size_and_time(m: &fs::Metadata) -> Result<(u64, i64), FileError> {
    let modified = m.modified().map_err(|_| FileError::Io)?;
    let ms = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok((m.len(), ms))
}

/// Digest of the file's identity: where it lives (volume + index) and the
/// final path it was opened at.
fn digest(a: u64, b: u64, final_path: &str) -> [u8; 32] {
    let mut h = Sha256Hasher::new();
    h.update(&a.to_be_bytes());
    h.update(&b.to_be_bytes());
    h.update(&(final_path.len() as u64).to_be_bytes());
    h.update(final_path.as_bytes());
    h.finalize_bytes()
}

/// The path text compared and hashed: Windows paths are case-insensitive and
/// accept `/`, unix paths are exact.
fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('/', "\\").to_lowercase()
    } else {
        s.into_owned()
    }
}

/// Any reparse point on the handle (not only a symlink or junction) is refused.
#[cfg(windows)]
fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse(_meta: &fs::Metadata) -> bool {
    false
}

/// The handle's own final path, read from the open handle, never from a path
/// lookup. `None` when the platform offers no such query (fail closed).
fn handle_path(file: &File) -> Option<PathBuf> {
    rightkit_fs::final_path(file).ok()
}

pub fn open_plain(path: &Path) -> Result<(File, FileMeta), FileError> {
    open_plain_with(path, &handle_path)
}

/// `open_plain` with the handle-path query injected, so a test can act between
/// the open and the query.
fn open_plain_with(
    path: &Path,
    handle_path: &dyn Fn(&File) -> Option<PathBuf>,
) -> Result<(File, FileMeta), FileError> {
    let file = rightkit_fs::open_read_nofollow(path, &ReadOptions::new())
        .map_err(|e| open_error(path, e))?;
    let meta = file.metadata().map_err(|_| FileError::Io)?;
    if !meta.is_file() || is_reparse(&meta) {
        return Err(FileError::NotPlain);
    }
    // The final path comes from the handle itself, so a parent link flipped
    // after the open cannot change it. No handle path: refuse.
    let actual = handle_path(&file).ok_or(FileError::NotPlain)?;
    let (actual, shown) = (path_key(&actual), path_key(path));
    if actual != shown {
        return Err(FileError::NotPlain);
    }
    let key = FileKey::of_file(&file, path).map_err(|_| FileError::Io)?;
    let (size, mtime_ms) = size_and_time(&meta)?;
    let file_id = digest(key.volume, key.index, &actual);
    Ok((
        file,
        FileMeta {
            size,
            mtime_ms,
            file_id,
        },
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("mr-attach-open-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        // Normalized (e.g. /var -> /private/var on macOS), as the card path is.
        fs::canonicalize(&p).unwrap()
    }

    #[test]
    fn plain_path_is_accepted() {
        let base = scratch("plain");
        let f = base.join("f.txt");
        fs::write(&f, b"x").unwrap();
        assert!(open_plain(&f).is_ok());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn path_check_uses_the_handle_not_a_later_lookup() {
        let base = scratch("race");
        fs::create_dir(base.join("real")).unwrap();
        fs::write(base.join("real").join("f.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let card = base.join("link").join("f.txt");

        // After the open (via the symlinked parent), restore the parent as a
        // real folder holding a file at the card path. A path lookup now
        // (canonicalize) would agree with the card; the handle still names real/.
        let r = open_plain_with(&card, &|file| {
            fs::remove_file(base.join("link")).unwrap();
            fs::create_dir(base.join("link")).unwrap();
            fs::write(base.join("link").join("f.txt"), b"y").unwrap();
            assert_eq!(fs::canonicalize(&card).unwrap(), card);
            handle_path(file)
        });
        assert!(matches!(r, Err(FileError::NotPlain)));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn no_handle_path_fails_closed() {
        let base = scratch("nofd");
        let f = base.join("f.txt");
        fs::write(&f, b"x").unwrap();
        assert!(matches!(
            open_plain_with(&f, &|_| None),
            Err(FileError::NotPlain)
        ));
        let _ = fs::remove_dir_all(&base);
    }
}
