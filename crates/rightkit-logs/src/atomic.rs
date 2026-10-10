//! Crash-safe file replacement shared by consent and the outbox, backed by `rightkit-fs`.

use std::io::{self, Write};
use std::path::Path;

use rightkit_fs::{atomic_write_with, AtomicOptions, FsError};

/// Write `bytes` to a sibling temp file, fsync, then rename over `path`, creating missing
/// parent directories. Readers see either the old or the new content, never a partial file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let options = AtomicOptions {
        create_parents: true,
        ..AtomicOptions::default()
    };
    atomic_write_with(path, options, |file| file.write_all(bytes)).map_err(into_io)
}

fn into_io(error: FsError) -> io::Error {
    match error {
        FsError::Io { source, .. } => source,
        other => io::Error::other(other.to_string()),
    }
}
