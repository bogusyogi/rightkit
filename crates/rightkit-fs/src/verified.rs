//! Verified copy and publish (M8-O9), behind the `digest` feature.
//!
//! [`copy_verified`] copies a regular file to a temp file beside the destination, hashes the
//! bytes it read, optionally compares them with an expected SHA-256, re-reads the written temp
//! file and compares again, and only then publishes it atomically. On any failure the destination
//! is untouched and the partial temp file is removed. This is the copy-hash-verify-publish
//! primitive that import and ingest code otherwise reimplements.
//!
//! The source is read once through one descriptor and is refused if it is a symlink or not a
//! regular file, or if its length changed while it was copied. It is not locked: a writer that
//! keeps the length but changes bytes mid-copy is caught only when an expected digest was given.

use crate::atomic::{AtomicWriteOptions, AtomicWriter};
use crate::digest::Sha256Hasher;
use crate::error::{FsError, Result};
use std::cell::RefCell;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// What [`copy_verified`] published.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifiedCopy {
    /// Lowercase hex SHA-256 of the copied bytes (source and published copy agree).
    pub sha256: String,
    /// Number of bytes copied.
    pub bytes: u64,
}

/// Copy `src` to `dest` atomically and verify it. `expected_sha256` (hex, any case) is compared
/// with the source's digest when given: a mismatch is [`FsError::DigestMismatch`] and nothing is
/// published. `options` is the usual atomic-write policy (`private`, `durable`, replace mode).
pub fn copy_verified(
    src: &Path,
    dest: &Path,
    options: AtomicWriteOptions,
    expected_sha256: Option<&str>,
) -> Result<VerifiedCopy> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| FsError::io(src, e))?;
    if meta.file_type().is_symlink() {
        return Err(FsError::Symlink(src.to_path_buf()));
    }
    if !meta.is_file() {
        return Err(FsError::NotAFile(src.to_path_buf()));
    }
    let mut input = File::open(src).map_err(|e| FsError::io(src, e))?;
    let mut writer = AtomicWriter::create_with(dest, options)?;
    let mut hasher = Sha256Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = input.read(&mut buffer).map_err(|e| FsError::io(src, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        writer.write_all(&buffer[..n])?;
        total += n as u64;
    }
    let sha256 = hasher.finalize();
    let length_now = input.metadata().map_err(|e| FsError::io(src, e))?.len();
    if length_now != total {
        return Err(FsError::Corrupt {
            path: src.to_path_buf(),
            reason: format!(
                "source changed length while it was copied ({total} read, {length_now} now)"
            ),
        });
    }
    if let Some(expected) = expected_sha256 {
        if !expected.eq_ignore_ascii_case(&sha256) {
            return Err(FsError::DigestMismatch {
                path: src.to_path_buf(),
                expected: expected.to_ascii_lowercase(),
                actual: sha256,
            });
        }
    }
    // The check runs on the synced temp file, so it verifies what was written, not what was read.
    let written = RefCell::new(None::<String>);
    let result = writer.commit_checked(|temp| {
        let (digest, len) = crate::digest::sha256_file_with_len(temp)?;
        if digest != sha256 || len != total {
            *written.borrow_mut() = Some(digest);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the written copy does not match the source",
            ));
        }
        Ok(())
    });
    match result {
        Ok(()) => Ok(VerifiedCopy {
            sha256,
            bytes: total,
        }),
        Err(error) => match written.into_inner() {
            Some(actual) => Err(FsError::DigestMismatch {
                path: dest.to_path_buf(),
                expected: sha256,
                actual,
            }),
            None => Err(error),
        },
    }
}
