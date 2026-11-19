//! Resumable, hash-pinned archive download.
//!
//! The transfer engine (range resume with strong-ETag continuity, bounded
//! retries, authorization rejection, integrity check, `.part` + `.meta`
//! bookkeeping) is `rightkit_models::download`. This module keeps the
//! The app call shape: the verified archive lands next to its `.part` file
//! (see [`archive_path_for`]) and a completed archive survives a restart
//! without touching the network until the caller discards it.

use rightkit_models::download as rk;
use rightkit_models::http::{
    http_client_with_options, AsHttpClient, ModelHttp, ModelHttpOptions, TrustRoots,
};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Largest `max_bytes` a download request may carry: 16 GiB. A host passes the real size of the
/// artifact, or a tight ceiling above it; a larger value is refused, not clamped.
pub const MAX_DOWNLOAD_BYTES_CEILING: u64 = 16 << 30;

pub struct DownloadRequest<'a> {
    pub url: &'a str,
    /// Scratch `.part` file; its sibling [`archive_path_for`] receives the verified archive.
    pub part_path: &'a Path,
    pub expected_sha256: &'a str,
    pub authorization: Option<&'a str>,
    /// Most bytes the archive may have, `1..=`[`MAX_DOWNLOAD_BYTES_CEILING`]. The download fails
    /// before any byte is written when the server reports more, and while streaming when it sends
    /// more than it reported; the `.part` is then removed. A completed archive already on disk
    /// that is larger is discarded, not hashed.
    pub max_bytes: u64,
}

pub struct RefreshingDownloadRequest<'a> {
    pub url: &'a str,
    pub part_path: &'a Path,
    pub expected_sha256: &'a str,
    /// See [`DownloadRequest::max_bytes`].
    pub max_bytes: u64,
}

pub struct DownloadResult {
    pub received: u64,
    pub total: u64,
    /// Verified archive, ready to install. Remove with [`discard_partial`].
    pub archive_path: PathBuf,
}

/// Final location of the verified archive for `part_path`: the same name without
/// its `.part` suffix (`<name>.tar.gz.part` -> `<name>.tar.gz`).
pub fn archive_path_for(part_path: &Path) -> PathBuf {
    match part_path.to_str().and_then(|p| p.strip_suffix(".part")) {
        Some(stem) if !stem.is_empty() => PathBuf::from(stem),
        _ => {
            let mut value = part_path.as_os_str().to_os_string();
            value.push(".verified");
            PathBuf::from(value)
        }
    }
}

fn valid_pin(pin: &str) -> bool {
    pin.len() == 64 && pin.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// User agent the former `ureq` agents sent (`ureq/<version>`); same literal as
/// `rightkit_dictation_telemetry::http_client`.
const FORMER_USER_AGENT: &str = "ureq/3.4.2";

/// Vetted download transport matching the former `ureq` agents: public-web address policy,
/// environment proxy, ten redirects, bundled (webpki) trust roots, the `ureq` user agent, one
/// `total` budget for the whole call, and no automatic retry. Connect, header and idle limits
/// equal `total`, so nothing fails sooner than the single former budget.
pub fn download_client(total: Duration) -> ModelHttp {
    client_with_limits(total, total, total, total)
}

/// Longest wait for a connection, for response headers and for the next body bytes of a
/// streaming download ([`streaming_download_client`]).
const STREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_HEADER_TIMEOUT: Duration = Duration::from_secs(60);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest one streaming download may take in all: six hours.
pub const STREAMING_DOWNLOAD_BUDGET: Duration = Duration::from_secs(6 * 60 * 60);

/// As [`download_client`] for a transfer whose size the caller bounds itself (the shared store's
/// download, which the store caps at the catalog's pinned archive size): the same address policy,
/// proxy, redirects and trust roots, but with the per-phase limits a stalled transfer needs
/// instead of one budget. A connection that takes longer than 30 s, headers that take longer than
/// 60 s, or 120 s without a body byte ends the attempt, and the whole download ends at
/// [`STREAMING_DOWNLOAD_BUDGET`]. Before, this client had no limit of any kind.
pub fn streaming_download_client() -> ModelHttp {
    client_with_limits(
        STREAM_CONNECT_TIMEOUT,
        STREAM_HEADER_TIMEOUT,
        STREAM_IDLE_TIMEOUT,
        STREAMING_DOWNLOAD_BUDGET,
    )
}

fn client_with_limits(
    connect: Duration,
    header: Duration,
    read: Duration,
    total: Duration,
) -> ModelHttp {
    let options = ModelHttpOptions {
        user_agent: Some(FORMER_USER_AGENT.to_string()),
        trust: TrustRoots::Bundled,
        connect_timeout: Some(connect),
        header_timeout: Some(header),
        read_timeout: Some(read),
        total_timeout: Some(total),
        retries: 0,
        ..Default::default()
    };
    http_client_with_options(with_dev_worker(options))
}

/// Debug builds with the non-default `dev-worker-override` feature may reach the loopback
/// worker named by `RIGHTKIT_MODEL_WORKER_BASE`; every other build returns `options` unchanged and
/// loopback stays refused.
#[cfg(feature = "dev-worker-override")]
fn with_dev_worker(options: ModelHttpOptions) -> ModelHttpOptions {
    match dev_worker_base().as_deref().and_then(worker_host_port) {
        Some((host, port)) => options.allow_loopback_host(host, port),
        None => options,
    }
}

#[cfg(not(feature = "dev-worker-override"))]
fn with_dev_worker(options: ModelHttpOptions) -> ModelHttpOptions {
    options
}

/// The development worker base of a build with the non-default `dev-worker-override` feature.
#[cfg(feature = "dev-worker-override")]
fn dev_worker_base() -> Option<String> {
    std::env::var("RIGHTKIT_MODEL_WORKER_BASE")
        .ok()
        .or_else(|| option_env!("RIGHTKIT_MODEL_WORKER_BASE").map(str::to_string))
}

/// Lower-case host (no brackets, no port) of the development worker, in a build with the
/// non-default `dev-worker-override` feature when `RIGHTKIT_MODEL_WORKER_BASE` names one; `None`
/// in every other build and when none is named. The grant request ([`crate::model_download_grant`])
/// lifts the public-address policy for this host and for no other.
#[cfg(feature = "dev-worker-override")]
pub(crate) fn dev_worker_host() -> Option<String> {
    let (host, _) = dev_worker_base().as_deref().and_then(worker_host_port)?;
    Some(
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase(),
    )
}

#[cfg(not(feature = "dev-worker-override"))]
pub(crate) fn dev_worker_host() -> Option<String> {
    None
}

/// Host and port of an `http(s)://host[:port]` worker base; the crate itself refuses anything
/// that is not a plain host name or IP literal.
#[cfg(feature = "dev-worker-override")]
fn worker_host_port(base: &str) -> Option<(String, u16)> {
    let (scheme, rest) = base.trim().split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    match authority.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']')?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().ok()?,
                None if after.is_empty() => default_port,
                None => return None,
            };
            Some((format!("[{host}]"), port))
        }
        None => match authority.rsplit_once(':') {
            Some((host, p)) => Some((host.to_string(), p.parse().ok()?)),
            None => Some((authority.to_string(), default_port)),
        },
    }
}

pub fn download_resumable(
    agent: &impl AsHttpClient,
    request: DownloadRequest<'_>,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, String> {
    download_resumable_until(agent, request, progress, &|| false)
}

/// [`download_resumable`] that stops when `cancelled` returns `true`. The transfer engine
/// (`rightkit_models::download::DownloadRequest::cancelled`) polls it before the first request
/// and between body chunks (64 KiB), so a stop is seen within one chunk read, which the client's
/// own read and total limits bound. A stop returns the message `download cancelled` and keeps
/// the `.part` and its resume record for a later call; the verified-archive shortcut and the
/// size and checksum checks above it run first and are not interruptible (they are bounded by
/// `max_bytes`).
pub fn download_resumable_until(
    agent: &impl AsHttpClient,
    request: DownloadRequest<'_>,
    progress: impl FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<DownloadResult, String> {
    download_resumable_inner(agent, request, progress, cancelled, None)
}

/// [`download_resumable_until`] that also ends by the absolute `deadline`, which is passed down
/// as an [`Instant`] and never turned into a duration here: the transfer engine
/// (`rightkit_models::download::download_resumable_capped_by`) derives each request's timeout,
/// each pause, each wait for body bytes and the check before each chunk from it when that step
/// starts. A deadline already passed returns the message `download exceeded the caller's
/// deadline` before the verified-archive check and before any request. The file-system work
/// around the transfer is not interruptible (it is bounded by `max_bytes`).
pub fn download_resumable_by(
    agent: &impl AsHttpClient,
    request: DownloadRequest<'_>,
    progress: impl FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<DownloadResult, String> {
    download_resumable_inner(agent, request, progress, cancelled, Some(deadline))
}

fn download_resumable_inner(
    agent: &impl AsHttpClient,
    request: DownloadRequest<'_>,
    progress: impl FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
    deadline: Option<Instant>,
) -> Result<DownloadResult, String> {
    if request.max_bytes == 0 || request.max_bytes > MAX_DOWNLOAD_BYTES_CEILING {
        return Err("download size limit is out of range".to_string());
    }
    let expected = request.expected_sha256.trim().to_ascii_lowercase();
    if !valid_pin(&expected) {
        discard_partial(request.part_path);
        return Err("download checksum must be a 64-character SHA-256".to_string());
    }
    if deadline.is_some_and(|limit| Instant::now() >= limit) {
        return Err("download exceeded the caller's deadline".to_string());
    }
    let archive_path = archive_path_for(request.part_path);

    // A fully verified archive from a run that died before install needs no network.
    if let Some(length) = verified_archive_length(&archive_path, &expected, request.max_bytes) {
        return Ok(DownloadResult {
            received: length,
            total: length,
            archive_path,
        });
    }

    // The transfer is capped at `max_bytes`: the size the server reports is checked before a
    // byte is written, and the bytes received while streaming are checked again.
    let transfer = rk::DownloadRequest {
        url: request.url,
        part_path: request.part_path,
        dest_path: &archive_path,
        expected_sha256: Some(&expected),
        authorization: request.authorization,
        cancelled,
    };
    let result = match deadline {
        Some(deadline) => rk::download_resumable_capped_by(
            agent,
            transfer,
            Some(request.max_bytes),
            deadline,
            progress,
        ),
        None => rk::download_resumable_capped(agent, transfer, Some(request.max_bytes), progress),
    }
    .map_err(|error| error.message)?;
    Ok(DownloadResult {
        received: result.received,
        total: result.total,
        archive_path,
    })
}

/// The length of the archive at `path` when it is a regular file of at most `max_bytes` bytes
/// whose SHA-256 is `expected`. Anything else at that name (a link, a directory, a file that is
/// too large or does not hash to the pin) is removed through its pinned parent and `None` is
/// returned. One handle serves the length, the hash and the decision: the file is not measured
/// and then opened again, and no more than `max_bytes + 1` bytes are ever read.
fn verified_archive_length(path: &Path, expected: &str, max_bytes: u64) -> Option<u64> {
    let file = match crate::confined_remove::open_file_at(path) {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(_) => {
            let _ = crate::confined_remove::remove_file_at(path);
            return None;
        }
    };
    let length = file.metadata().map(|metadata| metadata.len()).ok();
    if let Some(length) = length.filter(|length| *length <= max_bytes) {
        let hashed =
            rightkit_fs::digest::sha256_reader(&mut (&file).take(max_bytes.saturating_add(1)));
        if hashed.is_ok_and(|got| got.eq_ignore_ascii_case(expected)) {
            return Some(length);
        }
    }
    drop(file);
    let _ = crate::confined_remove::remove_file_at(path);
    None
}

pub fn download_resumable_with_authorization_refresh(
    agent: &impl AsHttpClient,
    request: RefreshingDownloadRequest<'_>,
    mut authorize: impl FnMut() -> Result<String, String>,
    mut progress: impl FnMut(u64, u64),
) -> Result<DownloadResult, String> {
    let first_authorization = authorize()?;
    let first = download_resumable(
        agent,
        DownloadRequest {
            url: request.url,
            part_path: request.part_path,
            expected_sha256: request.expected_sha256,
            authorization: Some(&first_authorization),
            max_bytes: request.max_bytes,
        },
        &mut progress,
    );
    if !matches!(&first, Err(message) if is_authorization_rejection(message)) {
        return first;
    }

    let refreshed_authorization = authorize()?;
    download_resumable(
        agent,
        DownloadRequest {
            url: request.url,
            part_path: request.part_path,
            expected_sha256: request.expected_sha256,
            authorization: Some(&refreshed_authorization),
            max_bytes: request.max_bytes,
        },
        &mut progress,
    )
}

fn is_authorization_rejection(message: &str) -> bool {
    message.starts_with("download authorization rejected (")
}

/// Remove the partial, its resume record and the verified archive.
pub fn discard_partial(part: &Path) {
    rk::discard_partial(part);
    let _ = crate::confined_remove::remove_file_at(&archive_path_for(part));
}
