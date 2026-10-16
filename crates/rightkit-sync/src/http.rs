//! Blocking HTTP backend for the `rightkit-sync-http/1` protocol (feature `http`).
//!
//! The protocol is specified in `HTTP-PROTOCOL.md` in this crate and implemented server-side
//! by `packages/sync-worker`. The `Backend` and `AuthProvider` traits are synchronous, so
//! this uses `rightkit-http`'s blocking client (ureq with the platform certificate verifier).
//! Call it from the engine's worker thread, never from an async task.
//!
//! # Plaintext never leaves
//!
//! The wire record carries only `name`, `type`, `updated_at`, `deleted_at`, `change_tag` and
//! one opaque `enc` body. [`HttpBackend`] refuses to send any other field: wrap it in
//! `cipher::EncryptingBackend` (feature `cipher`), which moves every domain field into `enc`.
//! A record with a plaintext field, or without `updatedAt`, fails the save with
//! `SyncError::Persistence` and nothing is sent.
//!
//! # Consent on account change
//!
//! An app syncing encrypted history through this backend must run its engine with
//! `SyncConfig::require_consent_on_scope_change` set (see [`sync_config`]) and a scope from
//! [`account_scope`] (which includes the key id). Otherwise a switch of account or key
//! silently resets the ledger and publishes local history into the new account.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rightkit_http::{parse_retry_after, Client, ErrorClass, HttpError, Request, Response, RetryPolicy};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::engine::SyncConfig;
use crate::error::{SyncError, SyncResult};
use crate::record::{field, FieldValue, RawRecord};
use crate::traits::{AuthProvider, Backend, ChangePage, RecordOutcome, SavePolicy};
use crate::wire::{b64_decode, b64_encode};

/// Protocol identifier, returned by `GET /v1/sync/health`.
pub const PROTOCOL: &str = "rightkit-sync-http/1";

/// The sealed-body field (`cipher::ENC_FIELD`), also the wire name.
const ENC_FIELD: &str = "enc";

/// Seconds to wait after a network failure (same as the helper protocol).
const NETWORK_RETRY_SECS: f64 = 15.0;
/// Seconds to wait after a server-side failure that gave no `Retry-After`.
const SERVER_RETRY_SECS: f64 = 30.0;

/// `SyncConfig` for an app that syncs through this backend: defaults plus
/// `require_consent_on_scope_change`, which HttpBackend users must set.
pub fn sync_config() -> SyncConfig {
    SyncConfig {
        require_consent_on_scope_change: true,
        ..SyncConfig::default()
    }
}

/// `AuthProvider::account_scope` value: lowercase hex SHA-256 of
/// `rightkit-cloud|<app>|<zone>|<account_id>|<key_id>`. A key rotation or an account switch
/// changes it, which resets (or, with consent required, pauses) the ledger.
pub fn account_scope(app: &str, zone: &str, account_id: &str, key_id: &str) -> String {
    rightkit_fs::digest::sha256_bytes(
        format!("rightkit-cloud|{app}|{zone}|{account_id}|{key_id}").as_bytes(),
    )
}

/// The app's session: the sync bearer token (refreshed by the app as it expires) and the
/// account identity.
pub trait TokenSource: Send + Sync {
    /// Current sync token; `Ok(None)` = signed out.
    fn bearer(&self) -> SyncResult<Option<String>>;
    /// Stable opaque identity, see [`account_scope`]. `None` = no isolation.
    fn account_scope(&self) -> Option<String>;
    /// Changes on sign-out or account switch.
    fn generation(&self) -> u64 {
        0
    }
}

/// `AuthProvider` over a [`TokenSource`], with the user's sync toggle.
pub struct HttpAuth {
    source: Arc<dyn TokenSource>,
    enabled: AtomicBool,
    epoch: AtomicU64,
}

impl HttpAuth {
    pub fn new(source: Arc<dyn TokenSource>) -> Self {
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

impl AuthProvider for HttpAuth {
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn availability(&self) -> SyncResult<()> {
        match self.source.bearer()? {
            Some(t) if !t.is_empty() => Ok(()),
            _ => Err(SyncError::Unavailable(
                "not signed in to the sync server".into(),
            )),
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
        self.source.bearer()
    }
}

#[derive(Debug, Clone)]
pub struct HttpBackendConfig {
    /// Server origin, e.g. `https://sync.example.com` (no trailing path).
    pub base_url: String,
    /// Zone name, `[A-Za-z0-9._-]{1,64}`.
    pub zone: String,
    /// Records per save call. Default 40, the `max_save_records` of a server on the Cloudflare
    /// free plan (a paid-plan server accepts up to 100, the protocol maximum). A server
    /// advertises its own cap as `max_save_records` in the health response; `health()` does
    /// not read it, so set this to at most that value when the server has a lower one.
    pub max_batch: usize,
    /// Records per changes page (the server caps it at 200).
    pub page_limit: usize,
    /// Per-request timeout; `None` keeps the client's.
    pub request_timeout: Option<Duration>,
}

impl HttpBackendConfig {
    pub fn new(base_url: impl Into<String>, zone: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            zone: zone.into(),
            max_batch: 40,
            page_limit: 200,
            request_timeout: None,
        }
    }
}

pub struct HttpBackend {
    client: Client,
    tokens: Arc<dyn TokenSource>,
    cfg: HttpBackendConfig,
    origin: String,
    zone_url: String,
}

/// Percent-encode one URL path segment (unreserved characters stay; everything else,
/// including `+`, becomes `%XX`).
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

#[derive(Deserialize)]
struct WireRec {
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    #[serde(default)]
    change_tag: Option<String>,
    updated_at: i64,
    #[serde(default)]
    deleted_at: Option<i64>,
    #[serde(default)]
    enc: Option<String>,
}

fn malformed(what: &str) -> SyncError {
    SyncError::Transient(format!("malformed sync server response: {what}"))
}

fn record_from_wire(v: &Value) -> SyncResult<RawRecord> {
    let w: WireRec = serde_json::from_value(v.clone()).map_err(|_| malformed("record"))?;
    let mut rec = RawRecord::new(&w.record_type, &w.name);
    rec.change_tag = w.change_tag;
    rec.set(field::UPDATED_AT, FieldValue::Date(w.updated_at));
    if let Some(d) = w.deleted_at {
        rec.set(field::DELETED_AT, FieldValue::Date(d));
    }
    if let Some(enc) = w.enc {
        let bytes = b64_decode(&enc).map_err(|_| malformed("enc"))?;
        rec.set(ENC_FIELD, FieldValue::Bytes(bytes));
    }
    Ok(rec)
}

fn record_to_wire(r: &RawRecord) -> SyncResult<Value> {
    let mut updated: Option<i64> = None;
    let mut deleted: Option<i64> = None;
    let mut enc: Option<String> = None;
    for (key, value) in &r.fields {
        match (key.as_str(), value) {
            (field::UPDATED_AT, FieldValue::Date(n)) => updated = Some(*n),
            (field::DELETED_AT, FieldValue::Date(n)) => deleted = Some(*n),
            (ENC_FIELD, FieldValue::Bytes(b)) => enc = Some(b64_encode(b)),
            _ => {
                return Err(SyncError::Persistence(format!(
                    "HttpBackend sends sealed records only; field `{key}` is plaintext \
                     (wrap the backend in EncryptingBackend)"
                )))
            }
        }
    }
    let updated = updated.ok_or_else(|| {
        SyncError::Persistence(format!("record `{}` has no updatedAt date", r.record_name))
    })?;
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), json!(r.record_name));
    obj.insert("type".into(), json!(r.record_type));
    obj.insert("updated_at".into(), json!(updated));
    if let Some(d) = deleted {
        obj.insert("deleted_at".into(), json!(d));
    }
    if let Some(e) = enc {
        obj.insert("enc".into(), json!(e));
    }
    if let Some(tag) = &r.change_tag {
        obj.insert("change_tag".into(), json!(tag));
    }
    Ok(Value::Object(obj))
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

/// `(code, message)` of an `{"error":{"code","message"}}` body.
fn error_body(resp: &Response) -> (Option<String>, String) {
    let parsed: Option<Value> = serde_json::from_slice(&resp.body).ok();
    let err = parsed.as_ref().and_then(|v| v.get("error"));
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (code, message)
}

fn error_from_response(resp: &Response) -> SyncError {
    let (code, message) = error_body(resp);
    let detail = match &code {
        Some(c) if message.is_empty() => format!("HTTP {} {c}", resp.status),
        Some(c) => format!("HTTP {} {c}: {message}", resp.status),
        None => format!("HTTP {}", resp.status),
    };
    let hint = retry_after(resp);
    if code.as_deref() == Some("token_expired") || resp.status == 410 {
        return SyncError::TokenExpired;
    }
    if code.as_deref() == Some("zone_missing") {
        return SyncError::ZoneMissing;
    }
    match resp.status {
        429 => SyncError::RetryAfter {
            secs: hint.unwrap_or(SERVER_RETRY_SECS),
            detail,
        },
        // 401 (token missing, invalid, expired, revoked): the app must sign in again.
        // 403 (`app_not_allowed`, `zone_limit`): not retryable by waiting, but `Cancelled`
        // would skip the engine's backoff and spin, so it is reported as unavailable.
        401 | 403 => SyncError::Unavailable(detail),
        404 => SyncError::ZoneMissing,
        502..=504 => SyncError::RetryAfter {
            secs: hint.unwrap_or(SERVER_RETRY_SECS),
            detail,
        },
        _ => match hint {
            Some(secs) => SyncError::RetryAfter { secs, detail },
            None => SyncError::Transient(detail),
        },
    }
}

fn outcome_error(code: &str, message: &str) -> SyncError {
    let detail = format!("{code}: {message}");
    match code {
        "quota_exceeded" => SyncError::Unavailable(detail),
        "blob_store_unavailable" | "blob_store_error" => SyncError::RetryAfter {
            secs: SERVER_RETRY_SECS,
            detail,
        },
        _ => SyncError::Transient(detail),
    }
}

impl HttpBackend {
    pub fn new(client: Client, tokens: Arc<dyn TokenSource>, cfg: HttpBackendConfig) -> Self {
        let origin = cfg.base_url.trim_end_matches('/').to_string();
        let zone_url = format!("{origin}/v1/sync/zones/{}", segment(&cfg.zone));
        Self {
            client,
            tokens,
            cfg,
            origin,
            zone_url,
        }
    }

    fn token(&self) -> SyncResult<String> {
        match self.tokens.bearer()? {
            Some(t) if !t.is_empty() => Ok(t),
            _ => Err(SyncError::Unavailable(
                "not signed in to the sync server".into(),
            )),
        }
    }

    fn send(&self, req: Request, authenticated: bool) -> SyncResult<Response> {
        let mut req = req
            .header("Accept", "application/json")
            .retry(RetryPolicy::none());
        if authenticated {
            req = req.bearer(&self.token()?);
        }
        if let Some(t) = self.cfg.request_timeout {
            req = req.timeout(t);
        }
        let resp = self.client.send_any(&req).map_err(|e| map_http_error(&e))?;
        if (200..300).contains(&resp.status) {
            Ok(resp)
        } else {
            Err(error_from_response(&resp))
        }
    }

    fn post_json(&self, url: String, body: &Value) -> SyncResult<Response> {
        let req = Request::post(url)
            .json(body)
            .map_err(|e| SyncError::Persistence(e.to_string()))?;
        self.send(req, true)
    }

    fn json_body(resp: &Response) -> SyncResult<Value> {
        resp.json::<Value>().map_err(|_| malformed("body is not JSON"))
    }

    /// `GET /v1/sync/health` (no token): confirms the server speaks this protocol.
    pub fn health(&self) -> SyncResult<()> {
        let resp = self.send(Request::get(format!("{}/v1/sync/health", self.origin)), false)?;
        let body = Self::json_body(&resp)?;
        if body.get("protocol").and_then(Value::as_str) == Some(PROTOCOL) {
            Ok(())
        } else {
            Err(SyncError::Unavailable(
                "server does not speak rightkit-sync-http/1".into(),
            ))
        }
    }

    /// `DELETE /v1/sync/account`: erase everything the server holds for the signed-in account
    /// (all zones). Irreversible; the app must confirm with the user first.
    pub fn erase_account(&self) -> SyncResult<()> {
        let req = Request::delete(format!("{}/v1/sync/account", self.origin))
            .header("x-rightkit-confirm", "erase-account");
        self.send(req, true).map(|_| ())
    }
}

impl Backend for HttpBackend {
    fn prepare(&self) -> SyncResult<()> {
        self.post_json(self.zone_url.clone(), &json!({})).map(|_| ())
    }

    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        let mut url = format!("{}/changes?limit={}", self.zone_url, self.cfg.page_limit);
        if let Some(bytes) = since {
            // A cursor that is not text did not come from this protocol: replay.
            let Ok(text) = std::str::from_utf8(bytes) else {
                return Err(SyncError::TokenExpired);
            };
            url.push_str("&since=");
            url.push_str(&rightkit_http::percent_encode(text));
        }
        let resp = self.send(Request::get(url), true)?;
        let body = Self::json_body(&resp)?;
        let mut records = Vec::new();
        for v in body
            .get("records")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("records"))?
        {
            records.push(record_from_wire(v)?);
        }
        // `deleted_record_names` is deliberately ignored: in this protocol a deletion is a
        // tombstone record, which `EncryptingBackend` authenticates. A bare name list cannot
        // be authenticated, so honouring it would let a server delete records on clients.
        Ok(ChangePage {
            records,
            deleted_record_names: Vec::new(),
            new_token: body
                .get("new_token")
                .and_then(Value::as_str)
                .map(|s| s.as_bytes().to_vec()),
            more_coming: body
                .get("more_coming")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn fetch_record(&self, record_name: &str) -> SyncResult<Option<RawRecord>> {
        let url = format!("{}/records/{}", self.zone_url, segment(record_name));
        let resp = self.send(Request::get(url), true)?;
        let body = Self::json_body(&resp)?;
        match body.get("record") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => Ok(Some(record_from_wire(v)?)),
        }
    }

    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let policy = match policy {
            SavePolicy::Overwrite => "overwrite",
            SavePolicy::IfServerUnchanged => "if_server_unchanged",
        };
        let wire = records
            .iter()
            .map(record_to_wire)
            .collect::<SyncResult<Vec<Value>>>()?;
        let resp = self.post_json(
            format!("{}/save", self.zone_url),
            &json!({ "policy": policy, "records": wire }),
        )?;
        let body = Self::json_body(&resp)?;
        let outcomes = body
            .get("outcomes")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("outcomes"))?;
        let mut out = Vec::with_capacity(outcomes.len());
        for o in outcomes {
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("outcome name"))?
                .to_string();
            let status = o.get("status").and_then(Value::as_str).unwrap_or("");
            let (result, server_record) = match status {
                "ok" => (Ok(()), None),
                "conflict" => {
                    let server = match o.get("server_record") {
                        None | Some(Value::Null) => None,
                        Some(v) => Some(record_from_wire(v)?),
                    };
                    (Err(SyncError::Conflict), server)
                }
                "error" => {
                    let code = o.get("code").and_then(Value::as_str).unwrap_or("error");
                    let message = o.get("message").and_then(Value::as_str).unwrap_or("");
                    (Err(outcome_error(code, message)), None)
                }
                _ => return Err(malformed("outcome status")),
            };
            out.push(RecordOutcome {
                record_name: name,
                result,
                server_record,
            });
        }
        Ok(out)
    }

    fn max_batch(&self) -> usize {
        self.cfg.max_batch.max(1)
    }
}
