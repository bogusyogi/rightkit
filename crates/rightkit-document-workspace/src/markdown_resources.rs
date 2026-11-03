//! Markdown reference inspection and pasted-asset staging (save, commit, roll back). Every
//! function is synchronous; the host runs them off its IPC thread.

use crate::error::{broker_error, WorkspaceError};
use crate::grants::PathGrants;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rightkit_fs::atomic::create_dir_all_durable;
use rightkit_fs::digest::sha256_bytes;
use rightkit_fs::{
    atomic_write_with_options, canonicalize_plain, contain, contain_lenient, numbered_candidates,
    AtomicWriteOptions, FsError, ReplaceMode,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

const MAX_ASSET_BYTES: usize = 20 * 1024 * 1024;
const MAX_MARKDOWN_TARGET_BYTES: u64 = 2 * 1024 * 1024;
const MAX_INSPECTION_REQUESTS: usize = 128;
const ALLOWED_MIMES: &[&str] = &["png", "jpeg", "webp", "gif", "svg"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMarkdownAssetRequest {
    pub payload_base64: String,
    pub hint_name: String,
    pub mime_kind: String,
    pub document_path: String,
    #[serde(default)]
    pub assets_folder: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedMarkdownAsset {
    pub relative_path: String,
    pub absolute_path: String,
    pub reused_existing: bool,
    pub byte_length: usize,
    pub rollback_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackMarkdownAssetRequest {
    pub rollback_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitMarkdownAssetRequest {
    pub rollback_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownResourceRequest {
    pub raw_url: String,
    pub need_text: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownResourceResult {
    pub raw_url: String,
    pub path_part: String,
    pub exists: bool,
    pub is_file: bool,
    pub format: Option<String>,
    pub granted_path: Option<String>,
    pub text: Option<String>,
    pub error: Option<String>,
}

/// Inspect explicitly referenced local resources only. This never enumerates a directory; each
/// target remains contained within the opened document folder.
pub fn inspect_markdown_resources(
    grants: &dyn PathGrants,
    document_path: &str,
    requests: Vec<MarkdownResourceRequest>,
) -> Result<Vec<MarkdownResourceResult>, WorkspaceError> {
    check_inspection_request_count(requests.len())?;
    let authorized = grants
        .require_existing_file(document_path)
        .map_err(broker_error)?;
    let document = PathBuf::from(authorized);
    let root = std::fs::canonicalize(
        document
            .parent()
            .ok_or_else(|| WorkspaceError::Other("no-document-parent".to_string()))?,
    )
    .map_err(|e| WorkspaceError::Io(format!("canonicalize markdown root: {e}")))?;
    Ok(requests
        .into_iter()
        .map(|request| inspect_resource(&root, request, grants))
        .collect())
}

fn check_inspection_request_count(count: usize) -> Result<(), WorkspaceError> {
    if count > MAX_INSPECTION_REQUESTS {
        return Err(WorkspaceError::DocumentTooLarge(format!(
            "markdown resource inspection exceeds {MAX_INSPECTION_REQUESTS} requests"
        )));
    }
    Ok(())
}

fn inspect_resource(
    root: &Path,
    request: MarkdownResourceRequest,
    grants: &dyn PathGrants,
) -> MarkdownResourceResult {
    let path_part = split_local_url(&request.raw_url);
    let mut result = MarkdownResourceResult {
        raw_url: request.raw_url,
        path_part: path_part.clone(),
        exists: false,
        is_file: false,
        format: None,
        granted_path: None,
        text: None,
        error: None,
    };
    let relative = match safe_relative_reference(&path_part) {
        Ok(path) => path,
        Err(error) => {
            result.error = Some(error);
            return result;
        }
    };
    let target = match std::fs::canonicalize(root.join(relative)) {
        Ok(path) => path,
        Err(_) => {
            result.error = Some("missing-target".to_string());
            return result;
        }
    };
    if !target.starts_with(root) {
        result.error = Some("path-escape".to_string());
        return result;
    }
    let metadata = match std::fs::metadata(&target) {
        Ok(meta) => meta,
        Err(_) => {
            result.error = Some("missing-target".to_string());
            return result;
        }
    };
    result.exists = true;
    result.is_file = metadata.is_file();
    if !metadata.is_file() {
        result.error = Some("not-a-file".to_string());
        return result;
    }
    let format = markdown_format(&target);
    result.format = format.map(str::to_string);
    if format.is_none() {
        result.error = Some("unsupported-format".to_string());
        return result;
    }
    if request.need_text && format.is_some() {
        if metadata.len() > MAX_MARKDOWN_TARGET_BYTES {
            result.error = Some("target-too-large".to_string());
            return result;
        }
        match std::fs::read_to_string(&target) {
            Ok(text) => result.text = Some(text),
            Err(_) => {
                result.error = Some("read-failed".to_string());
                return result;
            }
        }
    }
    // Inspection can only extend a file grant for a Markdown document. This
    // prevents this command from becoming a same-folder arbitrary-file grant.
    match grants.grant_existing_file_path(&target) {
        Ok(path) => result.granted_path = Some(path),
        Err(_) => result.error = Some("grant-failed".to_string()),
    }
    result
}

fn split_local_url(raw: &str) -> String {
    raw.split(['?', '#']).next().unwrap_or_default().to_string()
}

fn safe_relative_reference(raw: &str) -> Result<PathBuf, String> {
    let decoded = percent_decode(raw)?;
    if decoded.is_empty()
        || decoded.contains('\0')
        || decoded.starts_with('/')
        || decoded.starts_with('\\')
        || decoded.contains("://")
        || decoded.as_bytes().get(1) == Some(&b':')
    {
        return Err("invalid-reference-path".to_string());
    }
    let path = Path::new(&decoded);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("invalid-reference-path".to_string());
    }
    Ok(path.to_path_buf())
}

/// Strict percent-decoder for a reference path: every `%` must start two hex digits and the decoded
/// bytes must be valid UTF-8, otherwise the whole reference is refused (and `+` stays `+`).
/// `rightkit_http::percent_decode_component` keeps a malformed escape as written and replaces
/// invalid UTF-8 lossily, so it cannot answer "was this input well formed"; the hex digits come
/// from the shared `rightkit_fs::digest::unhex`.
fn percent_decode(raw: &str) -> Result<String, String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let byte = raw
                .get(index + 1..index + 3)
                .and_then(rightkit_fs::digest::unhex)
                .and_then(|decoded| decoded.first().copied())
                .ok_or_else(|| "invalid-percent-encoding".to_string())?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "invalid-percent-encoding".to_string())
}

fn markdown_format(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("md") => Some("md"),
        Some("markdown") => Some("markdown"),
        Some("mdx") => Some("mdx"),
        _ => None,
    }
}

#[derive(Debug)]
struct PendingAsset {
    path: PathBuf,
    parent: PathBuf,
    byte_length: u64,
    digest: String,
}

/// Staged-asset transactions awaiting commit or rollback, keyed by rollback token.
#[derive(Default)]
pub struct MarkdownAssetTransactions {
    pending: Mutex<HashMap<String, PendingAsset>>,
}

/// Validate and stage a pasted asset next to the document: decode, type and size checks, an
/// assets folder contained in the document's folder, content-addressed dedupe, and a pending
/// transaction the caller later commits or rolls back.
pub fn save_markdown_asset(
    req: SaveMarkdownAssetRequest,
    grants: &dyn PathGrants,
    transactions: &MarkdownAssetTransactions,
) -> Result<SavedMarkdownAsset, WorkspaceError> {
    let mime_kind = req.mime_kind.to_lowercase();
    if !ALLOWED_MIMES.contains(&mime_kind.as_str()) {
        return Err(WorkspaceError::Other(format!(
            "mime-not-allowed: {mime_kind}"
        )));
    }
    let bytes = BASE64
        .decode(req.payload_base64.as_bytes())
        .map_err(|e| WorkspaceError::Other(format!("base64-decode-failed: {e}")))?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(WorkspaceError::Other(format!(
            "size-limit: {} bytes exceeds {}",
            bytes.len(),
            MAX_ASSET_BYTES
        )));
    }
    if mime_kind == "svg" && !is_safe_svg(&bytes) {
        return Err(WorkspaceError::Other(
            "svg-unsafe: forbidden element or attribute".to_string(),
        ));
    }

    let authorized_doc = grants
        .require_existing_file(&req.document_path)
        .map_err(broker_error)?;
    let doc_path = PathBuf::from(authorized_doc);
    let doc_parent = doc_path
        .parent()
        .ok_or_else(|| WorkspaceError::Other("no-document-parent".to_string()))?;
    let folder_name = req.assets_folder.unwrap_or_else(|| {
        format!(
            "{}.assets",
            doc_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document")
        )
    });
    validate_relative_folder(&folder_name)?;

    let canonical_parent = canonicalize_plain(doc_parent)
        .map_err(|e| WorkspaceError::Io(format!("canonicalize document parent: {e}")))?;
    // The assets folder may not exist yet: resolve it leniently (the deepest existing ancestor is
    // followed through links and must stay inside the document's folder), create it, then resolve
    // it again strictly so nothing that was created can lead out.
    let planned_assets = contain_lenient(&canonical_parent, &folder_name).map_err(escape_error)?;
    create_dir_all_durable(&planned_assets, false)
        .map_err(|e| WorkspaceError::Io(format!("create assets dir: {}", fs_error_text(e))))?;
    let canonical_assets = contain(&canonical_parent, &folder_name).map_err(escape_error)?;

    let digest = sha256_bytes(&bytes);
    let base_name = compute_file_name(&req.hint_name, &digest, &mime_kind);
    let (target, reused_existing) = store_asset(&canonical_assets, &base_name, &bytes, &digest)?;
    if reused_existing {
        return Ok(SavedMarkdownAsset {
            relative_path: relative_path(&canonical_parent, &target),
            absolute_path: target.to_string_lossy().to_string(),
            reused_existing: true,
            byte_length: bytes.len(),
            rollback_token: None,
        });
    }

    let target_name = target
        .file_name()
        .map(PathBuf::from)
        .ok_or_else(|| WorkspaceError::Other("asset target has no file name".to_string()))?;
    let canonical_target = match contain(&canonical_assets, &target_name) {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_file(&target);
            return Err(escape_error(error));
        }
    };

    let token = random_token()?;
    transactions
        .pending
        .lock()
        .map_err(|_| WorkspaceError::Other("asset transaction lock poisoned".to_string()))?
        .insert(
            token.clone(),
            PendingAsset {
                path: canonical_target.clone(),
                parent: canonical_assets,
                byte_length: bytes.len() as u64,
                digest,
            },
        );
    Ok(SavedMarkdownAsset {
        relative_path: relative_path(&canonical_parent, &canonical_target),
        absolute_path: canonical_target.to_string_lossy().to_string(),
        reused_existing: false,
        byte_length: bytes.len(),
        rollback_token: Some(token),
    })
}

/// Roll back a staged asset: re-stat, re-read and delete the file, refusing if it changed after
/// creation. Consumes the token.
pub fn rollback_markdown_asset(
    transactions: &MarkdownAssetTransactions,
    token: &str,
) -> Result<bool, WorkspaceError> {
    let Some(pending) = take_pending(transactions, token)? else {
        return Ok(false);
    };
    let canonical_parent = canonicalize_plain(
        pending
            .path
            .parent()
            .ok_or_else(|| WorkspaceError::Other("rollback path has no parent".to_string()))?,
    )
    .map_err(|error| WorkspaceError::Io(format!("rollback parent: {error}")))?;
    let canonical_path = canonicalize_plain(&pending.path)
        .map_err(|error| WorkspaceError::Io(format!("rollback target: {error}")))?;
    if canonical_parent != pending.parent || canonical_path != pending.path {
        return Err(WorkspaceError::BrokerDenied(
            "rollback-refused: asset path changed after creation".to_string(),
        ));
    }
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|error| WorkspaceError::Io(format!("rollback metadata: {error}")))?;
    if !metadata.is_file() || metadata.len() != pending.byte_length {
        return Err(WorkspaceError::Other(
            "rollback-refused: asset length changed after creation".to_string(),
        ));
    }
    let current = match std::fs::read(&canonical_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(WorkspaceError::Io(format!("rollback read: {error}"))),
    };
    let current_digest = sha256_bytes(&current);
    if current_digest != pending.digest {
        return Err(WorkspaceError::Other(
            "rollback-refused: asset changed after creation".to_string(),
        ));
    }
    std::fs::remove_file(&canonical_path)
        .map_err(|e| WorkspaceError::Io(format!("rollback: {e}")))?;
    Ok(true)
}

/// Commit a staged asset: the file stays and the token is consumed.
pub fn commit_markdown_asset(
    transactions: &MarkdownAssetTransactions,
    token: &str,
) -> Result<bool, WorkspaceError> {
    Ok(take_pending(transactions, token)?.is_some())
}

fn take_pending(
    transactions: &MarkdownAssetTransactions,
    token: &str,
) -> Result<Option<PendingAsset>, WorkspaceError> {
    Ok(transactions
        .pending
        .lock()
        .map_err(|_| WorkspaceError::Other("asset transaction lock poisoned".to_string()))?
        .remove(token))
}

fn validate_relative_folder(folder: &str) -> Result<(), WorkspaceError> {
    let path = Path::new(folder);
    if folder.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(WorkspaceError::Other(
            "path-traversal: assets folder must be a relative child path".to_string(),
        ));
    }
    Ok(())
}

/// The most candidate names tried for one asset: the base name, then `stem (1).ext` and on.
const MAX_ASSET_CANDIDATES: usize = 999;

/// Store `bytes` under the first free name of `numbered_candidates(base_name)` in `assets_dir`, or
/// reuse the first existing file with identical content. A free name is claimed by an atomic
/// no-replace write, so a concurrent writer that takes the name first is never overwritten: the
/// name is inspected again (an identical asset it wrote is reused) and otherwise skipped.
/// Returns the path and whether an existing file was reused.
fn store_asset(
    assets_dir: &Path,
    base_name: &str,
    bytes: &[u8],
    digest: &str,
) -> Result<(PathBuf, bool), WorkspaceError> {
    for name in numbered_candidates(base_name, MAX_ASSET_CANDIDATES) {
        let candidate = assets_dir.join(name);
        for _ in 0..2 {
            match std::fs::symlink_metadata(&candidate) {
                Ok(meta) if !meta.file_type().is_file() => break,
                Ok(_) => {
                    let existing = std::fs::read(&candidate)
                        .map_err(|e| WorkspaceError::Io(format!("read existing asset: {e}")))?;
                    if sha256_bytes(&existing) == digest && existing == bytes {
                        return Ok((candidate, true));
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match write_new_asset(&candidate, bytes) {
                        Ok(()) => return Ok((candidate, false)),
                        // Someone created the name between the inspection and the write.
                        Err(FsError::AlreadyExists(_)) => continue,
                        Err(error) => {
                            return Err(WorkspaceError::Io(format!(
                                "write asset: {}",
                                fs_error_text(error)
                            )));
                        }
                    }
                }
                Err(error) => {
                    return Err(WorkspaceError::Io(format!("inspect asset target: {error}")));
                }
            }
        }
    }
    Err(WorkspaceError::Other(format!(
        "asset-collision-limit: exhausted {MAX_ASSET_CANDIDATES} candidate names"
    )))
}

/// Publish `bytes` at `path` only if nothing exists there: flushed temp file, then an atomic
/// no-replace publish. A failed write leaves no partial asset behind.
fn write_new_asset(path: &Path, bytes: &[u8]) -> Result<(), FsError> {
    let options = AtomicWriteOptions {
        replace: ReplaceMode::NoReplace,
        ..AtomicWriteOptions::default()
    };
    atomic_write_with_options(path, options, |file| file.write_all(bytes))
}

/// The OS error text alone, without the temp path `FsError::Io` adds to its display.
fn fs_error_text(error: FsError) -> String {
    match error {
        FsError::Io { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

/// A refused or unresolvable asset location: a path that leaves the document folder is a broker
/// denial (as before); anything that could not be resolved is an I/O failure.
fn escape_error(error: rightkit_fs::ContainError) -> WorkspaceError {
    match error {
        rightkit_fs::ContainError::Io(io) => {
            WorkspaceError::Io(format!("resolve assets folder: {io}"))
        }
        _ => WorkspaceError::BrokerDenied(
            "asset folder escapes the granted document directory".to_string(),
        ),
    }
}

fn is_safe_svg(bytes: &[u8]) -> bool {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let lower = text.to_ascii_lowercase();
    let compact = lower
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && !ch.is_ascii_control())
        .collect::<String>();
    if !lower.contains("<svg") {
        return false;
    }
    !["<script", "<foreignobject", "<!entity"]
        .iter()
        .any(|needle| lower.contains(needle))
        && !["javascript:", "data:text/html"]
            .iter()
            .any(|needle| compact.contains(needle))
        && !has_inline_event_handler(&lower)
}

fn has_inline_event_handler(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        let boundary =
            index == 0 || bytes[index - 1].is_ascii_whitespace() || bytes[index - 1] == b'<';
        if boundary && bytes[index] == b'o' && bytes[index + 1] == b'n' {
            let mut cursor = index + 2;
            let name_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
                cursor += 1;
            }
            if cursor > name_start {
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if bytes.get(cursor) == Some(&b'=') {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn compute_file_name(hint: &str, digest: &str, mime: &str) -> String {
    let safe = sanitize_hint(hint);
    let stem = safe
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("asset");
    format!("{}-{}.{}", stem, &digest[..12], extension_for_mime(mime))
}

fn sanitize_hint(name: &str) -> String {
    let out = name
        .chars()
        .take(80)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        "asset".to_string()
    } else {
        out
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "png" => "png",
        "jpeg" => "jpg",
        "webp" => "webp",
        "gif" => "gif",
        "svg" => "svg",
        _ => "bin",
    }
}

fn random_token() -> Result<String, WorkspaceError> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes)
        .map_err(|e| WorkspaceError::Other(format!("asset token generation failed: {e}")))?;
    Ok(rightkit_crypto::hex_encode(&bytes))
}

fn relative_path(base: &Path, target: &Path) -> String {
    target
        .strip_prefix(base)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| target.to_string_lossy().replace('\\', "/"))
}
