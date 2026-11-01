//! Atomic whole-file replace shared by the JSON store and the recovery marker.

use std::io::{self, Write};
use std::path::Path;

/// Replace `path` with `bytes` atomically (sibling temp file, fsync, rename, directory fsync),
/// creating missing parent directories. A directory-fsync failure after the rename is an error.
pub(super) fn replace_file_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let options = rightkit_fs::atomic::AtomicWriteOptions {
        create_parents: true,
        ..Default::default()
    };
    rightkit_fs::atomic::atomic_write_with_options(path, options, |file| file.write_all(bytes))
        .map_err(|error| match error {
            rightkit_fs::FsError::Io { source, .. }
            | rightkit_fs::FsError::ReplacedButDirSyncFailed { source, .. } => source,
            other => io::Error::other(other.to_string()),
        })
}
