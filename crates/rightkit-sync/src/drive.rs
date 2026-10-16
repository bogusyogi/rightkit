//! Google Drive `appDataFolder` backend (feature `drive`): the non-Apple own-cloud backend.
//!
//! [`DriveBackend`] implements [`Backend`] over the Drive v3 REST API with the
//! `https://www.googleapis.com/auth/drive.appdata` scope, on `rightkit-http`'s blocking client.
//! The data lives in the user's own Drive quota, in the hidden per-app data folder; no service of
//! ours is involved. Apple platforms use CloudKit instead; the `http` feature (the RightKit sync
//! service) is parked and `drive` replaces it for Windows, Linux and Android. Call it from the
//! engine's worker thread, never from an async task.
//!
//! # Layout
//!
//! * One **zone** is one folder named `zone` directly inside `appDataFolder`.
//! * One **record** is one small JSON file `<record name>.json` in that folder, holding the shared
//!   [`WireRecord`] encoding (the `rightkit-sync-helper/1` record object, without its
//!   `change_tag`, which Drive supplies).
//! * **Tombstones are kept**: a deletion is an ordinary record write (`deletedAt`), never a Drive
//!   delete. Drive-side removals (the user clearing the app's hidden data, a file deleted by
//!   another tool) are deliberately NOT turned into deletions on other devices; see below.
//! * The **cursor** is the Drive changes page token, wrapped so a first full listing can span
//!   pages. A first fetch (no cursor) reads `changes.getStartPageToken`, then lists the folder
//!   page by page, then continues on `changes.list` from that start token.
//! * The **change tag** of a record is `"<file id>:<file version>"`, where `version` is the Drive
//!   file `version` field.
//!
//! # No atomic compare-and-swap: the race window, exactly
//!
//! Drive v3 documents no precondition on `files.update` or `files.create`: there is no `If-Match`
//! / ETag support, no `headRevisionId` precondition and no create-only flag in the reference
//! (<https://developers.google.com/workspace/drive/api/reference/rest/v3/files/update>,
//! <https://developers.google.com/workspace/drive/api/reference/rest/v3/files/create>), and file
//! names are not unique within a folder
//! (<https://developers.google.com/workspace/drive/api/reference/rest/v3/files>). The Drive docs
//! do not document the absence of such a precondition either; this backend treats it as absent.
//! [`SavePolicy::IfServerUnchanged`] is therefore a **check, then write**, not an atomic swap:
//!
//! 1. `GET files/{id}?fields=id,version`; the saved record's tag must match, else the save is
//!    refused with `SyncError::Conflict` and the current server record, exactly as the engine
//!    expects.
//! 2. `PATCH upload/drive/v3/files/{id}` with the new content.
//!
//! **Race window.** A second writer whose `PATCH` commits between this writer's step 1 and its
//! step 2 is overwritten without notice (last `PATCH` wins). The window is one request round trip
//! plus the upload time of the record body: typically 100 ms to a few seconds, longer under
//! retries or on a slow link. The loser of that race is only the earlier of two edits made inside
//! the same window by two devices of one user on the same record name. What the engine then does:
//!
//! * A lost live edit heals on the next pass of the device that made it, because the engine
//!   compares every local item with the canonical remote record each pass and re-uploads when its
//!   conflict policy says local wins (always true for `LastWriterWins` when the lost edit was
//!   the newer one; when it was the older one, last-writer-wins would have discarded it anyway).
//!   A custom field-merging `ConflictPolicy` only sees the merged result of the two edits if both
//!   devices later pass over the record; a field edit that was never merged can be lost.
//! * A lost tombstone (a live save landing on top of a tombstone written inside the window)
//!   leaves a live record on the server. Devices that hold the tombstone in their ledger never
//!   apply or republish it, but a device with a reset or new ledger sees the record live again:
//!   the deletion is resurrected there. The deleting device already acknowledged its tombstone.
//! * Tombstones use [`SavePolicy::Overwrite`] (no check); they cannot be refused but they can be
//!   overwritten inside the window by a live save that checked just before.
//!
//! **Concurrent creation.** Two devices creating the same new record name at once can both pass
//! the "does not exist" check (list by name) and create two files with the same name. After each
//! create the backend lists the name again; the file with the smallest Drive file id is the
//! winner, the other creator deletes its own file and reports a conflict (or, for an overwrite,
//! writes its content into the winner). If Drive's list had not yet indexed the rival file, both
//! creators succeed and two files with one name exist. Readers then see both in the change feed
//! and the engine's duplicate reduction ranks them by `updatedAt`; later saves update the
//! smallest-id file. The listing consistency of Drive is not documented; this is unverified.
//! The same smallest-id rule resolves duplicate zone folders created by two devices' first
//! `prepare`.
//!
//! None of this can lose a record that no other device touched; it can only drop the older of two
//! near-simultaneous writes to the same record, or revive a deleted one on a fresh ledger.
//! A stronger design (immutable per-version files with a deterministic winner by server creation
//! time) would close most of the window but depends on Drive ordering that the docs do not
//! promise; it is left as a follow-up.
//!
//! # Errors
//!
//! | Drive response | Engine class |
//! | --- | --- |
//! | 401 (one retry after `DriveTokenSource::rejected` and a fresh token) | `Unavailable` (sign in again) |
//! | 403 `rateLimitExceeded`, `userRateLimitExceeded`, `sharingRateLimitExceeded` | `RetryAfter` (`Retry-After` or 30 s; the engine backs off exponentially on repeats) |
//! | 403 `storageQuotaExceeded` | `Unavailable` (the user's Drive is full) |
//! | 429, 408, 5xx | `RetryAfter` |
//! | 404 on a record file | the record does not exist (`Ok(None)` / recreate); on the zone folder `ZoneMissing` |
//! | 400 or 410 on a stored cursor | `TokenExpired` (full replay) |
//! | any other 4xx (including other 403) | `Unavailable`: permanent, surfaced to the user, retried on the backoff cadence |
//! | timeout, connection failure | `RetryAfter` (15 s) |
//!
//! # Notes
//!
//! * Deletions seen in the change feed (`removed`) are ignored, like in `http`, because a bare
//!   removal cannot be authenticated and would delete local data when a user clears the app's
//!   hidden Drive data. If the zone folder itself is removed, or a stored cursor belongs to a
//!   different folder than the current zone folder (cleared and recreated, or recreated by
//!   another device), the fetch reports `ZoneMissing`; the engine recreates the folder, forgets
//!   what it believed was published and republishes every local item.
//! * Per-record size is capped (`DriveConfig::max_record_bytes`, default 1 MiB). Which fields and
//!   attachments go into Drive is the app's decision (`RecordMapper`); keep audio and other large
//!   bodies out of records.
//! * Request volume: a conditional update costs two requests, a create three, and every record
//!   delivered by a fetch costs one download on top of the paging requests. Drive publishes
//!   per-user rate limits; keep `max_batch` small and rely on the engine's backoff.
//! * The access token comes from the caller ([`DriveTokenSource`]): desktop feeds it from
//!   `rightkit-oauth`'s loopback PKCE flow, Android from the Kotlin `AuthorizationClient`. This
//!   crate depends on neither.
//! * An app must run its engine with `SyncConfig::require_consent_on_scope_change` set (see
//!   [`sync_config`]) and a scope from [`account_scope`], or switching the Google account would
//!   silently publish local history into the other account.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_http::{
    form_encode, parse_retry_after, Client, ErrorClass, HttpError, Method, Request, Response,
    RetryPolicy,
};
use serde_json::{json, Value};

use crate::engine::SyncConfig;
use crate::error::{SyncError, SyncResult};
use crate::record::RawRecord;
use crate::traits::{AuthProvider, Backend, ChangePage, RecordOutcome, SavePolicy};
use crate::wire::WireRecord;

/// Production API origin (REST and media upload share this host).
pub const DEFAULT_API_BASE: &str = "https://www.googleapis.com";
/// OAuth scope the access token must carry.
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const RECORD_MIME: &str = "application/json";

/// Seconds to wait after a network failure.
const NETWORK_RETRY_SECS: f64 = 15.0;
/// Seconds to wait after a rate limit or server failure that gave no `Retry-After`.
const SERVER_RETRY_SECS: f64 = 30.0;

/// `SyncConfig` for an app that syncs through this backend: defaults plus
/// `require_consent_on_scope_change`.
pub fn sync_config() -> SyncConfig {
    SyncConfig {
        require_consent_on_scope_change: true,
        ..SyncConfig::default()
    }
}

/// `AuthProvider::account_scope` value: lowercase hex SHA-256 of
/// `rightkit-drive|<app>|<zone>|<account_id>`. `account_id` is any stable identifier of the Google
/// account the app obtained (for example the OIDC `sub`, or the Drive `permissionId` of the user).
pub fn account_scope(app: &str, zone: &str, account_id: &str) -> String {
    rightkit_fs::digest::sha256_bytes(format!("rightkit-drive|{app}|{zone}|{account_id}").as_bytes())
}

/// The caller's Google session. Desktop implements it over `rightkit-oauth`'s loopback PKCE flow
/// (refreshing the access token as it expires); Android implements it with the token the Kotlin
/// `AuthorizationClient` hands in.
pub trait DriveTokenSource: Send + Sync {
    /// A valid access token with the `drive.appdata` scope; `Ok(None)` = signed out. Called before
    /// every request, so it should be cheap and refresh transparently.
    fn access_token(&self) -> SyncResult<Option<String>>;
    /// Drive answered 401 for `token`. Drop or refresh it: the next `access_token` call is
    /// expected to return a different token. The request is retried once.
    fn rejected(&self, _token: &str) {}
    /// Stable opaque identity of the signed-in account, see [`account_scope`]. `None` = no
    /// isolation.
    fn account_scope(&self) -> Option<String> {
        None
    }
    /// Changes on sign-out or account switch.
    fn generation(&self) -> u64 {
        0
    }
}

/// A [`DriveTokenSource`] over a closure that returns the current access token.
pub struct FnToken<F>(pub F);

impl<F> DriveTokenSource for FnToken<F>
where
    F: Fn() -> SyncResult<Option<String>> + Send + Sync,
{
    fn access_token(&self) -> SyncResult<Option<String>> {
        (self.0)()
    }
}

/// `AuthProvider` over a [`DriveTokenSource`], with the user's sync toggle.
pub struct DriveAuth {
    source: Arc<dyn DriveTokenSource>,
    enabled: AtomicBool,
    epoch: AtomicU64,
}

impl DriveAuth {
    pub fn new(source: Arc<dyn DriveTokenSource>) -> Self {
        Self {
            source,
            enabled: AtomicBool::new(true),
            epoch: AtomicU64::new(0),
        }
    }

    /// The user's sync toggle. Disabling cancels an in-flight pass.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl AuthProvider for DriveAuth {
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn availability(&self) -> SyncResult<()> {
        match self.source.access_token()? {
            Some(t) if !t.is_empty() => Ok(()),
            _ => Err(SyncError::Unavailable("not signed in to Google Drive".into())),
        }
    }

    fn generation(&self) -> u64 {
        self.source
            .generation()
            .wrapping_add(self.epoch.load(Ordering::SeqCst))
    }

    fn account_scope(&self) -> Option<String> {
        self.source.account_scope()
    }

    fn bearer(&self) -> SyncResult<Option<String>> {
        self.source.access_token()
    }
}

#[derive(Debug, Clone)]
pub struct DriveConfig {
    /// API origin without a trailing path. Default [`DEFAULT_API_BASE`]; tests point it at a fake.
    pub api_base: String,
    /// Zone name: the folder inside `appDataFolder`.
    pub zone: String,
    /// Records per save call (each record costs two or three requests). Default 25.
    pub max_batch: usize,
    /// Records per change/list page, 1..=1000 (every record costs one download). Default 50.
    pub page_size: usize,
    /// Largest serialized record accepted; a bigger one fails with `SyncError::Persistence` and
    /// nothing is sent. Default 1 MiB.
    pub max_record_bytes: usize,
    /// Per-request timeout; `None` keeps the client's.
    pub request_timeout: Option<Duration>,
}

impl DriveConfig {
    pub fn new(zone: impl Into<String>) -> Self {
        Self {
            api_base: DEFAULT_API_BASE.to_string(),
            zone: zone.into(),
            max_batch: 25,
            page_size: 50,
            max_record_bytes: 1024 * 1024,
            request_timeout: None,
        }
    }
}

/// A failed request: the HTTP status (0 = none, e.g. a transport failure) and the engine class.
struct Fail {
    status: u16,
    err: SyncError,
}

impl Fail {
    fn not_found(&self) -> bool {
        self.status == 404
    }
    fn local(err: SyncError) -> Self {
        Self { status: 0, err }
    }
}

impl From<SyncError> for Fail {
    fn from(err: SyncError) -> Self {
        Fail::local(err)
    }
}

impl From<Fail> for SyncError {
    fn from(f: Fail) -> Self {
        f.err
    }
}

type DResult<T> = Result<T, Fail>;

fn zone_missing() -> Fail {
    Fail {
        status: 404,
        err: SyncError::ZoneMissing,
    }
}

fn malformed(what: &str) -> Fail {
    Fail::local(SyncError::Transient(format!(
        "malformed Google Drive response: {what}"
    )))
}

/// Percent-encode one URL path segment.
fn segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Quote a value for a Drive `q` expression: backslash and single quote are escaped
/// (<https://developers.google.com/workspace/drive/api/guides/search-files>).
fn q_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn file_name(record_name: &str) -> String {
    format!("{record_name}.json")
}

fn make_tag(id: &str, version: &str) -> String {
    format!("{id}:{version}")
}

fn parse_tag(tag: Option<&str>) -> Option<(String, String)> {
    let (id, version) = tag?.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    Some((id.to_string(), version.to_string()))
}

fn version_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct FileMeta {
    id: String,
    version: String,
}

fn meta_of(v: &Value) -> Option<FileMeta> {
    Some(FileMeta {
        id: v.get("id")?.as_str()?.to_string(),
        version: v.get("version").and_then(version_str).unwrap_or_default(),
    })
}

fn map_http_error(e: &HttpError) -> SyncError {
    match e.class() {
        ErrorClass::Timeout | ErrorClass::Transport => SyncError::RetryAfter {
            secs: NETWORK_RETRY_SECS,
            detail: e.to_string(),
        },
        ErrorClass::AddressDenied | ErrorClass::Invalid => SyncError::Unavailable(e.to_string()),
        _ => SyncError::Transient(e.to_string()),
    }
}

fn retry_after(resp: &Response) -> Option<f64> {
    resp.header("retry-after")
        .and_then(parse_retry_after)
        .map(|d| d.as_secs_f64())
        .filter(|s| s.is_finite() && *s > 0.0)
}

/// `(reason, message)` of a Google `{"error":{"message","errors":[{"reason"}]}}` body.
fn google_error(resp: &Response) -> (Option<String>, String) {
    let parsed: Option<Value> = serde_json::from_slice(&resp.body).ok();
    let err = parsed.as_ref().and_then(|v| v.get("error"));
    let reason = err
        .and_then(|e| e.get("errors"))
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find_map(|e| e.get("reason").and_then(Value::as_str))
        })
        .map(str::to_string);
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (reason, message)
}

fn fail_from_response(resp: &Response) -> Fail {
    let (reason, message) = google_error(resp);
    let mut detail = format!("Google Drive HTTP {}", resp.status);
    if let Some(r) = &reason {
        detail.push(' ');
        detail.push_str(r);
    }
    if !message.is_empty() {
        detail.push_str(": ");
        detail.push_str(&message);
    }
    let hint = retry_after(resp);
    let err = match resp.status {
        401 => SyncError::Unavailable(detail),
        403 => match reason.as_deref() {
            Some("rateLimitExceeded" | "userRateLimitExceeded" | "sharingRateLimitExceeded") => {
                SyncError::RetryAfter {
                    secs: hint.unwrap_or(SERVER_RETRY_SECS),
                    detail,
                }
            }
            // storageQuotaExceeded and every other 403 (insufficient scope, app not authorized):
            // waiting does not help; surface it, and keep the engine's backoff (Cancelled would
            // skip it and spin).
            _ => SyncError::Unavailable(detail),
        },
        404 => SyncError::Transient(detail),
        408 | 429 | 500..=599 => SyncError::RetryAfter {
            secs: hint.unwrap_or(SERVER_RETRY_SECS),
            detail,
        },
        _ => SyncError::Unavailable(detail),
    };
    Fail {
        status: resp.status,
        err,
    }
}

fn json_of(resp: &Response) -> DResult<Value> {
    resp.json::<Value>()
        .map_err(|_| malformed("body is not JSON"))
}

/// Build a `multipart/related` body (metadata part, then media part) and its boundary. The
/// boundary is chosen so that it occurs in neither part.
fn multipart_body(meta: &Value, content: &[u8]) -> (String, Vec<u8>) {
    let meta_bytes = serde_json::to_vec(meta).unwrap_or_default();
    let clashes = |b: &str| {
        let needle = b.as_bytes();
        meta_bytes.windows(needle.len()).any(|w| w == needle)
            || content.windows(needle.len()).any(|w| w == needle)
    };
    let mut n = 0u32;
    let boundary = loop {
        let b = format!("rightkit_sync_{n:04}_boundary");
        if !clashes(&b) {
            break b;
        }
        n += 1;
    };
    let mut body = Vec::with_capacity(meta_bytes.len() + content.len() + 256);
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(&meta_bytes);
    body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Type: {RECORD_MIME}\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

enum Downloaded {
    Record(RawRecord),
    /// The file vanished between listing and download.
    Gone,
    /// The file is not a readable wire record.
    Malformed,
}

enum Saved {
    Ok,
    Conflict(Option<RawRecord>),
}

/// Where a fetch continues. Encoded as JSON bytes in `ChangePage::new_token`.
///
/// Every cursor records the zone folder it was issued for. A cursor of a folder that is no longer
/// the zone folder (the user cleared the app's data, or another device recreated the folder) is
/// answered with `ZoneMissing`, not `TokenExpired`: the engine must then also forget what it
/// believes is published and republish its local items.
enum Cursor {
    /// First full listing of the zone folder; `start` is the changes token read before it began.
    List {
        token: Option<String>,
        start: Option<String>,
        folder: Option<String>,
    },
    /// `changes.list` page token.
    Changes {
        token: String,
        folder: Option<String>,
    },
}

impl Cursor {
    fn folder(&self) -> Option<&str> {
        match self {
            Cursor::List { folder, .. } | Cursor::Changes { folder, .. } => folder.as_deref(),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let v = match self {
            Cursor::List {
                token,
                start,
                folder,
            } => json!({ "v": 1, "p": "list", "t": token, "s": start, "f": folder }),
            Cursor::Changes { token, folder } => {
                json!({ "v": 1, "p": "chg", "t": token, "f": folder })
            }
        };
        v.to_string().into_bytes()
    }

    fn decode(bytes: &[u8]) -> Option<Cursor> {
        let v: Value = serde_json::from_slice(bytes).ok()?;
        if v.get("v").and_then(Value::as_i64) != Some(1) {
            return None;
        }
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        match v.get("p").and_then(Value::as_str)? {
            "list" => Some(Cursor::List {
                token: text("t"),
                start: text("s"),
                folder: text("f"),
            }),
            "chg" => Some(Cursor::Changes {
                token: text("t")?,
                folder: text("f"),
            }),
            _ => None,
        }
    }
}

pub struct DriveBackend {
    client: Client,
    tokens: Arc<dyn DriveTokenSource>,
    cfg: DriveConfig,
    origin: String,
    /// Zone folder id, resolved by `prepare`.
    folder: Mutex<Option<String>>,
    /// Record name -> file id of the last file seen or written for it.
    ids: Mutex<HashMap<String, String>>,
    malformed: AtomicU64,
}

impl DriveBackend {
    /// `client` should be built with an address policy that allows the API host, for example
    /// `ClientConfig::first_party(vec!["www.googleapis.com".into()])`.
    pub fn new(client: Client, tokens: Arc<dyn DriveTokenSource>, mut cfg: DriveConfig) -> Self {
        cfg.page_size = cfg.page_size.clamp(1, 1000);
        let origin = cfg.api_base.trim_end_matches('/').to_string();
        Self {
            client,
            tokens,
            cfg,
            origin,
            folder: Mutex::new(None),
            ids: Mutex::new(HashMap::new()),
            malformed: AtomicU64::new(0),
        }
    }

    /// Record files skipped so far because their content was not a readable wire record.
    pub fn malformed_skipped(&self) -> u64 {
        self.malformed.load(Ordering::SeqCst)
    }

    fn token(&self) -> SyncResult<String> {
        match self.tokens.access_token()? {
            Some(t) if !t.is_empty() => Ok(t),
            _ => Err(SyncError::Unavailable("not signed in to Google Drive".into())),
        }
    }

    fn url(&self, path: &str, params: &[(&str, &str)]) -> String {
        if params.is_empty() {
            format!("{}{path}", self.origin)
        } else {
            format!("{}{path}?{}", self.origin, form_encode(params))
        }
    }

    /// Send `req` with the bearer token. A 401 is retried once after `rejected`.
    fn call(&self, req: Request) -> DResult<Response> {
        let mut refreshed = false;
        loop {
            let token = self.token()?;
            let mut attempt = req
                .clone()
                .header("Accept", "application/json")
                .retry(RetryPolicy::none())
                .bearer(&token);
            if let Some(t) = self.cfg.request_timeout {
                attempt = attempt.timeout(t);
            }
            let resp = self
                .client
                .send_any(&attempt)
                .map_err(|e| Fail::local(map_http_error(&e)))?;
            if (200..300).contains(&resp.status) {
                return Ok(resp);
            }
            if resp.status == 401 && !refreshed {
                refreshed = true;
                self.tokens.rejected(&token);
                continue;
            }
            return Err(fail_from_response(&resp));
        }
    }

    // ---- caches ----

    fn cached_folder(&self) -> Option<String> {
        self.folder.lock().unwrap().clone()
    }

    fn set_folder(&self, id: &str) {
        let mut f = self.folder.lock().unwrap();
        if f.as_deref() != Some(id) {
            *f = Some(id.to_string());
            self.ids.lock().unwrap().clear();
        }
    }

    fn forget_folder(&self) {
        *self.folder.lock().unwrap() = None;
        self.ids.lock().unwrap().clear();
    }

    fn remember(&self, name: &str, id: &str) {
        self.ids
            .lock()
            .unwrap()
            .insert(name.to_string(), id.to_string());
    }

    fn forget(&self, name: &str) {
        self.ids.lock().unwrap().remove(name);
    }

    fn cached_id(&self, name: &str) -> Option<String> {
        self.ids.lock().unwrap().get(name).cloned()
    }

    // ---- zone folder ----

    /// The zone folder with the smallest id, if any.
    fn find_folder(&self) -> DResult<Option<String>> {
        let q = format!(
            "name = '{}' and mimeType = '{FOLDER_MIME}' and 'appDataFolder' in parents and trashed = false",
            q_quote(&self.cfg.zone)
        );
        let url = self.url(
            "/drive/v3/files",
            &[
                ("spaces", "appDataFolder"),
                ("q", q.as_str()),
                ("fields", "files(id)"),
                ("pageSize", "100"),
            ],
        );
        let v = json_of(&self.call(Request::get(url))?)?;
        let mut ids: Vec<String> = v
            .get("files")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        Ok(ids.into_iter().next())
    }

    fn ensure_folder(&self) -> DResult<String> {
        if let Some(id) = self.find_folder()? {
            self.set_folder(&id);
            return Ok(id);
        }
        let url = self.url("/drive/v3/files", &[("fields", "id")]);
        let req = Request::post(url)
            .json(&json!({
                "name": self.cfg.zone,
                "mimeType": FOLDER_MIME,
                "parents": ["appDataFolder"],
            }))
            .map_err(|e| Fail::local(SyncError::Persistence(e.to_string())))?;
        let v = json_of(&self.call(req)?)?;
        let mine = v
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("folder id"))?
            .to_string();
        // Two devices preparing at once: the smallest id wins, the other removes its empty folder.
        match self.find_folder()? {
            Some(first) if first != mine => {
                let _ = self.delete(&mine);
                self.set_folder(&first);
                Ok(first)
            }
            _ => {
                self.set_folder(&mine);
                Ok(mine)
            }
        }
    }

    /// The resolved zone folder; `ZoneMissing` when `prepare` found none.
    fn folder_id(&self) -> DResult<String> {
        if let Some(id) = self.cached_folder() {
            return Ok(id);
        }
        match self.find_folder()? {
            Some(id) => {
                self.set_folder(&id);
                Ok(id)
            }
            None => Err(zone_missing()),
        }
    }

    // ---- files ----

    /// Live files named after `record_name` in the zone folder, smallest id first.
    fn find_files(&self, record_name: &str) -> DResult<Vec<FileMeta>> {
        let folder = self.folder_id()?;
        let q = format!(
            "name = '{}' and '{}' in parents and trashed = false",
            q_quote(&file_name(record_name)),
            q_quote(&folder)
        );
        let url = self.url(
            "/drive/v3/files",
            &[
                ("spaces", "appDataFolder"),
                ("q", q.as_str()),
                ("fields", "files(id,version)"),
                ("pageSize", "100"),
            ],
        );
        let v = match self.call(Request::get(url)) {
            Ok(r) => json_of(&r)?,
            Err(f) if f.not_found() => {
                self.forget_folder();
                return Err(zone_missing());
            }
            Err(f) => return Err(f),
        };
        let mut files: Vec<FileMeta> = v
            .get("files")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(meta_of).collect())
            .unwrap_or_default();
        files.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(files)
    }

    fn get_meta(&self, id: &str) -> DResult<FileMeta> {
        let url = self.url(
            &format!("/drive/v3/files/{}", segment(id)),
            &[("fields", "id,version,trashed")],
        );
        let v = json_of(&self.call(Request::get(url))?)?;
        if v.get("trashed").and_then(Value::as_bool) == Some(true) {
            return Err(Fail {
                status: 404,
                err: SyncError::Transient("file is trashed".into()),
            });
        }
        meta_of(&v).ok_or_else(|| malformed("file metadata"))
    }

    fn download(&self, id: &str, version: &str) -> DResult<Downloaded> {
        let url = self.url(
            &format!("/drive/v3/files/{}", segment(id)),
            &[("alt", "media")],
        );
        let resp = match self.call(Request::get(url)) {
            Ok(r) => r,
            Err(f) if f.not_found() => return Ok(Downloaded::Gone),
            Err(f) => return Err(f),
        };
        let parsed = serde_json::from_slice::<WireRecord>(&resp.body)
            .map_err(|e| e.to_string())
            .and_then(WireRecord::into_raw);
        match parsed {
            Ok(mut rec) => {
                rec.change_tag = Some(make_tag(id, version));
                self.remember(&rec.record_name, id);
                Ok(Downloaded::Record(rec))
            }
            Err(_) => {
                self.malformed.fetch_add(1, Ordering::SeqCst);
                Ok(Downloaded::Malformed)
            }
        }
    }

    fn server_record(&self, meta: &FileMeta) -> DResult<Option<RawRecord>> {
        Ok(match self.download(&meta.id, &meta.version)? {
            Downloaded::Record(r) => Some(r),
            Downloaded::Gone | Downloaded::Malformed => None,
        })
    }

    fn create(&self, folder: &str, name: &str, body: &[u8]) -> DResult<FileMeta> {
        let meta = json!({
            "name": name,
            "mimeType": RECORD_MIME,
            "parents": [folder],
        });
        let (boundary, payload) = multipart_body(&meta, body);
        let url = self.url(
            "/upload/drive/v3/files",
            &[("uploadType", "multipart"), ("fields", "id,version")],
        );
        let req = Request::post(url)
            .header(
                "Content-Type",
                format!("multipart/related; boundary={boundary}"),
            )
            .body(payload);
        let v = json_of(&self.call(req)?)?;
        meta_of(&v).ok_or_else(|| malformed("created file"))
    }

    fn update(&self, id: &str, name: &str, body: &[u8]) -> DResult<FileMeta> {
        let meta = json!({ "name": name, "mimeType": RECORD_MIME });
        let (boundary, payload) = multipart_body(&meta, body);
        let url = self.url(
            &format!("/upload/drive/v3/files/{}", segment(id)),
            &[("uploadType", "multipart"), ("fields", "id,version")],
        );
        let req = Request::new(Method::Patch, url)
            .header(
                "Content-Type",
                format!("multipart/related; boundary={boundary}"),
            )
            .body(payload);
        let v = json_of(&self.call(req)?)?;
        meta_of(&v).ok_or_else(|| malformed("updated file"))
    }

    fn delete(&self, id: &str) -> DResult<()> {
        let url = self.url(&format!("/drive/v3/files/{}", segment(id)), &[]);
        self.call(Request::delete(url)).map(|_| ())
    }

    fn start_page_token(&self) -> DResult<String> {
        let url = self.url(
            "/drive/v3/changes/startPageToken",
            &[("fields", "startPageToken")],
        );
        let v = json_of(&self.call(Request::get(url))?)?;
        v.get("startPageToken")
            .and_then(version_str)
            .ok_or_else(|| malformed("startPageToken"))
    }

    // ---- saving ----

    fn encode_record(&self, rec: &RawRecord) -> DResult<Vec<u8>> {
        if rec.record_name.is_empty() {
            return Err(Fail::local(SyncError::Persistence(
                "record has no name".into(),
            )));
        }
        let mut w = WireRecord::from(rec);
        w.change_tag = None;
        w.modified_ms = None;
        let bytes = serde_json::to_vec(&w)
            .map_err(|e| Fail::local(SyncError::Persistence(e.to_string())))?;
        if bytes.len() > self.cfg.max_record_bytes {
            return Err(Fail::local(SyncError::Persistence(format!(
                "record `{}` is {} bytes, over the {} byte cap",
                rec.record_name,
                bytes.len(),
                self.cfg.max_record_bytes
            ))));
        }
        Ok(bytes)
    }

    fn save_one(&self, rec: &RawRecord, policy: SavePolicy) -> DResult<Saved> {
        let body = self.encode_record(rec)?;
        match policy {
            SavePolicy::Overwrite => self.write_overwrite(rec, &body),
            SavePolicy::IfServerUnchanged => self.write_conditional(rec, &body),
        }
    }

    /// Create the record file, then re-list the name: the smallest file id wins; a losing
    /// creator deletes its own file.
    fn create_checked(&self, rec: &RawRecord, body: &[u8], policy: SavePolicy) -> DResult<Saved> {
        let name = rec.record_name.as_str();
        let fname = file_name(name);
        let folder = self.folder_id()?;
        let created = match self.create(&folder, &fname, body) {
            Ok(m) => m,
            Err(f) if f.not_found() => {
                self.forget_folder();
                return Err(zone_missing());
            }
            Err(f) => return Err(f),
        };
        let all = self.find_files(name)?;
        match all.first() {
            Some(first) if first.id != created.id => {
                let _ = self.delete(&created.id);
                match policy {
                    SavePolicy::IfServerUnchanged => {
                        let server = self.server_record(first)?;
                        Ok(Saved::Conflict(server))
                    }
                    SavePolicy::Overwrite => {
                        self.update(&first.id, &fname, body)?;
                        self.remember(name, &first.id);
                        Ok(Saved::Ok)
                    }
                }
            }
            _ => {
                self.remember(name, &created.id);
                Ok(Saved::Ok)
            }
        }
    }

    fn write_overwrite(&self, rec: &RawRecord, body: &[u8]) -> DResult<Saved> {
        let name = rec.record_name.as_str();
        let fname = file_name(name);
        let mut candidate = parse_tag(rec.change_tag.as_deref())
            .map(|(id, _)| id)
            .or_else(|| self.cached_id(name));
        for _ in 0..2 {
            let id = match candidate.take() {
                Some(id) => Some(id),
                None => self.find_files(name)?.into_iter().next().map(|m| m.id),
            };
            match id {
                Some(id) => match self.update(&id, &fname, body) {
                    Ok(_) => {
                        self.remember(name, &id);
                        return Ok(Saved::Ok);
                    }
                    Err(f) if f.not_found() => {
                        self.forget(name);
                    }
                    Err(f) => return Err(f),
                },
                None => return self.create_checked(rec, body, SavePolicy::Overwrite),
            }
        }
        self.create_checked(rec, body, SavePolicy::Overwrite)
    }

    fn write_conditional(&self, rec: &RawRecord, body: &[u8]) -> DResult<Saved> {
        let name = rec.record_name.as_str();
        let fname = file_name(name);
        match (&rec.change_tag, parse_tag(rec.change_tag.as_deref())) {
            (Some(_), None) => Err(Fail::local(SyncError::Transient(format!(
                "record `{name}` carries a change tag that is not a Drive tag"
            )))),
            (Some(_), Some((id, version))) => match self.get_meta(&id) {
                Ok(meta) if meta.version == version => match self.update(&id, &fname, body) {
                    Ok(_) => {
                        self.remember(name, &id);
                        Ok(Saved::Ok)
                    }
                    Err(f) if f.not_found() => {
                        self.forget(name);
                        Ok(Saved::Conflict(None))
                    }
                    Err(f) => Err(f),
                },
                Ok(meta) => {
                    let server = self.server_record(&meta)?;
                    Ok(Saved::Conflict(server))
                }
                Err(f) if f.not_found() => {
                    // The file this write was based on is gone. Report the file that now carries
                    // the name, if any; with none the engine re-reads and recreates next pass.
                    self.forget(name);
                    let server = match self.find_files(name)?.first() {
                        Some(m) => self.server_record(m)?,
                        None => None,
                    };
                    Ok(Saved::Conflict(server))
                }
                Err(f) => Err(f),
            },
            (None, _) => {
                if let Some(first) = self.find_files(name)?.first() {
                    return match self.download(&first.id, &first.version)? {
                        Downloaded::Record(r) => Ok(Saved::Conflict(Some(r))),
                        Downloaded::Malformed => Err(Fail::local(SyncError::Persistence(format!(
                            "the Drive file for record `{name}` is not a readable record; not overwritten"
                        )))),
                        // Vanished while we looked: nothing stands in the way of a create.
                        Downloaded::Gone => self.create_checked(rec, body, SavePolicy::IfServerUnchanged),
                    };
                }
                self.create_checked(rec, body, SavePolicy::IfServerUnchanged)
            }
        }
    }

    // ---- fetching ----

    fn push_download(&self, out: &mut Vec<RawRecord>, meta: &FileMeta) -> DResult<()> {
        if let Downloaded::Record(r) = self.download(&meta.id, &meta.version)? {
            out.push(r);
        }
        Ok(())
    }

    fn list_page(
        &self,
        folder: &str,
        token: Option<String>,
        start: Option<String>,
    ) -> SyncResult<ChangePage> {
        let start = match start {
            Some(s) => s,
            None => self.start_page_token()?,
        };
        let q = format!("'{}' in parents and trashed = false", q_quote(folder));
        let size = self.cfg.page_size.to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("spaces", "appDataFolder"),
            ("q", q.as_str()),
            ("pageSize", size.as_str()),
            ("fields", "nextPageToken,files(id,version,mimeType)"),
        ];
        if let Some(t) = &token {
            params.push(("pageToken", t.as_str()));
        }
        let resp = match self.call(Request::get(self.url("/drive/v3/files", &params))) {
            Ok(r) => r,
            Err(f) if f.not_found() => {
                self.forget_folder();
                return Err(SyncError::ZoneMissing);
            }
            Err(f) if token.is_some() && (f.status == 400 || f.status == 410) => {
                return Err(SyncError::TokenExpired)
            }
            Err(f) => return Err(f.err),
        };
        let v = json_of(&resp)?;
        let files = v
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("files"))?;
        let mut records = Vec::new();
        for f in files {
            if f.get("mimeType").and_then(Value::as_str) == Some(FOLDER_MIME) {
                continue;
            }
            if let Some(meta) = meta_of(f) {
                self.push_download(&mut records, &meta)?;
            }
        }
        let next = v
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        let folder = Some(folder.to_string());
        let (cursor, more) = match next {
            Some(t) => (
                Cursor::List {
                    token: Some(t),
                    start: Some(start),
                    folder,
                },
                true,
            ),
            None => (
                Cursor::Changes {
                    token: start,
                    folder,
                },
                false,
            ),
        };
        Ok(ChangePage {
            records,
            deleted_record_names: Vec::new(),
            new_token: Some(cursor.encode()),
            more_coming: more,
        })
    }

    fn change_page(&self, folder: &str, token: &str) -> SyncResult<ChangePage> {
        let size = self.cfg.page_size.to_string();
        let url = self.url(
            "/drive/v3/changes",
            &[
                ("pageToken", token),
                ("spaces", "appDataFolder"),
                ("includeRemoved", "true"),
                ("pageSize", size.as_str()),
                (
                    "fields",
                    "nextPageToken,newStartPageToken,changes(fileId,removed,file(id,version,mimeType,parents,trashed))",
                ),
            ],
        );
        let resp = match self.call(Request::get(url)) {
            Ok(r) => r,
            Err(f) if f.status == 400 || f.status == 410 => return Err(SyncError::TokenExpired),
            Err(f) => return Err(f.err),
        };
        let v = json_of(&resp)?;
        let changes = v
            .get("changes")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("changes"))?;
        // Newest change per file id, in feed order.
        let mut order: Vec<String> = Vec::new();
        let mut latest: HashMap<String, FileMeta> = HashMap::new();
        for c in changes {
            let Some(file_id) = c.get("fileId").and_then(Value::as_str) else {
                continue;
            };
            if c.get("removed").and_then(Value::as_bool) == Some(true) {
                if file_id == folder {
                    self.forget_folder();
                    return Err(SyncError::ZoneMissing);
                }
                // A removed record file is not a deletion (see the module docs).
                latest.remove(file_id);
                continue;
            }
            let Some(file) = c.get("file") else { continue };
            let in_zone = file
                .get("parents")
                .and_then(Value::as_array)
                .is_some_and(|p| p.iter().any(|x| x.as_str() == Some(folder)));
            let trashed = file.get("trashed").and_then(Value::as_bool) == Some(true);
            let is_folder = file.get("mimeType").and_then(Value::as_str) == Some(FOLDER_MIME);
            if !in_zone || trashed || is_folder {
                continue;
            }
            if let Some(meta) = meta_of(file) {
                if !latest.contains_key(file_id) {
                    order.push(file_id.to_string());
                }
                latest.insert(file_id.to_string(), meta);
            }
        }
        let mut records = Vec::new();
        for id in &order {
            if let Some(meta) = latest.get(id) {
                self.push_download(&mut records, meta)?;
            }
        }
        let (token, more) = match v.get("nextPageToken").and_then(version_str) {
            Some(t) => (t, true),
            None => (
                v.get("newStartPageToken")
                    .and_then(version_str)
                    .ok_or_else(|| malformed("newStartPageToken"))?,
                false,
            ),
        };
        Ok(ChangePage {
            records,
            deleted_record_names: Vec::new(),
            new_token: Some(
                Cursor::Changes {
                    token,
                    folder: Some(folder.to_string()),
                }
                .encode(),
            ),
            more_coming: more,
        })
    }
}

impl Backend for DriveBackend {
    fn prepare(&self) -> SyncResult<()> {
        self.ensure_folder().map(|_| ()).map_err(SyncError::from)
    }

    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        let cursor = match since {
            None => Cursor::List {
                token: None,
                start: None,
                folder: None,
            },
            // A cursor that did not come from this backend: replay.
            Some(bytes) => Cursor::decode(bytes).ok_or(SyncError::TokenExpired)?,
        };
        let folder = self.folder_id()?;
        if cursor.folder().is_some_and(|f| f != folder) {
            return Err(SyncError::ZoneMissing);
        }
        match cursor {
            Cursor::List { token, start, .. } => self.list_page(&folder, token, start),
            Cursor::Changes { token, .. } => self.change_page(&folder, &token),
        }
    }

    fn fetch_record(&self, record_name: &str) -> SyncResult<Option<RawRecord>> {
        let files = self.find_files(record_name)?;
        let Some(first) = files.first() else {
            return Ok(None);
        };
        Ok(self.server_record(first)?)
    }

    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>> {
        let mut out: Vec<RecordOutcome> = Vec::with_capacity(records.len());
        let mut abort: Option<SyncError> = None;
        for (i, r) in records.iter().enumerate() {
            let name = r.record_name.clone();
            if let Some(e) = &abort {
                out.push(RecordOutcome {
                    record_name: name,
                    result: Err(e.clone()),
                    server_record: None,
                });
                continue;
            }
            match self.save_one(r, policy) {
                Ok(Saved::Ok) => out.push(RecordOutcome {
                    record_name: name,
                    result: Ok(()),
                    server_record: None,
                }),
                Ok(Saved::Conflict(server)) => out.push(RecordOutcome {
                    record_name: name,
                    result: Err(SyncError::Conflict),
                    server_record: server,
                }),
                Err(f) => {
                    // Errors about the session or the service stop the batch; the first record
                    // failing that way fails the whole call, as nothing was written.
                    let call_level = matches!(
                        f.err,
                        SyncError::RetryAfter { .. }
                            | SyncError::Unavailable(_)
                            | SyncError::ZoneMissing
                            | SyncError::Cancelled
                    );
                    if call_level {
                        if i == 0 {
                            return Err(f.err);
                        }
                        abort = Some(f.err.clone());
                    }
                    out.push(RecordOutcome {
                        record_name: name,
                        result: Err(f.err),
                        server_record: None,
                    });
                }
            }
        }
        Ok(out)
    }

    fn max_batch(&self) -> usize {
        self.cfg.max_batch.max(1)
    }
}
