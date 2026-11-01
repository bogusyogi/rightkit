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
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct DownloadRequest<'a> {
    pub url: &'a str,
    /// Scratch `.part` file; its sibling [`archive_path_for`] receives the verified archive.
    pub part_path: &'a Path,
    pub expected_sha256: &'a str,
    pub authorization: Option<&'a str>,
}

pub struct RefreshingDownloadRequest<'a> {
    pub url: &'a str,
    pub part_path: &'a Path,
    pub expected_sha256: &'a str,
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
    client_with_limit(Some(total))
}

/// As [`download_client`] with no connect, header, idle or total limit: the former default
/// `ureq` agent (`store::http_agent`) set none.
pub fn unlimited_download_client() -> ModelHttp {
    client_with_limit(None)
}

fn client_with_limit(total: Option<Duration>) -> ModelHttp {
    let options = ModelHttpOptions {
        user_agent: Some(FORMER_USER_AGENT.to_string()),
        trust: TrustRoots::Bundled,
        connect_timeout: total,
        header_timeout: total,
        read_timeout: total,
        total_timeout: total,
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
    let base = std::env::var("RIGHTKIT_MODEL_WORKER_BASE")
        .ok()
        .or_else(|| option_env!("RIGHTKIT_MODEL_WORKER_BASE").map(str::to_string));
    match base.as_deref().and_then(worker_host_port) {
        Some((host, port)) => options.allow_loopback_host(host, port),
        None => options,
    }
}

#[cfg(not(feature = "dev-worker-override"))]
fn with_dev_worker(options: ModelHttpOptions) -> ModelHttpOptions {
    options
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
    let expected = request.expected_sha256.trim().to_ascii_lowercase();
    if !valid_pin(&expected) {
        discard_partial(request.part_path);
        return Err("download checksum must be a 64-character SHA-256".to_string());
    }
    let archive_path = archive_path_for(request.part_path);

    // A fully verified archive from a run that died before install needs no network.
    if let Ok(length) = fs::metadata(&archive_path).map(|metadata| metadata.len()) {
        match rightkit_models::hash::sha256_file(&archive_path) {
            Ok(got) if got.eq_ignore_ascii_case(&expected) => {
                return Ok(DownloadResult {
                    received: length,
                    total: length,
                    archive_path,
                });
            }
            _ => {
                let _ = fs::remove_file(&archive_path);
            }
        }
    }

    let result = rk::download_resumable(
        agent,
        rk::DownloadRequest {
            url: request.url,
            part_path: request.part_path,
            dest_path: &archive_path,
            expected_sha256: Some(&expected),
            authorization: request.authorization,
            cancelled: &|| false,
        },
        progress,
    )
    .map_err(|error| error.message)?;
    Ok(DownloadResult {
        received: result.received,
        total: result.total,
        archive_path,
    })
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
    let _ = fs::remove_file(archive_path_for(part));
}

#[cfg(test)]
#[path = "resumable_download_tests.rs"]
mod tests;
