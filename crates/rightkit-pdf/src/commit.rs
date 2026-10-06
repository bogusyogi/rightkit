//! Atomic on-disk commit for a staged save token.
//!
//! [`save::resolve_commit`] is filesystem-free; this module supplies the real source re-read and a
//! crash-safe replace (write temp sibling, fsync, rename). Path authorization is the caller's job.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::model::PdfEditError;
use crate::save::{resolve_commit, PdfCommitResult, PdfSaveTokenStore};

#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error(transparent)]
    Edit(#[from] PdfEditError),
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Writes `bytes` to `target` atomically: a temp file in the same directory is fsynced and then
/// renamed over the target, so readers observe either the old or the new file, never a mix.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut nonce = [0_u8; 6];
    getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let tmp: PathBuf = dir.join(format!(".{name}.{suffix}.tmp"));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Consumes `token`, re-checks the on-disk source for same-path saves, and atomically writes the
/// verified candidate to `target` (or back to the source path when `None`).
pub fn commit_pdf_save(
    store: &PdfSaveTokenStore,
    token: &str,
    target: Option<&Path>,
) -> Result<PdfCommitResult, CommitError> {
    let (_source, bytes, result) = resolve_commit(
        store,
        token,
        target.map(|p| p.to_string_lossy().into_owned()),
        |source| fs::read(source).map_err(|_| PdfEditError::WriteConflict),
    )?;
    write_atomic(Path::new(&result.target_path), &bytes)?;
    Ok(result)
}
