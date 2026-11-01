use std::io::Write;
use std::path::Path;

use crate::render_qa::error::QaError;

/// Atomic replace via RightKit (`rightkit_fs::atomic`): creates missing parent directories,
/// temp file in the same directory, fsync, rename over the target.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), QaError> {
    let options = rightkit_fs::AtomicOptions {
        create_parents: true,
        ..rightkit_fs::AtomicOptions::default()
    };
    rightkit_fs::atomic::atomic_write_with(path, options, |f| f.write_all(bytes)).map_err(|e| {
        QaError::io(
            format!("writing {}", path.display()),
            std::io::Error::other(e),
        )
    })
}
