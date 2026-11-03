//! ENG-55: Git LFS batch-protocol HTTPS client.
//!
//! `git2` (even with `vendored-libgit2`) exposes no LFS transfer API — LFS
//! is a separate Git extension layered on top of plain HTTPS, not something
//! libgit2's smart-HTTP transport speaks. This module implements the batch
//! protocol directly:
//!
//! 1. POST `{remote}.git/info/lfs/objects/batch` with
//!    `{"operation":"download","transfers":["basic"],"objects":[{"oid","size"},...]}`.
//! 2. Parse each response object's `actions.download.href`/`header`/
//!    `expires_at` (or its `error`, which fails that object).
//! 3. Stream each object to a temp file inside the destination objects
//!    root, hard-capped at the pointer's declared size, verify its SHA-256
//!    against the requested oid, then atomically move it into
//!    `<objects_root>/<oid[0:2]>/<oid[2:4]>/<oid>`. Any size or hash
//!    mismatch is a hard error and leaves no partial object behind — the
//!    temp file is dropped (and thus deleted) on every failure path before
//!    `persist` is ever called.
//!
//! Only HTTPS remotes are supported; LFS over SSH remotes is out of scope
//! here (see `batch_endpoint`).
//!
//! Reuses the bounded-response / 15s-timeout / redirect-refusal pattern
//! from `cloudflare.rs` for the batch metadata call, and the
//! `Arc<AtomicBool>` cooperative-cancellation pattern fed by `ctx.cancel_rx`
//! from `git_fetch.rs::callbacks` for the object download. The token is
//! never logged: it is attached only as a request header (`bearer_auth` for
//! the GitHub-hosted batch call, or whatever `header` map the server itself
//! returned for the signed per-object download href) and every error
//! message that carries a URL is passed through
//! `crate::coding_tools::output_compaction::mask_secrets`.

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Batch metadata responses are a small JSON list of signed actions, never
/// the object bytes themselves; bound it generously but firmly, the same
/// way `cloudflare.rs` bounds its API responses.
const MAX_BATCH_RESPONSE_BYTES: usize = 1024 * 1024;
const BATCH_METADATA_TIMEOUT: Duration = Duration::from_secs(15);
/// Objects can legitimately be large, so only the connection stall is
/// capped here; the real bound on total transfer is each object's declared
/// pointer size, enforced byte-by-byte while streaming in
/// `download_and_verify`.
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct BatchObjectRequest {
    pub oid: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct FetchedObject {
    pub oid: String,
    pub size: u64,
    /// Final path under `<objects_root>/<oid[0:2]>/<oid[2:4]>/<oid>`.
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
struct BatchRequestBody<'a> {
    operation: &'a str,
    transfers: [&'a str; 1],
    objects: Vec<BatchRequestObjectWire<'a>>,
}

#[derive(Debug, Serialize)]
struct BatchRequestObjectWire<'a> {
    oid: &'a str,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct BatchResponseBody {
    #[serde(default)]
    objects: Vec<BatchResponseObject>,
}

#[derive(Debug, Deserialize)]
struct BatchResponseObject {
    oid: String,
    #[serde(default)]
    #[allow(dead_code)]
    size: u64,
    #[serde(default)]
    actions: Option<BatchActions>,
    #[serde(default)]
    error: Option<BatchObjectError>,
}

#[derive(Debug, Deserialize)]
struct BatchActions {
    download: Option<BatchDownloadAction>,
}

#[derive(Debug, Deserialize)]
struct BatchDownloadAction {
    href: String,
    #[serde(default)]
    header: HashMap<String, String>,
    #[serde(default)]
    #[allow(dead_code)]
    expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BatchObjectError {
    code: u32,
    message: String,
}

/// Build the LFS batch endpoint for an HTTPS remote per the batch-api spec:
/// `<remote url, forced to end in .git>/info/lfs/objects/batch`. LFS over
/// SSH remotes is refused — the host app only speaks the HTTPS batch protocol.
fn batch_endpoint(remote_url: &str) -> Result<url::Url, String> {
    if !remote_url.starts_with("https://") {
        return Err(format!(
            "LFS batch transfer requires an HTTPS remote; '{}' is not HTTPS.",
            crate::coding_tools::output_compaction::mask_secrets(remote_url)
        ));
    }
    let base = remote_url.trim_end_matches('/');
    let base = if base.ends_with(".git") {
        base.to_string()
    } else {
        format!("{base}.git")
    };
    url::Url::parse(&format!("{base}/info/lfs/objects/batch"))
        .map_err(|error| format!("Could not build LFS batch URL: {error}"))
}

/// Mirrors `git_fetch.rs::callbacks`'s GitHub-host gate: never attach the
/// token to a non-GitHub batch endpoint.
fn github_bearer(remote_url: &str, github_token: Option<&str>) -> Option<String> {
    github_token
        .filter(|_| {
            remote_url.starts_with("https://github.com/")
                || remote_url.starts_with("https://api.github.com/")
        })
        .map(str::to_owned)
}

/// Cooperative cancellation flag driven by `ctx.cancel_rx`, matching the
/// pattern in `git_fetch.rs::callbacks`.
fn spawn_cancel_flag(cancel_rx: Option<tokio::sync::watch::Receiver<bool>>) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Some(mut rx) = cancel_rx {
        let flag = flag.clone();
        tokio::spawn(async move {
            if rx.changed().await.is_ok() && *rx.borrow() {
                flag.store(true, Ordering::SeqCst);
            }
        });
    }
    flag
}

/// POST the batch request and return the server's per-object entries. Never
/// logs the token: it is attached only via `bearer_auth` on this one
/// request.
async fn request_batch(
    remote_url: &str,
    objects: &[BatchObjectRequest],
    github_token: Option<&str>,
) -> Result<Vec<BatchResponseObject>, String> {
    let url = batch_endpoint(remote_url)?;
    let client = crate::coding_tools::http_client::client_for(url.as_str(), BATCH_METADATA_TIMEOUT)
        .map_err(|error| format!("Could not create LFS batch client: {error}"))?;
    let body = BatchRequestBody {
        operation: "download",
        transfers: ["basic"],
        objects: objects
            .iter()
            .map(|object| BatchRequestObjectWire {
                oid: &object.oid,
                size: object.size,
            })
            .collect(),
    };
    let mut request = rightkit_http::Request::post(url.as_str())
        .header("Accept", "application/vnd.git-lfs+json")
        .max_response_bytes(MAX_BATCH_RESPONSE_BYTES)
        .json(&body)
        .map_err(|error| format!("LFS batch request failed: {error}"))?
        .set_header("Content-Type", "application/vnd.git-lfs+json");
    if let Some(token) = github_bearer(remote_url, github_token) {
        request = request.bearer(&token);
    }
    let response = client
        .send_any(&request)
        .await
        .map_err(|error| match error {
            rightkit_http::HttpError::BodyTooLarge { .. } => {
                "LFS batch response exceeds size limit.".to_string()
            }
            other => format!("LFS batch request failed: {other}"),
        })?;
    let status = response.status;
    let bytes = response.body;
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(&bytes);
        return Err(format!(
            "LFS batch request returned HTTP {}: {}",
            status, text
        ));
    }
    let parsed: BatchResponseBody = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not parse LFS batch response: {error}"))?;
    Ok(parsed.objects)
}

/// A sha256 oid is always exactly 64 lowercase hex characters. Rejecting
/// anything else up front is what keeps the `<oid[0:2]>/<oid[2:4]>/<oid>`
/// path construction below safe against a malformed/hostile server
/// response.
fn is_valid_sha256_oid(oid: &str) -> bool {
    oid.len() == 64 && oid.chars().all(|c| c.is_ascii_hexdigit())
}

/// Stream one object to a temp file inside `objects_root`, capped at
/// `expected_size` bytes, verify its SHA-256 against `oid`, then move it
/// into `objects_root/<oid[0:2]>/<oid[2:4]>/<oid>`. A size or hash mismatch
/// is a hard error and leaves no partial object behind — the `NamedTempFile`
/// is only ever `persist`-ed on the success path, so every early return
/// simply drops (and thereby deletes) it.
async fn download_and_verify(
    client: &rightkit_http::AsyncClient,
    action: &BatchDownloadAction,
    oid: &str,
    expected_size: u64,
    objects_root: &Path,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<PathBuf, String> {
    if !is_valid_sha256_oid(oid) {
        return Err(format!("Refusing to fetch malformed LFS oid '{oid}'."));
    }
    std::fs::create_dir_all(objects_root)
        .map_err(|error| format!("Could not create LFS objects dir: {error}"))?;

    // The cancel flag is shared with the transfer: a cancelled flag stops the call
    // before it is sent and a quiet body stream within a poll interval.
    let mut request = rightkit_http::Request::get(&action.href)
        .header("Accept-Encoding", "identity")
        .cancel(rightkit_http::CancelToken::from_flag(cancel_flag.clone()));
    for (name, value) in &action.header {
        // Signed-URL headers may carry credentials; keep them out of error text.
        request = request.secret_header(name.as_str(), value.as_str());
    }
    let response = client
        .stream_any(&request)
        .await
        .map_err(|error| match error {
            rightkit_http::HttpError::Cancelled => {
                format!("LFS download of {oid} was cancelled.")
            }
            other => format!("LFS object download failed for {oid}: {other}"),
        })?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "LFS object download for {oid} returned HTTP {}",
            response.status
        ));
    }
    if response
        .header("content-length")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .is_some_and(|length| length != expected_size)
    {
        return Err(format!(
            "LFS object {oid}: server-declared length does not match pointer size {expected_size}."
        ));
    }
    let mut body = response.into_stream();

    let dest = objects_root.join(&oid[0..2]).join(&oid[2..4]).join(oid);
    // Verified before commit; dropping the writer on any early return removes
    // the temp file. Rebuildable cache, so no fsync.
    let mut temp = rightkit_fs::AtomicWriter::create(
        &dest,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            durable: false,
            ..rightkit_fs::AtomicOptions::default()
        },
    )
    .map_err(|error| format!("Could not create temp file for LFS object {oid}: {error}"))?;
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    let mut written: u64 = 0;
    loop {
        if cancel_flag.load(Ordering::SeqCst) {
            return Err(format!("LFS download of {oid} was cancelled."));
        }
        match body.next().await {
            Some(Ok(chunk)) => {
                written += chunk.len() as u64;
                if written > expected_size {
                    return Err(format!(
                        "LFS object {oid} exceeded its declared size {expected_size} bytes; aborting."
                    ));
                }
                hasher.update(&chunk);
                temp.write_all(&chunk)
                    .map_err(|error| format!("Could not write LFS object {oid}: {error}"))?;
            }
            None => break,
            Some(Err(rightkit_http::HttpError::Cancelled)) => {
                return Err(format!("LFS download of {oid} was cancelled."))
            }
            Some(Err(error)) => {
                return Err(format!(
                    "LFS object download for {oid} failed mid-stream: {error}"
                ))
            }
        }
    }
    if written != expected_size {
        return Err(format!(
            "LFS object {oid}: downloaded {written} bytes, expected exactly {expected_size}."
        ));
    }
    let digest = hasher.finalize();
    if digest != oid {
        return Err(format!(
            "LFS object hash mismatch: expected {oid}, got {digest}. Refusing to store it."
        ));
    }

    temp.commit()
        .map_err(|error| format!("Could not finalize LFS object {oid}: {error}"))?;
    Ok(dest)
}

/// Fetch and verify every requested object against `remote_url`, writing
/// each into `objects_root`. A batch-level failure (transport, auth,
/// malformed response, an unresolved oid the server itself rejected) fails
/// the whole call. Returns one `FetchedObject` per oid that was actually
/// downloaded and verified — the caller (`git_lfs.rs::lfs_fetch`) maps this
/// back onto the originally requested pointer paths.
pub async fn fetch_objects(
    remote_url: &str,
    objects: &[BatchObjectRequest],
    objects_root: &Path,
    github_token: Option<&str>,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<Vec<FetchedObject>, String> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    let batch = request_batch(remote_url, objects, github_token).await?;
    let cancel_flag = spawn_cancel_flag(cancel_rx);
    // Only the connect/header stall is capped; total transfer is bounded by each
    // object's declared size, enforced while streaming. The remote's own host is
    // the one host allowed to resolve privately; redirects are never followed.
    let download_client =
        crate::coding_tools::http_client::client_for(remote_url, DOWNLOAD_CONNECT_TIMEOUT)
            .map_err(|error| format!("Could not create LFS download client: {error}"))?;

    let requested: HashMap<&str, u64> = objects
        .iter()
        .map(|object| (object.oid.as_str(), object.size))
        .collect();

    let mut fetched = Vec::with_capacity(objects.len());
    for entry in &batch {
        let Some(&expected_size) = requested.get(entry.oid.as_str()) else {
            // Server echoed an object we never asked for; ignore it rather
            // than trusting server-declared identity for anything we did
            // not request.
            continue;
        };
        if let Some(error) = &entry.error {
            return Err(format!(
                "LFS server refused object {}: {} (code {})",
                entry.oid, error.message, error.code
            ));
        }
        let Some(actions) = &entry.actions else {
            return Err(format!(
                "LFS server returned no actions for object {}.",
                entry.oid
            ));
        };
        let Some(download) = &actions.download else {
            return Err(format!(
                "LFS server returned no download action for object {}.",
                entry.oid
            ));
        };
        if cancel_flag.load(Ordering::SeqCst) {
            return Err("LFS fetch was cancelled.".to_string());
        }
        let path = download_and_verify(
            &download_client,
            download,
            &entry.oid,
            expected_size,
            objects_root,
            &cancel_flag,
        )
        .await?;
        fetched.push(FetchedObject {
            oid: entry.oid.clone(),
            size: expected_size,
            path,
        });
    }
    Ok(fetched)
}
