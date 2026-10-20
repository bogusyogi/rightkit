//! Resumable single-file download with sha256 verification and atomic
//! placement. HTTP bytes stream through RightKit's `AtomicWriter` with a digest;
//! `<file>.part` (+ `.meta` resume record) separately checkpoints resumable bytes,
//! so failed transfers and hashes never replace the existing destination. Origin:
//! HeardRight `resumable_download.rs`
//! via ScrapeRight's model-store; adds cooperative cancel and typed errors.
//!
//! Byte-range resume requires a strong ETag match and matching resume
//! metadata; anything unprovable restarts from zero rather than risking a
//! spliced file. The one opt-in exception is [`DownloadOptions::resume_without_strong_etag`],
//! which needs a pinned sha256 so a spliced file is still caught.
//!
//! Model payloads are rebuildable: checked commit syncs file contents before verification
//! & atomic rename, without syncing the parent directory afterward. Success means verified,
//! synced bytes are atomically visible; it does not promise rename persistence after power loss.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Error, ErrorCode};
use crate::http::{AsHttpClient, IdleReader, ModelHttp};
use rightkit_fs::atomic::{AtomicOptions, AtomicWriter};
use rightkit_fs::digest::Sha256Hasher;
use rightkit_http::{HttpError, Request};

const CANCELLED: &str = "download cancelled";
const BASE_BACKOFF_MS: u64 = 250;
/// Start of a failure message from the local disk (create, write, sync, rename, hash read).
/// `download_resumable*` reports these as `ErrorCode::Io`, never `ErrorCode::Download`.
const IO_PREFIX: &str = "local disk error: ";
/// Start of the failure message when a download is larger than the caller's size cap.
const CAP_PREFIX: &str = "download exceeds the size cap: ";

fn io_msg(context: &str, error: impl std::fmt::Display) -> String {
    format!("{IO_PREFIX}{context}: {error}")
}

/// Default Rename has no post-publication cleanup, & disabling directory sync leaves no
/// fallible operation after publication. `commit_checked` still syncs file contents first.
/// Keep this policy restricted to rebuildable model payloads.
fn model_payload_atomic_options() -> AtomicOptions {
    AtomicOptions {
        durable: false,
        ..AtomicOptions::default()
    }
}

/// Compare names in their resolved existing parent without rewriting caller paths.
/// The file itself may be absent; canonicalizing the full file would miss that case.
fn resolved_parent_name(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io_msg("invalid download path", "missing filename"))?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // Use the filesystem owner's plain-name rule without rejecting Unicode or
        // converting non-Unicode names lossily. Win32 drops these final UTF-16 units.
        if name
            .to_str()
            .is_some_and(rightkit_fs::names::is_unsafe_windows_component)
            || matches!(name.encode_wide().last(), Some(0x20 | 0x2e))
        {
            return Err(io_msg(
                "invalid download path",
                "not a plain Windows filename",
            ));
        }
    }
    let parent =
        fs::canonicalize(parent).map_err(|error| io_msg("resolve download parent", error))?;
    Ok(parent.join(name))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn same_download_name(left: &Path, right: &Path) -> Result<bool, String> {
    Ok(left == right)
}

#[cfg(target_os = "macos")]
fn same_download_name(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::unix::fs::DirBuilderExt;
    if left == right {
        return Ok(true);
    }
    let left_parent = left.parent().expect("resolved download name has a parent");
    let right_parent = right.parent().expect("resolved download name has a parent");
    if rightkit_fs::FileKey::of_path(left_parent)
        .map_err(|e| io_msg("identify download parent", e))?
        != rightkit_fs::FileKey::of_path(right_parent)
            .map_err(|e| io_msg("identify download parent", e))?
    {
        return Ok(false);
    }
    // Ask this filesystem rather than guessing its case or Unicode normalization
    // rules. Both names are tested inside a new private directory on the same
    // volume; neither actual checkpoint nor destination is created or written.
    let mut probe = None;
    for _ in 0..16 {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| io_msg("name probe randomness", e))?;
        let candidate = left_parent.join(format!(
            ".rightkit-model-name-probe-{}",
            rightkit_fs::digest::hex(&random),
        ));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => {
                probe = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_msg("create private name probe", error)),
        }
    }
    let probe = probe.ok_or_else(|| io_msg("create private name probe", "name collisions"))?;
    let left_probe = probe.join(left.file_name().expect("resolved download name has a leaf"));
    let right_probe = probe.join(
        right
            .file_name()
            .expect("resolved download name has a leaf"),
    );
    let mut created_leaf = false;
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&left_probe)
            .map_err(|e| io_msg("create name probe leaf", e))?;
        created_leaf = true;
        let key = rightkit_fs::FileKey::of_file(&file, &left_probe)
            .map_err(|e| io_msg("identify name probe leaf", e))?;
        match fs::metadata(&right_probe) {
            Ok(_) => Ok(rightkit_fs::FileKey::of_path(&right_probe)
                .map_err(|e| io_msg("identify alternate name probe leaf", e))?
                == key),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_msg("read alternate name probe leaf", error)),
        }
    })();
    // A failed create_new does not confer ownership of that leaf. Always attempt
    // both cleanups, even when probing failed, & never recursively remove unknown
    // entries. Ordinary concurrent creators cannot enter this private directory.
    let leaf_cleanup = if created_leaf {
        fs::remove_file(&left_probe)
    } else {
        Ok(())
    };
    let dir_cleanup = fs::remove_dir(&probe);
    leaf_cleanup.map_err(|e| io_msg("remove name probe leaf", e))?;
    dir_cleanup.map_err(|e| io_msg("remove private name probe", e))?;
    result
}

#[cfg(windows)]
fn same_download_name(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    // Different resolved parents cannot name the same absent entry. File identity
    // also handles alternate spellings of the same parent without text folding.
    let left_parent = left.parent().expect("resolved download name has a parent");
    let right_parent = right.parent().expect("resolved download name has a parent");
    if rightkit_fs::FileKey::of_path(left_parent)
        .map_err(|e| io_msg("identify download parent", e))?
        != rightkit_fs::FileKey::of_path(right_parent)
            .map_err(|e| io_msg("identify download parent", e))?
    {
        return Ok(false);
    }
    let left: Vec<u16> = left
        .file_name()
        .expect("resolved download name has a leaf")
        .encode_wide()
        .collect();
    let right: Vec<u16> = right
        .file_name()
        .expect("resolved download name has a leaf")
        .encode_wide()
        .collect();
    let left_len = i32::try_from(left.len()).map_err(|e| io_msg("compare download names", e))?;
    let right_len = i32::try_from(right.len()).map_err(|e| io_msg("compare download names", e))?;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CompareStringOrdinal(
            left: *const u16,
            left_len: i32,
            right: *const u16,
            right_len: i32,
            ignore_case: i32,
        ) -> i32;
    }
    // SAFETY: each pointer addresses its UTF-16 vector for the explicit length;
    // the call retains no pointer. Native ordinal casing avoids Unicode approximations.
    match unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) } {
        0 => Err(io_msg(
            "compare download names",
            std::io::Error::last_os_error(),
        )),
        2 => Ok(true),
        _ => Ok(false),
    }
}

/// The total time budget of one download call ([`crate::http::ModelHttpOptions::total_timeout`]),
/// started when the first request is about to be made.
struct Budget {
    total: Duration,
    deadline: Instant,
}

impl Budget {
    /// `None` when no budget is set (or it is too large to represent as an instant).
    fn start(total: Option<Duration>) -> Option<Self> {
        let total = total?;
        Some(Self {
            total,
            deadline: Instant::now().checked_add(total)?,
        })
    }

    fn left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    fn spent(&self) -> bool {
        self.left().is_zero()
    }

    /// The failure message, also recorded as the transport failure that ended the call.
    fn exceeded(&self, slot: &FailureSlot) -> String {
        let message = format!(
            "download exceeded the total time budget of {:?}",
            self.total
        );
        *slot.borrow_mut() = Some(LastFailure::Transport(message.clone()));
        message
    }
}

/// The HTTP-level failure that ended a download attempt, for typed callers
/// (`ModelStore::try_ensure_entitled`). Cleared when an attempt's response is accepted, so it
/// only describes the failure that ended a call that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LastFailure {
    /// A non-2xx response; `retry_after` is the parsed `Retry-After` header, when present.
    Status {
        status: u16,
        retry_after: Option<Duration>,
    },
    /// No response: connect, TLS, timeout, address policy or body-read failure. The message is
    /// the one the string-returning API reports.
    Transport(String),
}

/// Where [`download_resumable_recorded`] stores its last [`LastFailure`].
pub(crate) type FailureSlot = std::cell::RefCell<Option<LastFailure>>;

pub struct DownloadRequest<'a> {
    pub url: &'a str,
    /// Scratch `.part` resume checkpoint (same dir as `dest_path`).
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadResult {
    pub received: u64,
    pub total: u64,
    /// SHA-256 of the committed file — recorded into the store manifest even
    /// when no pin was provided, so later verifications check real bytes.
    pub sha256: String,
}

/// Opt-in behaviour for [`download_resumable_with_options`]. `Default` is exactly
/// [`download_resumable`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownloadOptions {
    /// On cancel, delete the `.part` and its resume record instead of keeping them for a later
    /// resume. Off by default: a cancelled download keeps its partial bytes.
    pub discard_partial_on_cancel: bool,
    /// Resume a `.part` although the server offers no strong ETag (only a weak one,
    /// `Last-Modified`, or nothing). The resume continues only when the request pins
    /// `expected_sha256`, the response continues exactly at the saved offset, and its total size
    /// equals the saved total. Without a pin this is ignored and the download restarts from zero,
    /// because nothing would catch a spliced file. A same-size splice that passes the size check
    /// is caught by the pin after the whole file is downloaded: the `.part` is deleted and the
    /// call fails with `ErrorCode::HashMismatch`.
    pub resume_without_strong_etag: bool,
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
    weak_total: Option<u64>,
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
    if let Some(stored_total) = weak_total {
        // No strong validator to compare: the exact continuation and the saved total are all
        // that can be checked here. The request's pinned sha256 catches a same-size splice.
        if total != stored_total {
            return Ok(ResponsePlan::RetryFromZero);
        }
        return Ok(ResponsePlan::Append { offset, total });
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
    /// The call's total time budget ran out while the body was streaming.
    BudgetSpent,
    /// The body is larger than the caller's size cap.
    TooLarge(String),
}

fn write_response_body_with_progress(
    part: &Path,
    plan: ResponsePlan,
    writer: &mut AtomicWriter,
    digest: &mut Sha256Hasher,
    reader: &mut dyn Read,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
    deadline: Option<Instant>,
    max_bytes: Option<u64>,
) -> Result<u64, TransferError> {
    let (mut file, mut received, total) = match plan {
        ResponsePlan::Append { offset, total } => {
            let length = fs::metadata(part)
                .map_err(|error| TransferError::Fatal(io_msg("read partial metadata", error)))?
                .len();
            if length != offset {
                return Err(TransferError::Fatal(format!(
                    "partial length changed before resume: expected {offset}, got {length}"
                )));
            }
            let file = OpenOptions::new()
                .append(true)
                .open(part)
                .map_err(|error| TransferError::Fatal(io_msg("open partial for append", error)))?;
            (file, offset, total)
        }
        ResponsePlan::Replace { total } => {
            let file = File::create(part)
                .map_err(|error| TransferError::Fatal(io_msg("create partial", error)))?;
            (file, 0, total)
        }
        ResponsePlan::RetryFromZero => {
            return Err(TransferError::Fatal(
                "response continuity was not established".to_string(),
            ));
        }
    };

    if let Some(cap) = max_bytes {
        if total > cap {
            return Err(TransferError::TooLarge(format!(
                "{CAP_PREFIX}the server reports {total} bytes, the cap is {cap}"
            )));
        }
    }
    let mut buffer = [0u8; 1 << 16];
    loop {
        if cancelled() {
            let _ = file.sync_all();
            return Err(TransferError::Fatal(CANCELLED.to_string()));
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            let _ = file.sync_all();
            return Err(TransferError::BudgetSpent);
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
        writer
            .write_all(&buffer[..count])
            .map_err(|error| TransferError::Fatal(io_msg("write atomic destination", error)))?;
        digest.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|error| TransferError::Fatal(io_msg("write partial", error)))?;
        received += count as u64;
        if let Some(cap) = max_bytes.filter(|cap| received > *cap) {
            let _ = file.sync_all();
            return Err(TransferError::TooLarge(format!(
                "{CAP_PREFIX}received more than {cap} bytes"
            )));
        }
        progress(received, total);
    }
    file.sync_all()
        .map_err(|error| TransferError::Fatal(io_msg("sync partial", error)))?;
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
/// `.part` when present. HTTP bytes enter destination-local atomic staging;
/// only fully downloaded, hash-verified staging replaces the destination.
/// A failure preserves existing destination contents. Success atomically exposes verified,
/// synced file contents; parent-directory crash durability is not promised for rebuildable models.
///
/// Byte-range resume needs a strong ETag (`"..."`, not `W/"..."`) that matches the saved resume
/// record. A server that sends only a weak ETag, `Last-Modified` or no validator restarts from
/// zero, unless [`download_resumable_with_options`] is called with
/// [`DownloadOptions::resume_without_strong_etag`] and the request pins a sha256.
///
/// Requests go through the vetted `rightkit-http` transport (see [`crate::http`]).
/// Pass a [`Client`]; a legacy `&ureq::Agent` still compiles but only contributes
/// its redirect limit.
pub fn download_resumable(
    client: &impl AsHttpClient,
    request: DownloadRequest<'_>,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, Error> {
    download_resumable_with_identity(&client.vetted_client(), request, None, progress)
}

/// As [`download_resumable`], with an optional size cap. The cap is checked against the
/// size the server reports (`Content-Length` / `Content-Range`) before any byte is written,
/// and again against the bytes received while streaming, so a server that lies or sends no
/// length still cannot fill the disk past `max_bytes`. A download over the cap fails with
/// `ErrorCode::SizeMismatch` and its `.part` is removed. `None` is exactly
/// [`download_resumable`]. Disk-write failures (full disk, permissions, rename) are
/// `ErrorCode::Io` in both functions; network failures stay `ErrorCode::Download`.
pub fn download_resumable_capped(
    client: &impl AsHttpClient,
    request: DownloadRequest<'_>,
    max_bytes: Option<u64>,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, Error> {
    download_resumable_recorded_capped(
        &client.vetted_client(),
        request,
        None,
        max_bytes,
        progress,
        &FailureSlot::default(),
    )
}

/// As [`download_resumable_capped`], with the opt-in behaviour in `options` (see
/// [`DownloadOptions`]). `DownloadOptions::default()` with `max_bytes: None` is exactly
/// [`download_resumable`].
pub fn download_resumable_with_options(
    client: &impl AsHttpClient,
    request: DownloadRequest<'_>,
    max_bytes: Option<u64>,
    options: DownloadOptions,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, Error> {
    download_resumable_recorded_with(
        &client.vetted_client(),
        request,
        None,
        max_bytes,
        options,
        progress,
        &FailureSlot::default(),
    )
}

/// As [`download_resumable`], with an immutable, hash-pinned resume identity
/// independent of a rotating signed URL. Callers must bind it to object identity.
pub(crate) fn download_resumable_with_identity(
    client: &ModelHttp,
    request: DownloadRequest<'_>,
    identity: Option<&str>,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, Error> {
    download_resumable_recorded(client, request, identity, progress, &FailureSlot::default())
}

/// As [`download_resumable_with_identity`], also leaving the last HTTP-level failure in `slot`.
pub(crate) fn download_resumable_recorded(
    client: &ModelHttp,
    request: DownloadRequest<'_>,
    identity: Option<&str>,
    progress: impl FnMut(u64, u64),
    slot: &FailureSlot,
) -> Result<DownloadResult, Error> {
    download_resumable_recorded_capped(client, request, identity, None, progress, slot)
}

pub(crate) fn download_resumable_recorded_capped(
    client: &ModelHttp,
    request: DownloadRequest<'_>,
    identity: Option<&str>,
    max_bytes: Option<u64>,
    progress: impl FnMut(u64, u64),
    slot: &FailureSlot,
) -> Result<DownloadResult, Error> {
    download_resumable_recorded_with(
        client,
        request,
        identity,
        max_bytes,
        DownloadOptions::default(),
        progress,
        slot,
    )
}

/// The one body behind the `download_resumable*` entry points; `options` opts into the
/// behaviour described on [`DownloadOptions`].
fn download_resumable_recorded_with(
    client: &ModelHttp,
    request: DownloadRequest<'_>,
    identity: Option<&str>,
    max_bytes: Option<u64>,
    options: DownloadOptions,
    progress: impl FnMut(u64, u64),
    slot: &FailureSlot,
) -> Result<DownloadResult, Error> {
    let resume_url = identity
        .filter(|_| request.expected_sha256.is_some())
        .unwrap_or(request.url);
    let part_path = request.part_path.to_path_buf();
    let outcome = download_inner(
        client, request, resume_url, max_bytes, options, progress, slot,
    );
    if options.discard_partial_on_cancel && matches!(&outcome, Err(message) if message == CANCELLED)
    {
        discard_partial(&part_path);
    }
    outcome.map_err(|message| {
        let code = if message == CANCELLED {
            ErrorCode::Cancelled
        } else if message.starts_with(IO_PREFIX) {
            ErrorCode::Io
        } else if message.starts_with(CAP_PREFIX) {
            ErrorCode::SizeMismatch
        } else if message.contains("integrity check") {
            ErrorCode::HashMismatch
        } else {
            ErrorCode::Download
        };
        Error::new(code, message)
    })
}

fn download_inner(
    http: &ModelHttp,
    request: DownloadRequest<'_>,
    resume_url: &str,
    max_bytes: Option<u64>,
    options: DownloadOptions,
    mut progress: impl FnMut(u64, u64),
    slot: &FailureSlot,
) -> Result<DownloadResult, String> {
    let metadata_path = metadata_path(request.part_path);
    let staged_metadata_path = self::metadata_path(&metadata_path);
    // Resolve only parents, after the existing directory-creation step. Comparing
    // complete paths with canonicalize would fail while the destination is absent.
    if let Some(parent) = request.part_path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_msg("create download dir", error))?;
    }
    if let Some(parent) = request.dest_path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_msg("create dest dir", error))?;
    }
    // Inspect the endpoints without following links: a dangling link looks absent
    // to metadata(), but a later checkpoint write could create its destination.
    // Parent symlinks remain supported through resolved_parent_name below.
    for endpoint in [
        request.dest_path,
        request.part_path,
        metadata_path.as_path(),
        staged_metadata_path.as_path(),
    ] {
        match fs::symlink_metadata(endpoint) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io_msg("invalid download path", "symlink endpoint refused"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_msg("inspect download endpoint", error)),
        }
    }
    let destination_name = resolved_parent_name(request.dest_path)?;
    // A checkpoint must never alias the existing destination: creating/appending it
    // would otherwise bypass atomic staging & damage the bytes we promise to preserve.
    let destination_key = match fs::metadata(request.dest_path) {
        Ok(_) => Some(
            rightkit_fs::FileKey::of_path(request.dest_path)
                .map_err(|error| io_msg("identify destination", error))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_msg("read destination metadata", error)),
    };
    for checkpoint in [
        request.part_path,
        metadata_path.as_path(),
        staged_metadata_path.as_path(),
    ] {
        if same_download_name(&resolved_parent_name(checkpoint)?, &destination_name)? {
            return Err(io_msg(
                "invalid checkpoint",
                "checkpoint aliases destination",
            ));
        }
        if let Some(destination_key) = destination_key {
            match fs::metadata(checkpoint) {
                Ok(_) => {
                    if rightkit_fs::FileKey::of_path(checkpoint)
                        .map_err(|error| io_msg("identify checkpoint", error))?
                        == destination_key
                    {
                        return Err(io_msg(
                            "invalid checkpoint",
                            "checkpoint aliases destination",
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_msg("read checkpoint metadata", error)),
            }
        }
    }
    if (request.cancelled)() {
        return Err(CANCELLED.to_string());
    }
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
        match commit_part(
            request.part_path,
            &metadata_path,
            request.dest_path,
            expected.as_deref(),
        ) {
            Ok(got) => {
                return Ok(DownloadResult {
                    received,
                    total: received,
                    sha256: got,
                });
            }
            Err(message) if message.contains("integrity check") => {
                cleanup_poisoned(request.part_path, &metadata_path);
            }
            Err(message) => return Err(message),
        }
    }

    let attempts = (http.retries() as usize).saturating_add(1);
    let budget = Budget::start(http.total_timeout());
    let mut last_transient = None;
    for attempt in 0..attempts {
        if let Some(budget) = &budget {
            if budget.spent() {
                return Err(budget.exceeded(slot));
            }
        }
        let Resume {
            offset,
            etag: stored_etag,
            total: stored_total,
        } = resume_state(
            request.part_path,
            &metadata_path,
            resume_url,
            &expected,
            options.resume_without_strong_etag,
        )?;
        // A weak resume has no ETag to send as If-Range; the saved total is its only proof.
        let weak_total = (offset > 0 && stored_etag.is_none()).then_some(stored_total);
        // Byte ranges and the final file hash must describe the same wire
        // representation; disallow transparent content-encoding changes.
        let mut req = Request::get(request.url).header("Accept-Encoding", "identity");
        if let Some(authorization) = request.authorization {
            req = req.secret_header("Authorization", authorization);
        }
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
            if let Some(etag) = stored_etag.as_deref() {
                req = req.header("If-Range", etag);
            }
        }

        if let Some(budget) = &budget {
            // The rest of the budget bounds this attempt, headers and body.
            req = req.timeout(budget.left());
        }

        let response = match http.client_for(request.url).stream_any(&req) {
            Ok(response) if response.status >= 300 => {
                let status = response.status;
                let retry_after = response
                    .header("retry-after")
                    .and_then(crate::suite::grant::parse_retry_after_header);
                *slot.borrow_mut() = Some(LastFailure::Status {
                    status,
                    retry_after,
                });
                drop(response);
                match status {
                    401 | 403 => {
                        return Err(format!("download authorization rejected ({status})"));
                    }
                    404 => {
                        return Err(
                            "model file not available on the download server (404)".to_string()
                        );
                    }
                    412 | 416 if offset > 0 => {
                        cleanup_poisoned(request.part_path, &metadata_path);
                        last_transient = Some(
                            "server rejected the saved range; restarting from zero".to_string(),
                        );
                        backoff(attempt, attempts, budget.as_ref());
                        continue;
                    }
                    _ => {
                        let message = format!("download request failed: http status: {status}");
                        if !is_transient_status(status) {
                            return Err(message);
                        }
                        last_transient = Some(message);
                        backoff(attempt, attempts, budget.as_ref());
                        continue;
                    }
                }
            }
            Ok(response) => {
                *slot.borrow_mut() = None;
                response
            }
            Err(error) => {
                let message = format!("download request failed: {error}");
                *slot.borrow_mut() = Some(match &error {
                    rightkit_http::HttpError::Status {
                        status,
                        retry_after,
                        ..
                    } => LastFailure::Status {
                        status: *status,
                        retry_after: *retry_after,
                    },
                    _ => LastFailure::Transport(message.clone()),
                });
                if !is_transient_http_error(&error) {
                    return Err(message);
                }
                last_transient = Some(message);
                backoff(attempt, attempts, budget.as_ref());
                continue;
            }
        };
        let status = response.status;
        let content_length = response
            .header("content-length")
            .and_then(|value| value.trim().parse::<u64>().ok());
        let content_range = response.header("content-range").map(str::to_string);
        let response_etag = response
            .header("etag")
            .and_then(strong_etag)
            .map(str::to_string);
        let plan = plan_response(
            offset,
            stored_etag.as_deref(),
            status,
            content_range.as_deref(),
            response_etag.as_deref(),
            content_length,
            weak_total,
        )?;
        if plan == ResponsePlan::RetryFromZero {
            cleanup_poisoned(request.part_path, &metadata_path);
            last_transient = Some("server response could not prove partial continuity".to_string());
            backoff(attempt, attempts, budget.as_ref());
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
        let mut reader: Box<dyn std::io::Read + Send> = match http.read_timeout() {
            Some(idle) => Box::new(IdleReader::new(response.into_reader(), idle)),
            None => response.into_reader(),
        };
        let (mut writer, mut digest) = stage_partial(
            request.part_path,
            request.dest_path,
            match plan {
                ResponsePlan::Append { offset, .. } => offset,
                _ => 0,
            },
        )?;
        match write_response_body_with_progress(
            request.part_path,
            plan,
            &mut writer,
            &mut digest,
            &mut *reader,
            &mut progress,
            request.cancelled,
            budget.as_ref().map(|budget| budget.deadline),
            max_bytes,
        ) {
            Ok(received) => {
                let got = match commit_staged(
                    writer,
                    digest,
                    received,
                    request.part_path,
                    &metadata_path,
                    expected.as_deref(),
                ) {
                    Ok(got) => got,
                    Err(message) if message.contains("integrity check") => {
                        cleanup_poisoned(request.part_path, &metadata_path);
                        return Err(message);
                    }
                    Err(message) => return Err(message),
                };
                let verified_total = if total == 0 { received } else { total };
                return Ok(DownloadResult {
                    received,
                    total: verified_total,
                    sha256: got,
                });
            }
            Err(TransferError::Fatal(message)) => return Err(message),
            Err(TransferError::TooLarge(message)) => {
                cleanup_poisoned(request.part_path, &metadata_path);
                return Err(message);
            }
            Err(TransferError::BudgetSpent) => {
                return Err(budget
                    .as_ref()
                    .map(|budget| budget.exceeded(slot))
                    .unwrap_or_else(|| "download exceeded its time budget".to_string()));
            }
            Err(TransferError::Transient(message)) => {
                last_transient = Some(message);
                backoff(attempt, attempts, budget.as_ref());
            }
        }
    }
    if let Some(budget) = &budget {
        if budget.spent() {
            return Err(budget.exceeded(slot));
        }
    }
    Err(last_transient.unwrap_or_else(|| "download failed after bounded retries".to_string()))
}

/// Seed only saved resume bytes. Incoming HTTP bytes never take this copy path.
fn stage_partial(
    part: &Path,
    dest: &Path,
    offset: u64,
) -> Result<(AtomicWriter, Sha256Hasher), String> {
    let mut writer = AtomicWriter::create(dest, model_payload_atomic_options())
        .map_err(|e| io_msg("create atomic destination", e))?;
    let mut digest = Sha256Hasher::new();
    if offset > 0 {
        let mut source = File::open(part).map_err(|e| io_msg("open saved partial", e))?;
        let mut copied = 0;
        let mut buffer = [0u8; 1 << 16];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|e| io_msg("read saved partial", e))?;
            if count == 0 {
                break;
            }
            copied += count as u64;
            if copied > offset {
                return Err(io_msg("resume partial changed", "saved prefix grew"));
            }
            writer
                .write_all(&buffer[..count])
                .map_err(|e| io_msg("stage saved partial", e))?;
            digest.update(&buffer[..count]);
        }
        if copied != offset {
            return Err(io_msg("resume partial changed", "saved prefix shrank"));
        }
    }
    Ok((writer, digest))
}

/// Recover a complete checkpoint after an interrupted earlier call.
fn commit_part(
    part: &Path,
    metadata: &Path,
    dest: &Path,
    expected: Option<&str>,
) -> Result<String, String> {
    let received = fs::metadata(part)
        .map_err(|e| io_msg("read completed partial", e))?
        .len();
    let (writer, digest) = stage_partial(part, dest, received)?;
    commit_staged(writer, digest, received, part, metadata, expected)
}

/// Verify the synced staged bytes immediately before publication.
fn commit_staged(
    writer: AtomicWriter,
    digest: Sha256Hasher,
    received: u64,
    part: &Path,
    metadata: &Path,
    expected: Option<&str>,
) -> Result<String, String> {
    let got = digest.finalize();
    if let Some(pin) = expected {
        if !got.eq_ignore_ascii_case(pin) {
            return Err(format!(
                "downloaded model file failed integrity check: got {got}, expected {pin}"
            ));
        }
    }
    writer
        .commit_checked(|temp| {
            let (written, length) = rightkit_fs::digest::sha256_file_with_len(temp)?;
            if written != got || length != received {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "staged model bytes changed before publication",
                ));
            }
            Ok(())
        })
        .map_err(|e| io_msg("commit atomic destination", e))?;
    let _ = fs::remove_file(part);
    let _ = fs::remove_file(metadata);
    Ok(got)
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

/// What a `.part` on disk lets the next request resume from.
struct Resume {
    /// Bytes already in the `.part`; `0` means start from zero.
    offset: u64,
    /// The saved strong ETag, sent as `If-Range`. `None` with `offset > 0` is a weak resume.
    etag: Option<String>,
    /// The total size the saved resume record was written with.
    total: u64,
}

fn resume_state(
    part: &Path,
    metadata: &Path,
    url: &str,
    expected_sha256: &Option<String>,
    weak_ok: bool,
) -> Result<Resume, String> {
    let offset = match fs::metadata(part) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(io_msg("read partial metadata", error)),
    };
    if offset == 0 {
        let _ = fs::remove_file(metadata);
        return Ok(Resume {
            offset: 0,
            etag: None,
            total: 0,
        });
    }
    let record = fs::read(metadata)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ResumeMetadata>(&bytes).ok())
        .filter(|resume| {
            resume.url == url && resume.expected_sha256.as_deref() == expected_sha256.as_deref()
        });
    if let Some(resume) = record {
        if strong_etag(&resume.etag).is_some() {
            return Ok(Resume {
                offset,
                etag: Some(resume.etag),
                total: resume.total,
            });
        }
        // Weak resume: only with the opt-in and a pin, and only when a partial is actually short.
        if weak_ok && expected_sha256.is_some() && resume.total > offset {
            return Ok(Resume {
                offset,
                etag: None,
                total: resume.total,
            });
        }
    }
    cleanup_poisoned(part, metadata);
    Ok(Resume {
        offset: 0,
        etag: None,
        total: 0,
    })
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
        Err(error) => return Err(io_msg("read partial metadata", error)),
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
    fs::write(&staged, bytes).map_err(|error| io_msg("write resume metadata", error))?;
    fs::rename(&staged, path).map_err(|error| io_msg("commit resume metadata", error))
}

fn cleanup_poisoned(part: &Path, metadata: &Path) {
    let _ = fs::remove_file(part);
    let _ = fs::remove_file(metadata);
}

fn is_transient_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..=599).contains(&status)
}

/// Transport failures are worth another attempt; a refused destination, a
/// malformed request or an unfollowed redirect are not.
fn is_transient_http_error(error: &HttpError) -> bool {
    match error {
        HttpError::Status { status, .. } => is_transient_status(*status),
        HttpError::InvalidRequest(_) | HttpError::AddressDenied { .. } | HttpError::Cancelled => {
            false
        }
        _ => true,
    }
}

/// Rough "is this a connectivity problem" classifier for offline-first-run
/// messaging — anything that is not an HTTP status response counts (DNS,
/// connect, TLS, timeout, body-read). A destination refused by the address
/// policy is not connectivity either: the network may be fine.
pub fn is_connectivity_error(message: &str) -> bool {
    !(message.starts_with(IO_PREFIX)
        || message.starts_with(CAP_PREFIX)
        || message.contains("status")
        || message.contains("address denied")
        || message.contains("(404)")
        || message.contains("integrity check")
        || message.contains("not available"))
}

/// Pause before the next attempt (250 ms, 500 ms, then 1 s), never past the total budget.
fn backoff(attempt: usize, attempts: usize, budget: Option<&Budget>) {
    if attempt + 1 < attempts {
        let mut pause = Duration::from_millis(BASE_BACKOFF_MS << attempt.min(2));
        if let Some(budget) = budget {
            pause = pause.min(budget.left());
        }
        thread::sleep(pause);
    }
}

/// Shared ureq agent from before downloads were vetted.
///
/// Nothing in this crate sends a request through it any more: the download
/// entry points that still accept an agent read its redirect limit and use a
/// vetted [`crate::http::http_client`] instead.
#[deprecated(
    since = "0.2.9",
    note = "use `rightkit_models::http::http_client`; the agent's transport is no longer used"
)]
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(crate::http::DEFAULT_MAX_REDIRECTS)
        .build()
        .into()
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    #[test]
    fn rebuildable_payload_policy_has_no_postpublication_failure_steps() {
        let options: rightkit_fs::atomic::AtomicWriteOptions =
            model_payload_atomic_options().into();
        // Durable writes can fail directory sync after replacing the target. Exclusive
        // publication can fail cleanup after linking it. Neither belongs in this API,
        // whose error result promises to preserve the previous destination.
        assert!(!options.durable);
        assert_eq!(options.replace, rightkit_fs::atomic::ReplaceMode::Rename);
    }

    fn temp_part(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rk-models-cap-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("f.part")
    }

    #[test]
    fn a_reported_total_over_the_cap_writes_nothing() {
        let part = temp_part("reported");
        let (mut writer, mut digest) =
            stage_partial(&part, &part.with_extension("bin"), 0).unwrap();
        let mut body: &[u8] = b"0123456789";
        let err = write_response_body_with_progress(
            &part,
            ResponsePlan::Replace { total: 10 },
            &mut writer,
            &mut digest,
            &mut body,
            &mut |_, _| {},
            &|| false,
            None,
            Some(5),
        )
        .unwrap_err();
        assert!(matches!(err, TransferError::TooLarge(ref m) if m.starts_with(CAP_PREFIX)));
        assert_eq!(fs::metadata(&part).unwrap().len(), 0);
        let _ = fs::remove_dir_all(part.parent().unwrap());
    }

    #[test]
    fn a_stream_past_the_cap_stops_even_without_a_reported_length() {
        let part = temp_part("streamed");
        let (mut writer, mut digest) =
            stage_partial(&part, &part.with_extension("bin"), 0).unwrap();
        let data = vec![7u8; 200_000];
        let mut body: &[u8] = &data;
        let err = write_response_body_with_progress(
            &part,
            ResponsePlan::Replace { total: 0 },
            &mut writer,
            &mut digest,
            &mut body,
            &mut |_, _| {},
            &|| false,
            None,
            Some(100_000),
        )
        .unwrap_err();
        assert!(matches!(err, TransferError::TooLarge(_)));
        assert!(fs::metadata(&part).unwrap().len() <= 100_000 + (1 << 16));
        // Within the cap: unchanged behaviour.
        drop(writer);
        let (mut writer, mut digest) =
            stage_partial(&part, &part.with_extension("bin"), 0).unwrap();
        let mut ok: &[u8] = &data[..1000];
        let n = write_response_body_with_progress(
            &part,
            ResponsePlan::Replace { total: 1000 },
            &mut writer,
            &mut digest,
            &mut ok,
            &mut |_, _| {},
            &|| false,
            None,
            Some(1000),
        )
        .unwrap();
        assert_eq!(n, 1000);
        let _ = fs::remove_dir_all(part.parent().unwrap());
    }

    #[test]
    fn disk_and_cap_messages_are_not_connectivity_errors() {
        assert!(!is_connectivity_error(&io_msg(
            "write partial",
            "disk full"
        )));
        assert!(!is_connectivity_error(&format!("{CAP_PREFIX}too big")));
        assert!(is_connectivity_error(
            "download request failed: connection reset"
        ));
    }
}

#[cfg(test)]
mod weak_resume_tests {
    use super::*;

    const PIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    const URL: &str = "https://models.example.test/f.bin";

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rk-models-weak-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_weak_resume_continues_only_at_the_same_total_and_exact_offset() {
        let ok = plan_response(
            10,
            None,
            206,
            Some("bytes 10-19/20"),
            None,
            Some(10),
            Some(20),
        );
        assert_eq!(
            ok,
            Ok(ResponsePlan::Append {
                offset: 10,
                total: 20
            })
        );
        let changed_total = plan_response(
            10,
            None,
            206,
            Some("bytes 10-19/21"),
            None,
            Some(10),
            Some(20),
        );
        assert_eq!(changed_total, Ok(ResponsePlan::RetryFromZero));
        // Without the opt-in a response with no strong ETag still restarts from zero.
        let default = plan_response(10, None, 206, Some("bytes 10-19/20"), None, Some(10), None);
        assert_eq!(default, Ok(ResponsePlan::RetryFromZero));
        let shifted = plan_response(
            10,
            None,
            206,
            Some("bytes 11-19/20"),
            None,
            Some(9),
            Some(20),
        );
        assert_eq!(shifted, Ok(ResponsePlan::RetryFromZero));
    }

    #[test]
    fn a_weak_partial_is_kept_only_when_pinned_and_opted_in() {
        let dir = temp_dir("state");
        let part = dir.join("f.part");
        let meta = metadata_path(&part);
        let record = ResumeMetadata {
            url: URL.into(),
            expected_sha256: Some(PIN.into()),
            etag: String::new(),
            total: 20,
        };
        let setup = || {
            fs::write(&part, vec![1u8; 10]).unwrap();
            fs::write(&meta, serde_json::to_vec(&record).unwrap()).unwrap();
        };
        let pinned = Some(PIN.to_string());

        setup();
        let kept = resume_state(&part, &meta, URL, &pinned, true).unwrap();
        assert_eq!((kept.offset, kept.etag.clone(), kept.total), (10, None, 20));
        assert_eq!(fs::metadata(&part).unwrap().len(), 10);

        setup();
        let not_opted_in = resume_state(&part, &meta, URL, &pinned, false).unwrap();
        assert_eq!(not_opted_in.offset, 0);
        assert!(!part.exists());

        setup();
        let unpinned = resume_state(&part, &meta, URL, &None, true).unwrap();
        assert_eq!(unpinned.offset, 0);
        assert!(!part.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
