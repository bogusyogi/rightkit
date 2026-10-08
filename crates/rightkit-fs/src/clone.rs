//! Copy-on-write file clones that never fall back to a byte copy.
//!
//! On macOS this calls `clonefile(2)` with `CLONE_NOFOLLOW`. The kernel either creates a
//! clone on the same APFS volume or fails with a typed error; there is no `read`/`write`
//! fallback, so a caller that asked for a clone can never silently receive a copy.
//! Other platforms always return [`CloneError::Unsupported`].

use std::io;
use std::path::{Path, PathBuf};

/// Why a clone was not created. Each variant maps to a stable code for callers and the helper binary.
#[derive(Debug, thiserror::Error)]
pub enum CloneError {
    /// The source is a symbolic link. It is refused, never followed or cloned as a link.
    #[error("{0} is a symbolic link; clone refuses links")]
    Symlink(PathBuf),
    /// The source is not a regular file (directory, device, socket, ...).
    #[error("{0} is not a regular file")]
    NotAFile(PathBuf),
    /// Source and destination are on different volumes; clonefile needs one volume.
    #[error("{src} and {dst} are on different volumes")]
    CrossVolume { src: PathBuf, dst: PathBuf },
    /// The volume or filesystem cannot clone (not APFS, or the kernel reports ENOTSUP/ENOSYS).
    #[error("clonefile is not supported for {path}: {source}")]
    Unsupported {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The source or destination path cannot be passed to the kernel (embedded NUL).
    #[error("path contains a NUL byte: {0}")]
    InvalidPath(PathBuf),
    /// Any other failure (missing source, destination exists, permissions, ...).
    #[error("clone of {path} failed: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl CloneError {
    /// Stable machine code, shared with the `rightkit-clonefile` helper's JSON output.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Symlink(_) => "CLONE_SYMLINK_REFUSED",
            Self::NotAFile(_) => "CLONE_NOT_A_FILE",
            Self::CrossVolume { .. } => "CLONE_CROSS_VOLUME",
            Self::Unsupported { .. } => "CLONE_UNSUPPORTED",
            Self::InvalidPath(_) => "CLONE_INVALID_PATH",
            Self::Io { .. } => "CLONE_IO",
        }
    }

    /// Exit status used by the `rightkit-clonefile` helper for this failure.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Unsupported { .. } => 3,
            Self::CrossVolume { .. } => 4,
            Self::Symlink(_) => 5,
            Self::NotAFile(_) => 6,
            Self::InvalidPath(_) => 2,
            Self::Io { .. } => 1,
        }
    }

    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Clones regular file `src` to a new path `dst` without following a symlink at `src`.
///
/// `dst` must not exist. The clone is created atomically by the kernel; on any failure no
/// partial destination is left behind and no bytes are copied.
#[cfg(target_os = "macos")]
pub fn clone_file_nofollow(src: &Path, dst: &Path) -> Result<(), CloneError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    /// `CLONE_NOFOLLOW` from `<sys/clonefile.h>`: do not follow a symlink at the final `src` component.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    let meta = std::fs::symlink_metadata(src).map_err(|source| CloneError::io(src, source))?;
    if meta.file_type().is_symlink() {
        return Err(CloneError::Symlink(src.to_path_buf()));
    }
    if !meta.is_file() {
        return Err(CloneError::NotAFile(src.to_path_buf()));
    }
    let c_src = CString::new(src.as_os_str().as_bytes())
        .map_err(|_| CloneError::InvalidPath(src.to_path_buf()))?;
    let c_dst = CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| CloneError::InvalidPath(dst.to_path_buf()))?;
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the call; flags is a documented value.
    let rc = unsafe { libc::clonefile(c_src.as_ptr(), c_dst.as_ptr(), CLONE_NOFOLLOW) };
    if rc == 0 {
        return Ok(());
    }
    let source = io::Error::last_os_error();
    Err(match source.raw_os_error() {
        Some(libc::EXDEV) => CloneError::CrossVolume {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
        },
        Some(code) if code == libc::ENOTSUP || code == libc::ENOSYS => CloneError::Unsupported {
            path: dst.to_path_buf(),
            source,
        },
        _ => CloneError::io(dst, source),
    })
}

/// Non-macOS platforms have no `clonefile(2)`; callers must treat this as unsupported and fail closed.
#[cfg(not(target_os = "macos"))]
pub fn clone_file_nofollow(src: &Path, dst: &Path) -> Result<(), CloneError> {
    Err(CloneError::Unsupported {
        path: dst.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::Unsupported,
            format!("clonefile is macOS-only (source {})", src.display()),
        ),
    })
}
