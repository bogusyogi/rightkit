//! Opening one exported file for a draft, once, with every check made on the
//! opened handle rather than on the path. A path that is checked and then
//! opened again can be swapped for a link in between; a handle cannot.
//!
//! The file is opened with `rightkit_fs::open_read_nofollow` (no final link or
//! reparse point followed, a regular file only, exactly one hard link, and on
//! Windows share-read-only so nobody can write to or delete it while it is
//! read). The handle's own final path (`rightkit_fs::final_path`) must then be
//! the expected path inside the canonical agent directory; a link, junction or
//! swapped folder anywhere in the path makes it differ.

use std::fs;
use std::path::Path;

use rightkit_fs::{FsError, ReadOptions};

/// Why a file was not opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFail {
    Missing,
    Unsafe,
    Io,
}

fn io_fail(e: &std::io::Error) -> OpenFail {
    if e.kind() == std::io::ErrorKind::NotFound {
        OpenFail::Missing
    } else {
        OpenFail::Io
    }
}

/// Paths are equal as text; Windows paths are case-insensitive.
fn same_path(a: &Path, b: &Path) -> bool {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    if cfg!(windows) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
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

/// Open `<canon_dir>/<name>` read-only, no link followed, and verify the handle.
/// `name` is already a validated bare name.
pub fn open_plain(canon_dir: &Path, name: &str) -> Result<fs::File, OpenFail> {
    let expected = canon_dir.join(name);
    // One hard link only, and share read only: while the bytes are read nobody
    // can write to or delete the file. A writer still holding it open makes
    // this open fail (`Io`).
    let options = ReadOptions::new()
        .require_single_link(true)
        .share_read_only(true);
    let file = rightkit_fs::open_read_nofollow(&expected, &options).map_err(|e| match e {
        FsError::Symlink(_) | FsError::NotAFile(_) | FsError::HardLinked(_) => OpenFail::Unsafe,
        FsError::Io { source, .. } => {
            if source.kind() == std::io::ErrorKind::NotFound {
                return OpenFail::Missing;
            }
            match fs::symlink_metadata(&expected) {
                // Windows refuses to open a folder this way: not a plain file.
                Ok(m) if m.file_type().is_symlink() || !m.is_file() => OpenFail::Unsafe,
                _ => io_fail(&source),
            }
        }
        _ => OpenFail::Io,
    })?;
    let meta = file.metadata().map_err(|_| OpenFail::Io)?;
    if !meta.is_file() || is_reparse(&meta) {
        return Err(OpenFail::Unsafe);
    }
    // The final path comes from the handle. None available: refuse.
    let actual = rightkit_fs::final_path(&file).map_err(|_| OpenFail::Unsafe)?;
    if !same_path(&actual, &expected) {
        return Err(OpenFail::Unsafe);
    }
    Ok(file)
}
