//! Document read/write/copy, sibling-image listing and file watching, with the grant checks the
//! commands apply. Every function is synchronous; the host runs them off its IPC thread.

use crate::documents::{DocumentBackend, FileFingerprint, RawDocument, WriteDocumentResult};
use crate::error::{broker_error, WorkspaceError};
use crate::grants::{authorize_existing_for_read, PathGrants};
use crate::store::Db;
use crate::watch::WorkspaceWatcher;

/// Read a granted document, attach a stream token for binary formats and restore the stored view
/// state.
pub fn read_document(
    db: &Db,
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
) -> Result<RawDocument, WorkspaceError> {
    let path = authorize_existing_for_read(path, grants)?;
    let format = docs.format_of(&path);
    let mut doc = docs.read(&path).map_err(|e| match format {
        "unknown" => WorkspaceError::UnsupportedFormat(e),
        _ if e.contains("too large") => WorkspaceError::DocumentTooLarge(e),
        _ => WorkspaceError::Io(e),
    })?;
    if matches!(format, "pdf" | "docx" | "image") {
        doc.stream_token = Some(grants.grant_stream_token(&path)?);
    }
    let st = db.view_state(&path)?;
    doc.last_view = st.last_view;
    doc.view_meta = st.view_meta;
    Ok(doc)
}

pub fn write_document(
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
    content: &str,
    encoding: Option<String>,
    expected_source_sha256: Option<&str>,
) -> Result<WriteDocumentResult, WorkspaceError> {
    let path = grants.require_write_target(path).map_err(broker_error)?;
    let encoding = encoding.unwrap_or_else(|| "UTF-8".to_string());
    let result = docs.write(&path, content, &encoding, expected_source_sha256);
    if result.is_ok() {
        let _ = grants.record_write_target(std::path::Path::new(&path));
    }
    result.map_err(|e| {
        if e.starts_with("write conflict:") {
            WorkspaceError::WriteConflict(e)
        } else if e.to_ascii_lowercase().contains("encod") {
            WorkspaceError::Encoding(e)
        } else {
            WorkspaceError::Io(e)
        }
    })
}

/// Decoded write request payload: the document bytes (raw) or base64 text (legacy).
#[derive(Debug, PartialEq)]
pub enum WritePayload {
    Raw(Vec<u8>),
    Base64(String),
}

/// Decode the hex-encoded UTF-8 target path a raw-body save carries in a request header. Hex keeps
/// the header ASCII-safe for any filesystem path.
pub fn decode_hex_path(hex: &str) -> Result<String, String> {
    rightkit_fs::digest::unhex(hex)
        .filter(|bytes| !bytes.is_empty())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| "invalid write path header".to_string())
}

pub fn write_document_payload(
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
    payload: WritePayload,
) -> Result<(), WorkspaceError> {
    let path = grants.require_write_target(path).map_err(broker_error)?;
    let result = match payload {
        WritePayload::Raw(bytes) => docs.write_raw_bytes(&path, &bytes),
        WritePayload::Base64(b64) => docs.write_bytes(&path, &b64),
    };
    if result.is_ok() {
        let _ = grants.record_write_target(std::path::Path::new(&path));
    }
    result.map_err(WorkspaceError::Io)
}

pub fn copy_document(
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    source_path: &str,
    target_path: &str,
) -> Result<(), WorkspaceError> {
    let source = authorize_existing_for_read(source_path, grants).map_err(broker_error)?;
    let target = grants
        .require_write_target(target_path)
        .map_err(broker_error)?;
    docs.copy_document(&source, &target)
        .map_err(WorkspaceError::Io)?;
    grants
        .record_write_target(std::path::Path::new(&target))
        .map_err(broker_error)?;
    Ok(())
}

pub fn revoke_path(grants: &dyn PathGrants, path: &str) -> Result<(), WorkspaceError> {
    grants.revoke_path(path).map_err(broker_error)
}

/// List sibling image files in the same directory as an already-opened image, naturally sorted
/// (so `img2` precedes `img10`), and grant each so the viewer can page through the folder in
/// place. The input must already be authorized, which scopes this to a folder the user chose, and
/// the filter is image-only so this never authorizes arbitrary file types. The returned list
/// includes the current image.
pub fn list_sibling_images(
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
) -> Result<Vec<String>, WorkspaceError> {
    let current = authorize_existing_for_read(path, grants).map_err(broker_error)?;
    let dir = std::path::Path::new(&current)
        .parent()
        .ok_or_else(|| WorkspaceError::FileNotFound("image has no parent directory".into()))?
        .to_path_buf();
    // Canonical containment root: a sibling is only granted if its REAL path stays inside this
    // folder. read_dir + canonicalize follow symlinks, so without this a symlink in the folder
    // pointing at an image elsewhere on disk would get a grant.
    let dir_canon = std::fs::canonicalize(&dir)?;
    let mut images: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && docs.format_of(&p.to_string_lossy()) == "image")
        .filter_map(|p| std::fs::canonicalize(&p).ok()) // resolve symlinks to their real target
        .filter(|c| c.starts_with(&dir_canon)) // drop anything whose real path escapes the folder
        .collect();
    images.sort_by(|a, b| natural_cmp(&file_name_lossy(a), &file_name_lossy(b)));
    images.dedup(); // a symlink + its target both resolve here: grant the file once
    Ok(grants.grant_existing_files(&images))
}

fn file_name_lossy(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Case-insensitive natural ordering: letter runs compare lexically (lowercased), digit runs compare
/// by numeric value so `2` sorts before `10`. Ties on value fall back to raw length then bytes for a
/// stable, deterministic order (e.g. `img02` vs `img2`). Shared with `explorer` for its tree sort.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let an = take_digits(&mut ai);
                    let bn = take_digits(&mut bi);
                    let at = an.trim_start_matches('0');
                    let bt = bn.trim_start_matches('0');
                    let ord = at
                        .len()
                        .cmp(&bt.len())
                        .then_with(|| at.cmp(bt))
                        .then_with(|| an.len().cmp(&bn.len()));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    let ord = ac.to_ascii_lowercase().cmp(&bc.to_ascii_lowercase());
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

pub fn file_fingerprint(
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
) -> Result<FileFingerprint, WorkspaceError> {
    let path = grants.require_existing_file(path).map_err(broker_error)?;
    docs.fingerprint(&path).map_err(WorkspaceError::Io)
}

pub fn watch_file(
    grants: &dyn PathGrants,
    watcher: &dyn WorkspaceWatcher,
    path: &str,
) -> Result<(), WorkspaceError> {
    let path = grants.require_existing_file(path).map_err(broker_error)?;
    watcher.watch_files(&[path]).map_err(WorkspaceError::Watch)
}

pub fn watch_files(
    grants: &dyn PathGrants,
    watcher: &dyn WorkspaceWatcher,
    paths: &[String],
) -> Result<(), WorkspaceError> {
    // Authorize each path independently; skip any no longer granted/existing rather than failing
    // the whole arm (a navigated sub-page may have been deleted or had its grant revoked).
    let authorized: Vec<String> = paths
        .iter()
        .filter_map(|p| grants.require_existing_file(p).ok())
        .collect();
    if authorized.is_empty() {
        // Nothing left to watch (last tab closed, or every grant revoked): drop the previous
        // watcher instead of leaving it armed on files no open tab owns anymore.
        watcher.clear_files();
        return Ok(());
    }
    watcher
        .watch_files(&authorized)
        .map_err(WorkspaceError::Watch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn natural_order_compares_digit_runs_numerically() {
        assert_eq!(natural_cmp("img2", "img10"), Ordering::Less);
        assert_eq!(natural_cmp("IMG2", "img2"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "a1"), Ordering::Less);
    }

    #[test]
    fn hex_path_header_round_trips() {
        assert_eq!(decode_hex_path("612f62").unwrap(), "a/b");
        assert!(decode_hex_path("").is_err());
        assert!(decode_hex_path("zz").is_err());
    }
}
