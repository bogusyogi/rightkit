//! Resumable single-file download with sha256 verification and atomic
//! placement. Each file downloads to `<file>.part` (+ `.meta` resume record)
//! and is renamed into place only after the hash verifies, so a half-written
//! file never counts as installed. Origin: HeardRight `resumable_download.rs`
//! via ScrapeRight's model-store; adds cooperative cancel and typed errors.
//!
//! Byte-range resume requires a strong ETag match and matching resume
//! metadata; anything unprovable restarts from zero rather than risking a
//! spliced file.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::error::{Error, ErrorCode};
use crate::hash::sha256_file;

const CANCELLED: &str = "download cancelled";
const MAX_ATTEMPTS: usize = 3;
const BASE_BACKOFF_MS: u64 = 250;

pub struct DownloadRequest<'a> {
    pub url: &'a str,
    /// Scratch `.part` file the body streams into (same dir as `dest_path`).
    pub part_path: &'a Path,
    /// Final destination, committed by atomic rename after verification.
    pub dest_path: &'a Path,
    /// Expected lowercase-hex sha256 of the *file*. `None` downloads without a
    /// pinned hash (platforms whose manifest is not yet hash-pinned).
    pub expected_sha256: Option<&'a str>,
    /// Optional `Authorization` header value (e.g. `Bearer …`).
    pub authorization: Option<&'a str>,
    /// Polled between body chunks; `true` aborts with `ErrorCode::Cancelled`
    /// and keeps the `.part` for a later resume.
    pub cancelled: &'a dyn Fn() -> bool,
}

pub struct DownloadResult {
    pub received: u64,
    pub total: u64,
    /// SHA-256 of the committed file — recorded into the store manifest even
    /// when no pin was provided, so later verifications check real bytes.
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResumeMetadata {
    url: String,
    expected_sha256: Option<String>,
    etag: String,
    total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponsePlan {
    Append { offset: u64, total: u64 },
    Replace { total: u64 },
    RetryFromZero,
}

fn plan_response(
    offset: u64,
    stored_etag: Option<&str>,
    status: u16,
    content_range: Option<&str>,
    response_etag: Option<&str>,
    content_length: Option<u64>,
) -> Result<ResponsePlan, String> {
    if status == 200 {
        return Ok(ResponsePlan::Replace {
            total: content_length.unwrap_or(0),
        });
    }
    if status != 206 {
        return Err(format!("unexpected download response status {status}"));
    }

    let Some((start, end, total)) = content_range.and_then(parse_content_range) else {
        return Ok(ResponsePlan::RetryFromZero);
    };
    if start != offset || end < start || total <= end {
        return Ok(ResponsePlan::RetryFromZero);
    }
    if content_length.is_some_and(|length| length != end - start + 1) {
        return Ok(ResponsePlan::RetryFromZero);
    }
    if offset == 0 {
        return Ok(ResponsePlan::Replace { total });
    }
    if stored_etag.is_none()
        || stored_etag != response_etag
        || response_etag.and_then(strong_etag).is_none()
    {
        return Ok(ResponsePlan::RetryFromZero);
    }
    Ok(ResponsePlan::Append { offset, total })
}

#[derive(Debug)]
enum TransferError {
    Transient(String),
    Fatal(String),
}

fn write_response_body_with_progress(
    part: &Path,
    plan: ResponsePlan,
    reader: &mut dyn Read,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<u64, TransferError> {
    let (mut file, mut received, total) = match plan {
        ResponsePlan::Append { offset, total } => {
            let length = fs::metadata(part)
                .map_err(|error| TransferError::Fatal(format!("read partial metadata: {error}")))?
                .len();
            if length != offset {
                return Err(TransferError::Fatal(format!(
                    "partial length changed before resume: expected {offset}, got {length}"
                )));
            }
            let file = OpenOptions::new()
                .append(true)
                .open(part)
                .map_err(|error| {
                    TransferError::Fatal(format!("open partial for append: {error}"))
                })?;
            (file, offset, total)
        }
        ResponsePlan::Replace { total } => {
            let file = File::create(part)
                .map_err(|error| TransferError::Fatal(format!("create partial: {error}")))?;
            (file, 0, total)
        }
        ResponsePlan::RetryFromZero => {
            return Err(TransferError::Fatal(
                "response continuity was not established".to_string(),
            ));
        }
    };

    let mut buffer = [0u8; 1 << 16];
    loop {
        if cancelled() {
            let _ = file.sync_all();
            return Err(TransferError::Fatal(CANCELLED.to_string()));
        }
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                let _ = file.sync_data();
                return Err(TransferError::Transient(format!(
                    "read response body: {error}"
                )));
            }
        };
        file.write_all(&buffer[..count])
            .map_err(|error| TransferError::Fatal(format!("write partial: {error}")))?;
        received += count as u64;
        progress(received, total);
    }
    file.sync_all()
        .map_err(|error| TransferError::Fatal(format!("sync partial: {error}")))?;
    if total > 0 && received != total {
        return Err(TransferError::Transient(format!(
            "incomplete download: received {received} of {total} bytes"
        )));
    }
    Ok(received)
}

fn valid_pin(pin: &str) -> bool {
    pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Download `request.url` into `request.dest_path`, resuming a matching
/// `.part` when present. The dest is only ever replaced by a fully-downloaded,
/// hash-verified `.part` via atomic rename.
pub fn download_resumable(
    agent: &ureq::Agent,
    request: DownloadRequest<'_>,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, Error> {
    download_resumable_with_identity(agent, request, None, progress)
}

/// As [`download_resumable`], with an immutable, hash-pinned resume identity
/// independent of a rotating signed URL. Callers must bind it to object identity.
pub(crate) fn download_resumable_with_identity(
    agent: &ureq::Agent,
    request: DownloadRequest<'_>,
    identity: Option<&str>,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, Error> {
    let resume_url = identity
        .filter(|_| request.expected_sha256.is_some())
        .unwrap_or(request.url);
    download_inner(agent, request, resume_url, progress).map_err(|message| {
        let code = if message == CANCELLED {
            ErrorCode::Cancelled
        } else if message.contains("integrity check") {
            ErrorCode::HashMismatch
        } else {
            ErrorCode::Download
        };
        Error::new(code, message)
    })
}

fn download_inner(
    agent: &ureq::Agent,
    request: DownloadRequest<'_>,
    resume_url: &str,
    mut progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, String> {
    if let Some(parent) = request.part_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("create download dir: {error}"))?;
    }
    if let Some(parent) = request.dest_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("create dest dir: {error}"))?;
    }
    let metadata_path = metadata_path(request.part_path);
    let expected = request
        .expected_sha256
        .map(|pin| pin.trim().to_ascii_lowercase());
    if let Some(pin) = expected.as_deref() {
        if !valid_pin(pin) {
            cleanup_poisoned(request.part_path, &metadata_path);
            return Err("download checksum must be a 64-character SHA-256".to_string());
        }
    }

    // Fast path: a fully-downloaded, already-verified `.part` from a crashed
    // run can be committed without touching the network.
    if let Some(received) =
        completed_partial_candidate(request.part_path, &metadata_path, resume_url, &expected)?
    {
        let got = sha256_file(request.part_path)?;
        if expected
            .as_deref()
            .map(|pin| got.eq_ignore_ascii_case(pin))
            .unwrap_or(true)
        {
            commit_part(request.part_path, &metadata_path, request.dest_path)?;
            return Ok(DownloadResult {
                received,
                total: received,
                sha256: got,
            });
        }
        cleanup_poisoned(request.part_path, &metadata_path);
    }

    let mut last_transient = None;
    for attempt in 0..MAX_ATTEMPTS {
        let (offset, stored_etag) =
            resume_state(request.part_path, &metadata_path, resume_url, &expected)?;
        // Byte ranges and the final file hash must describe the same wire
        // representation; disallow transparent content-encoding changes.
        let mut builder = agent.get(request.url).header("Accept-Encoding", "identity");
        if let Some(authorization) = request.authorization {
            builder = builder.header("Authorization", authorization);
        }
        if offset > 0 {
            builder = builder
                .header("Range", format!("bytes={offset}-"))
                .header("If-Range", stored_etag.as_deref().unwrap_or_default());
        }

        let mut response = match builder.call() {
            Ok(response) => response,
            Err(error) => {
                if matches!(error, ureq::Error::StatusCode(401 | 403)) {
                    return Err(format!(
                        "download authorization rejected ({})",
                        match error {
                            ureq::Error::StatusCode(status) => status,
                            _ => unreachable!(),
                        }
                    ));
                }
                if matches!(error, ureq::Error::StatusCode(404)) {
                    return Err("model file not available on the download server (404)".to_string());
                }
                if offset > 0 && matches!(error, ureq::Error::StatusCode(412 | 416)) {
                    cleanup_poisoned(request.part_path, &metadata_path);
                    last_transient =
                        Some("server rejected the saved range; restarting from zero".to_string());
                    backoff(attempt);
                    continue;
                }
                let message = format!("download request failed: {error}");
                if !is_transient_ureq_error(&error) {
                    return Err(message);
                }
                last_transient = Some(message);
                backoff(attempt);
                continue;
            }
        };
        let status = response.status().as_u16();
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let content_range = response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let response_etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .and_then(strong_etag)
            .map(str::to_string);
        let plan = plan_response(
            offset,
            stored_etag.as_deref(),
            status,
            content_range.as_deref(),
            response_etag.as_deref(),
            content_length,
        )?;
        if plan == ResponsePlan::RetryFromZero {
            cleanup_poisoned(request.part_path, &metadata_path);
            last_transient = Some("server response could not prove partial continuity".to_string());
            backoff(attempt);
            continue;
        }

        let response_etag_value = response_etag.unwrap_or_default();
        let total = match plan {
            ResponsePlan::Append { total, .. } | ResponsePlan::Replace { total } => total,
            ResponsePlan::RetryFromZero => unreachable!(),
        };
        write_metadata(
            &metadata_path,
            &ResumeMetadata {
                url: resume_url.to_string(),
                expected_sha256: expected.clone(),
                etag: response_etag_value.clone(),
                total,
            },
        )?;
        let mut reader = response.body_mut().as_reader();
        match write_response_body_with_progress(
            request.part_path,
            plan,
            &mut reader,
            &mut progress,
            request.cancelled,
        ) {
            Ok(received) => {
                let got = sha256_file(request.part_path)?;
                if let Some(pin) = expected.as_deref() {
                    if !got.eq_ignore_ascii_case(pin) {
                        cleanup_poisoned(request.part_path, &metadata_path);
                        return Err(format!(
                            "downloaded model file failed integrity check: got {got}, expected {pin}"
                        ));
                    }
                }
                commit_part(request.part_path, &metadata_path, request.dest_path)?;
                let verified_total = if total == 0 { received } else { total };
                return Ok(DownloadResult {
                    received,
                    total: verified_total,
                    sha256: got,
                });
            }
            Err(TransferError::Fatal(message)) => return Err(message),
            Err(TransferError::Transient(message)) => {
                last_transient = Some(message);
                backoff(attempt);
            }
        }
    }
    Err(last_transient.unwrap_or_else(|| "download failed after bounded retries".to_string()))
}

/// `.part` + `.meta` → final dest via atomic rename (same dir ⇒ same volume).
/// The dest may already hold a corrupt file being repaired. `std::fs::rename`
/// replaces an existing file in one step on Unix (rename(2)) and Windows
/// (MoveFileExW with MOVEFILE_REPLACE_EXISTING), so the previous file is never
/// deleted first: if the rename fails it is still there.
fn commit_part(part: &Path, metadata: &Path, dest: &Path) -> Result<(), String> {
    fs::rename(part, dest)
        .map_err(|e| format!("commit {} -> {}: {e}", part.display(), dest.display()))?;
    let _ = fs::remove_file(metadata);
    Ok(())
}

pub fn discard_partial(part: &Path) {
    cleanup_poisoned(part, &metadata_path(part));
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn strong_etag(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.starts_with("W/")
        || value.len() < 2
        || !value.starts_with('"')
        || !value.ends_with('"')
    {
        return None;
    }
    Some(value)
}

fn metadata_path(part: &Path) -> PathBuf {
    let mut value = part.as_os_str().to_os_string();
    value.push(".meta");
    PathBuf::from(value)
}

/// Sibling `.part` path for a manifest file inside the entry dir.
pub fn part_path_for(dest_path: &Path) -> PathBuf {
    let mut value = dest_path.as_os_str().to_os_string();
    value.push(".part");
    PathBuf::from(value)
}

fn resume_state(
    part: &Path,
    metadata: &Path,
    url: &str,
    expected_sha256: &Option<String>,
) -> Result<(u64, Option<String>), String> {
    let offset = match fs::metadata(part) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(format!("read partial metadata: {error}")),
    };
    if offset == 0 {
        let _ = fs::remove_file(metadata);
        return Ok((0, None));
    }
    let resume = fs::read(metadata)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ResumeMetadata>(&bytes).ok())
        .filter(|resume| {
            resume.url == url
                && resume.expected_sha256.as_deref() == expected_sha256.as_deref()
                && strong_etag(&resume.etag).is_some()
        });
    if let Some(resume) = resume {
        return Ok((offset, Some(resume.etag)));
    }
    cleanup_poisoned(part, metadata);
    Ok((0, None))
}

fn completed_partial_candidate(
    part: &Path,
    metadata: &Path,
    url: &str,
    expected_sha256: &Option<String>,
) -> Result<Option<u64>, String> {
    let length = match fs::metadata(part) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read partial metadata: {error}")),
    };
    let resume = fs::read(metadata)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ResumeMetadata>(&bytes).ok());
    Ok(resume
        .filter(|resume| {
            resume.url == url
                && resume.expected_sha256.as_deref() == expected_sha256.as_deref()
                && resume.total > 0
                && resume.total == length
        })
        .map(|_| length))
}

fn write_metadata(path: &Path, metadata: &ResumeMetadata) -> Result<(), String> {
    let staged = metadata_path(path);
    let bytes = serde_json::to_vec(metadata)
        .map_err(|error| format!("serialize resume metadata: {error}"))?;
    fs::write(&staged, bytes).map_err(|error| format!("write resume metadata: {error}"))?;
    fs::rename(&staged, path).map_err(|error| format!("commit resume metadata: {error}"))
}

fn cleanup_poisoned(part: &Path, metadata: &Path) {
    let _ = fs::remove_file(part);
    let _ = fs::remove_file(metadata);
}

fn is_transient_ureq_error(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::StatusCode(status) => {
            *status == 408 || *status == 429 || (500..=599).contains(status)
        }
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => false,
        _ => true,
    }
}

/// Rough "is this a connectivity problem" classifier for offline-first-run
/// messaging — anything that is not an HTTP status response counts (DNS,
/// connect, TLS, timeout, body-read).
pub fn is_connectivity_error(message: &str) -> bool {
    !(message.contains("status")
        || message.contains("(404)")
        || message.contains("integrity check")
        || message.contains("not available"))
}

fn backoff(attempt: usize) {
    if attempt + 1 < MAX_ATTEMPTS {
        thread::sleep(Duration::from_millis(BASE_BACKOFF_MS << attempt.min(2)));
    }
}

/// Shared HTTP agent for model downloads.
pub fn http_agent() -> ureq::Agent {
    http_agent_with_redirects(10)
}

pub(crate) fn http_agent_with_redirects(max_redirects: u32) -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(max_redirects)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .build()
        .into()
}
