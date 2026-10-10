//! The transport-free daemon core. [`Daemon::handle`] takes a fully read
//! request and returns a response; the axum adapter and the tests both drive
//! exactly this function, so there is one authorisation path.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rightkit_contracts::types::daemon_api::AuthClassId;
use ring::hmac;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde_json::{json, Value};

use crate::config::DaemonConfig;
use crate::events::EventLog;
use crate::routes::{match_route, Matched, RouteDef};
use crate::signing::{
    canonical_request, H_NONCE, H_PROVIDER_SIGNATURE, H_PROVIDER_TIMESTAMP, H_SIGNATURE,
    H_TIMESTAMP,
};
use crate::state::{Device, EnrolCode, IdemRecord, InboxItem, Job, State, Upload, UploadDone};
use crate::util::{atomic_write, ct_eq, hex, random_bytes, sha256_hex, unhex, Clock};

/// A fully read request. Header names are lower-case.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    pub path_and_query: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
    fn path(&self) -> &str {
        self.path_and_query.split('?').next().unwrap_or("")
    }
    fn query(&self) -> &str {
        self.path_and_query.split_once('?').map_or("", |(_, q)| q)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    /// `Value::Null` means an empty body.
    pub body: Value,
    pub headers: Vec<(String, String)>,
}

impl Response {
    fn new(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
        }
    }
    fn err(status: u16, code: &str) -> Self {
        Self::new(status, json!({ "error": code }))
    }
    fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

/// Outcome of delegating an OAuth callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthOutcome {
    Completed,
    /// State unknown, expired, replayed, denied by the user, or wrong provider.
    Rejected,
    /// Provider or storage failure; the browser may retry from the start.
    Unavailable,
}

/// Completes an OAuth authorization-code callback. The shipped implementation
/// wraps `rightkit-oauth`'s server flow ([`crate::oauth::FlowOAuthHandler`]).
pub trait OAuthCallbackHandler: Send + Sync {
    fn complete(
        &self,
        provider: &str,
        code: Option<String>,
        state: Option<String>,
        error: Option<String>,
    ) -> OAuthOutcome;
}

struct Inner {
    state: State,
    uploads: BTreeMap<String, Upload>,
}

pub struct Daemon {
    cfg: DaemonConfig,
    clock: Arc<dyn Clock>,
    events: Arc<EventLog>,
    oauth: Option<Arc<dyn OAuthCallbackHandler>>,
    inner: Mutex<Inner>,
}

type Guard<'a> = MutexGuard<'a, Inner>;

const MAX_ID: usize = 128;

impl Daemon {
    /// Open (or create) the daemon's durable state under `cfg.state_dir`.
    pub fn open(
        cfg: DaemonConfig,
        clock: Arc<dyn Clock>,
        oauth: Option<Arc<dyn OAuthCallbackHandler>>,
    ) -> std::io::Result<Self> {
        fs::create_dir_all(cfg.state_dir.join("uploads"))?;
        fs::create_dir_all(cfg.state_dir.join("objects"))?;
        let events = Arc::new(EventLog::open(&cfg.state_dir.join("events.jsonl"))?);
        let state_path = cfg.state_dir.join("state.json");
        let state: State = match fs::read(&state_path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e),
        };
        let mut uploads = BTreeMap::new();
        for entry in fs::read_dir(cfg.state_dir.join("uploads"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let up: Upload = serde_json::from_slice(&fs::read(&path)?)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            uploads.insert(up.id.clone(), up);
        }
        let d = Self {
            cfg,
            clock,
            events,
            oauth,
            inner: Mutex::new(Inner { state, uploads }),
        };
        d.recover()?;
        Ok(d)
    }

    /// Finish interrupted commits and drop expired or unbacked sessions.
    fn recover(&self) -> std::io::Result<()> {
        let now = self.clock.now_secs();
        let mut g = self.lock();
        let ids: Vec<String> = g.uploads.keys().cloned().collect();
        for id in ids {
            let part = self.part_path(&id);
            let obj = self.object_path(&id);
            let up = g.uploads.get_mut(&id).expect("listed above");
            if up.complete.is_some() {
                if !obj.exists() && part.exists() {
                    fs::rename(&part, &obj)?;
                }
            } else if now > up.expires_at {
                let _ = fs::remove_file(&part);
                let _ = fs::remove_file(self.meta_path(&id));
                g.uploads.remove(&id);
            } else if !part.exists() && !up.received.is_empty() {
                up.received.clear();
                let snapshot = up.clone();
                self.save_upload(&snapshot)?;
            }
        }
        Ok(())
    }

    fn lock(&self) -> Guard<'_> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> &DaemonConfig {
        &self.cfg
    }

    pub fn events(&self) -> Vec<Value> {
        self.events.snapshot()
    }

    pub fn emit(&self, event: &str, fields: Value) {
        self.events.emit(self.clock.now_secs(), event, fields);
    }

    /// Path of a committed object. Present only after `UPLOAD_COMPLETE`.
    pub fn object_path(&self, upload_id: &str) -> PathBuf {
        self.cfg.state_dir.join("objects").join(upload_id)
    }
    fn part_path(&self, id: &str) -> PathBuf {
        self.cfg
            .state_dir
            .join("uploads")
            .join(format!("{id}.part"))
    }
    fn meta_path(&self, id: &str) -> PathBuf {
        self.cfg
            .state_dir
            .join("uploads")
            .join(format!("{id}.json"))
    }

    // ---- administration (in-process, never exposed on a route) -------------

    /// Issue a single-use enrolment code. Only its hash is stored.
    pub fn issue_enrolment_code(&self, scopes: &[&str], ttl_secs: u64) -> std::io::Result<String> {
        let code = format!("rk-{}", hex(&random_bytes::<20>()));
        let mut g = self.lock();
        g.state.enrol_codes.insert(
            sha256_hex(code.as_bytes()),
            EnrolCode {
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                expires_at: self.clock.now_secs() + ttl_secs,
            },
        );
        self.save(&g)?;
        Ok(code)
    }

    pub fn revoke_device(&self, device_id: &str) -> std::io::Result<bool> {
        let mut g = self.lock();
        let Some(d) = g.state.devices.get_mut(device_id) else {
            return Ok(false);
        };
        d.revoked = true;
        self.save(&g)?;
        drop(g);
        self.emit("DEVICE_REVOKED", json!({ "deviceId": device_id }));
        Ok(true)
    }

    pub fn device_ids(&self) -> Vec<String> {
        self.lock().state.devices.keys().cloned().collect()
    }

    /// Take accepted webhook deliveries for processing: `(id, provider, body)`.
    pub fn drain_webhooks(&self) -> std::io::Result<Vec<(String, String, Vec<u8>)>> {
        let mut g = self.lock();
        let items: Vec<InboxItem> = std::mem::take(&mut g.state.webhook_inbox);
        self.save(&g)?;
        Ok(items
            .into_iter()
            .map(|i| {
                (
                    i.id,
                    i.provider,
                    STANDARD.decode(i.body_b64).unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Current revision of a job (0 when it does not exist).
    pub fn job_revision(&self, id: &str) -> u64 {
        self.lock().state.jobs.get(id).map_or(0, |j| j.revision)
    }

    // ---- persistence -------------------------------------------------------

    fn save(&self, g: &Inner) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(&g.state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        atomic_write(&self.cfg.state_dir.join("state.json"), &bytes)
    }

    fn save_upload(&self, up: &Upload) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(up)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        atomic_write(&self.meta_path(&up.id), &bytes)
    }

    fn storage_error(&self, e: std::io::Error) -> Response {
        self.emit(
            "STORAGE_ERROR",
            json!({ "kind": format!("{:?}", e.kind()) }),
        );
        Response::err(500, "storage_error")
    }

    // ---- request entry -----------------------------------------------------

    /// Largest body the transport should read for this request before the
    /// request is authenticated.
    pub fn body_limit(&self, method: &str, path: &str) -> usize {
        match match_route(method, path) {
            Matched::Route(r, _) => match r.id {
                "upload.chunk" => self.cfg.limits.chunk_max as usize + 1,
                "webhook.inbound" => self.cfg.max_webhook_body,
                _ => self.cfg.max_json_body,
            },
            _ => 0,
        }
    }

    pub fn handle(&self, req: Request) -> Response {
        let (route, params) = match match_route(&req.method, req.path()) {
            Matched::Route(r, p) => (r, p),
            Matched::MethodNotAllowed => return Response::err(405, "method_not_allowed"),
            Matched::NotFound => return Response::err(404, "not_found"),
        };
        if req.body.len() > self.body_limit(&req.method, req.path()) {
            return Response::err(413, "body_too_large");
        }
        match route.class {
            AuthClassId::DeviceAuthenticatedMutation => self.handle_device(route, params, &req),
            AuthClassId::OauthCallback => self.handle_oauth(route, params, &req),
            AuthClassId::SignedWebhook => self.handle_webhook(route, params, &req),
        }
    }

    fn reject(&self, route: &RouteDef, reason: &str, status: u16, code: &str) -> Response {
        self.emit(
            "AUTH_REJECTED",
            json!({
                "reason": reason,
                "routeId": route.id,
                "class": route.class,
            }),
        );
        Response::err(status, code)
    }

    // ---- device-authenticated class ---------------------------------------

    fn handle_device(
        &self,
        route: &'static RouteDef,
        params: Vec<String>,
        req: &Request,
    ) -> Response {
        let now = self.clock.now_secs();
        let mut g = self.lock();
        if route.id == "device.enrol" {
            return self.enrol(&mut g, route, req, now);
        }
        let device = match self.authenticate(&mut g, route, req, now) {
            Ok(d) => d,
            Err(r) => return r,
        };
        match route.id {
            "upload.init" => self.upload_init(&mut g, &device, req, now),
            "upload.chunk" => self.upload_chunk(&mut g, &device, &params, req, now),
            "upload.status" => self.upload_status(&mut g, &device, &params, now),
            "upload.complete" => self.upload_complete(g, &device, &params, req, now),
            "job.update" => self.job_update(&mut g, &device, &params, req, now),
            _ => Response::err(404, "not_found"),
        }
    }

    /// Parse the shared signed-request headers.
    fn signed_parts<'r>(
        &self,
        route: &RouteDef,
        req: &'r Request,
        now: u64,
    ) -> Result<(u64, &'r str, Vec<u8>), Response> {
        let ts: u64 = match req.header(H_TIMESTAMP).and_then(|v| v.parse().ok()) {
            Some(t) => t,
            None => return Err(self.reject(route, "malformed_timestamp", 401, "unauthenticated")),
        };
        if ts.abs_diff(now) > self.cfg.replay_window_secs {
            return Err(self.reject(route, "expired_timestamp", 401, "unauthenticated"));
        }
        let nonce = match req.header(H_NONCE) {
            Some(n)
                if !n.is_empty() && n.len() <= 128 && n.bytes().all(|b| b.is_ascii_graphic()) =>
            {
                n
            }
            _ => return Err(self.reject(route, "malformed_nonce", 401, "unauthenticated")),
        };
        let sig = match req
            .header(H_SIGNATURE)
            .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
        {
            Some(s) => s,
            None => return Err(self.reject(route, "malformed_signature", 401, "unauthenticated")),
        };
        Ok((ts, nonce, sig))
    }

    fn verify_signature(
        &self,
        key: &[u8],
        req: &Request,
        ts: u64,
        nonce: &str,
        sig: &[u8],
    ) -> bool {
        let msg = canonical_request(&req.method, &req.path_and_query, ts, nonce, &req.body);
        UnparsedPublicKey::new(&ED25519, key)
            .verify(msg.as_bytes(), sig)
            .is_ok()
    }

    fn authenticate(
        &self,
        g: &mut Inner,
        route: &RouteDef,
        req: &Request,
        now: u64,
    ) -> Result<String, Response> {
        if req.header(H_PROVIDER_SIGNATURE).is_some() {
            return Err(self.reject(route, "wrong_class", 401, "unauthenticated"));
        }
        let Some(auth) = req.header("authorization") else {
            return Err(self.reject(route, "missing_credentials", 401, "unauthenticated"));
        };
        let device_id = match auth.split_once(' ') {
            Some(("RK-Device", id)) if !id.is_empty() && id.len() <= MAX_ID => id.to_string(),
            Some(("RK-Enrol", _)) => {
                return Err(self.reject(
                    route,
                    "enrolment_code_on_non_enrol_route",
                    401,
                    "unauthenticated",
                ))
            }
            _ => return Err(self.reject(route, "malformed_credentials", 401, "unauthenticated")),
        };
        let (ts, nonce, sig) = self.signed_parts(route, req, now)?;
        let Some(device) = g.state.devices.get(&device_id) else {
            return Err(self.reject(route, "unknown_device", 401, "unauthenticated"));
        };
        if device.revoked {
            return Err(self.reject(route, "revoked_device", 401, "unauthenticated"));
        }
        let key = URL_SAFE_NO_PAD
            .decode(&device.public_key)
            .unwrap_or_default();
        if !self.verify_signature(&key, req, ts, nonce, &sig) {
            return Err(self.reject(route, "bad_signature", 401, "unauthenticated"));
        }
        let granted: Vec<String> = device.scopes.clone();
        let allowed = route
            .scopes
            .iter()
            .any(|s| granted.iter().any(|g| Some(g.as_str()) == scope_name(*s)));
        if !allowed {
            return Err(self.reject(route, "insufficient_scope", 403, "forbidden"));
        }
        // The nonce is recorded only after the signature verified, so an
        // unauthenticated caller cannot fill the replay table.
        let window = self.cfg.replay_window_secs;
        let seen = g.state.nonces.entry(device_id.clone()).or_default();
        if seen.contains_key(nonce) {
            return Err(self.reject(route, "replayed_nonce", 401, "unauthenticated"));
        }
        seen.retain(|_, t| *t + window >= now);
        seen.insert(nonce.to_string(), ts);
        self.save(g).map_err(|e| self.storage_error(e))?;
        Ok(device_id)
    }

    fn enrol(&self, g: &mut Inner, route: &RouteDef, req: &Request, now: u64) -> Response {
        if req.header(H_PROVIDER_SIGNATURE).is_some() {
            return self.reject(route, "wrong_class", 401, "unauthenticated");
        }
        let code = match req.header("authorization").and_then(|a| a.split_once(' ')) {
            Some(("RK-Enrol", c)) if !c.is_empty() && c.len() <= MAX_ID => c,
            Some(("RK-Device", _)) | None => {
                return self.reject(route, "missing_credentials", 401, "unauthenticated")
            }
            _ => return self.reject(route, "malformed_credentials", 401, "unauthenticated"),
        };
        let (ts, nonce, sig) = match self.signed_parts(route, req, now) {
            Ok(p) => p,
            Err(r) => return r,
        };
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(_) => return Response::err(400, "bad_request"),
        };
        let key = match body
            .get("publicKey")
            .and_then(Value::as_str)
            .and_then(|k| URL_SAFE_NO_PAD.decode(k).ok())
        {
            Some(k) if k.len() == 32 => k,
            _ => return Response::err(400, "bad_request"),
        };
        // Proof of possession: the enrolling key signs the enrolment request.
        if !self.verify_signature(&key, req, ts, nonce, &sig) {
            return self.reject(route, "bad_signature", 401, "unauthenticated");
        }
        let code_hash = sha256_hex(code.as_bytes());
        let scopes = match g.state.enrol_codes.get(&code_hash) {
            None => return self.reject(route, "bad_enrolment_code", 401, "unauthenticated"),
            Some(c) if c.expires_at < now => {
                g.state.enrol_codes.remove(&code_hash);
                let _ = self.save(g);
                return self.reject(route, "expired_enrolment_code", 401, "unauthenticated");
            }
            Some(c) => c.scopes.clone(),
        };
        let device_id = format!("dev_{}", &sha256_hex(&key)[..16]);
        if g.state.devices.contains_key(&device_id) {
            return Response::err(409, "already_enrolled");
        }
        g.state.enrol_codes.remove(&code_hash); // single use
        g.state.devices.insert(
            device_id.clone(),
            Device {
                public_key: URL_SAFE_NO_PAD.encode(&key),
                label: body
                    .get("label")
                    .and_then(Value::as_str)
                    .map(|s| s.chars().take(64).collect()),
                scopes: scopes.clone(),
                enrolled_at: now,
                revoked: false,
            },
        );
        g.state
            .nonces
            .entry(device_id.clone())
            .or_default()
            .insert(nonce.to_string(), ts);
        if let Err(e) = self.save(g) {
            return self.storage_error(e);
        }
        self.emit(
            "ENROLLED",
            json!({ "deviceId": device_id, "scopes": scopes }),
        );
        Response::new(201, json!({ "deviceId": device_id, "scopes": scopes }))
    }

    // ---- uploads -----------------------------------------------------------

    fn sweep_uploads(&self, g: &mut Inner, now: u64) {
        let expired: Vec<String> = g
            .uploads
            .values()
            .filter(|u| u.complete.is_none() && !u.finalizing && now > u.expires_at)
            .map(|u| u.id.clone())
            .collect();
        for id in expired {
            let _ = fs::remove_file(self.part_path(&id));
            let _ = fs::remove_file(self.meta_path(&id));
            g.uploads.remove(&id);
        }
    }

    fn upload_init(&self, g: &mut Inner, device: &str, req: &Request, now: u64) -> Response {
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(_) => return Response::err(400, "bad_request"),
        };
        let (Some(size), Some(chunk)) = (
            body.get("size").and_then(Value::as_u64),
            body.get("chunkBytes").and_then(Value::as_u64),
        ) else {
            return Response::err(400, "bad_request");
        };
        let lim = self.cfg.limits;
        if size == 0 || size > lim.max_object_bytes {
            return Response::err(403, "too_large");
        }
        if chunk < lim.chunk_min.min(size) || chunk > lim.chunk_max {
            return Response::err(400, "bad_chunk_size");
        }
        self.sweep_uploads(g, now);
        let open = g
            .uploads
            .values()
            .filter(|u| u.device_id == device && u.complete.is_none())
            .count();
        if open >= lim.max_open_per_device {
            return Response::err(403, "too_many_uploads");
        }
        let id = format!("up_{}", hex(&random_bytes::<16>()));
        let key = random_bytes::<32>();
        let up = Upload {
            id: id.clone(),
            device_id: device.to_string(),
            size,
            chunk_bytes: chunk,
            chunk_count: size.div_ceil(chunk),
            key_hex: hex(&key),
            created_at: now,
            expires_at: now + lim.session_ttl_secs,
            received: BTreeMap::new(),
            complete: None,
            finalizing: false,
        };
        if let Err(e) = self.save_upload(&up) {
            return self.storage_error(e);
        }
        let resp = json!({
            "uploadId": id,
            "digestKey": up.key_hex,
            "chunkBytes": up.chunk_bytes,
            "chunkCount": up.chunk_count,
            "expiresAt": up.expires_at,
        });
        g.uploads.insert(id.clone(), up);
        self.emit(
            "UPLOAD_STARTED",
            json!({ "uploadId": id, "deviceId": device, "size": size }),
        );
        Response::new(201, resp)
    }

    fn owned_upload<'a>(g: &'a mut Inner, device: &str, id: &str) -> Option<&'a mut Upload> {
        g.uploads.get_mut(id).filter(|u| u.device_id == device)
    }

    fn upload_chunk(
        &self,
        g: &mut Inner,
        device: &str,
        params: &[String],
        req: &Request,
        now: u64,
    ) -> Response {
        let Some(up) = Self::owned_upload(g, device, &params[0]) else {
            return Response::err(404, "not_found");
        };
        if up.complete.is_some() {
            return Response::err(409, "already_complete");
        }
        if up.finalizing {
            return Response::err(409, "finalizing");
        }
        if now > up.expires_at {
            return Response::err(404, "not_found");
        }
        let Ok(index) = params[1].parse::<u64>() else {
            return Response::err(416, "bad_chunk_index");
        };
        if index >= up.chunk_count {
            return Response::err(416, "bad_chunk_index");
        }
        let offset = index * up.chunk_bytes;
        let expected = up.chunk_bytes.min(up.size - offset);
        if req.body.len() as u64 > self.cfg.limits.chunk_max {
            return Response::err(416, "chunk_too_large");
        }
        if req.body.len() as u64 != expected {
            return Response::err(416, "bad_chunk_length");
        }
        let key: [u8; 32] = unhex(&up.key_hex)
            .and_then(|k| k.try_into().ok())
            .unwrap_or([0; 32]);
        let digest = blake3::keyed_hash(&key, &req.body).to_hex().to_string();
        if let Some(claimed) = req.header("x-chunk-digest") {
            if !ct_eq(claimed.as_bytes(), digest.as_bytes()) {
                return Response::err(400, "chunk_digest_mismatch");
            }
        }
        if let Some(prev) = up.received.get(&index) {
            return if ct_eq(prev.as_bytes(), digest.as_bytes()) {
                Response::new(204, Value::Null) // idempotent retry
            } else {
                Response::err(409, "chunk_conflict")
            };
        }
        let part = self.part_path(&up.id);
        let write = (|| -> std::io::Result<()> {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&part)?;
            if f.metadata()?.len() != up.size {
                f.set_len(up.size)?;
            }
            f.seek(SeekFrom::Start(offset))?;
            f.write_all(&req.body)?;
            f.sync_data()
        })();
        if let Err(e) = write {
            return self.storage_error(e);
        }
        up.received.insert(index, digest);
        let snapshot = up.clone();
        if let Err(e) = self.save_upload(&snapshot) {
            if let Some(u) = g.uploads.get_mut(&snapshot.id) {
                u.received.remove(&index);
            }
            return self.storage_error(e);
        }
        Response::new(204, Value::Null)
    }

    fn upload_status(&self, g: &mut Inner, device: &str, params: &[String], now: u64) -> Response {
        let Some(up) = Self::owned_upload(g, device, &params[0]) else {
            return Response::err(404, "not_found");
        };
        if up.complete.is_none() && now > up.expires_at {
            return Response::err(404, "not_found");
        }
        let next = (0..up.chunk_count)
            .find(|i| !up.received.contains_key(i))
            .map_or(up.size, |i| i * up.chunk_bytes);
        Response::new(
            200,
            json!({
                "uploadId": up.id,
                "size": up.size,
                "chunkBytes": up.chunk_bytes,
                "chunkCount": up.chunk_count,
                "received": up.received.keys().collect::<Vec<_>>(),
                "nextOffset": next,
                "complete": up.complete.is_some(),
                "digest": up.complete.as_ref().map(|c| c.digest.clone()),
                "expiresAt": up.expires_at,
            }),
        )
    }

    fn upload_complete(
        &self,
        mut g: Guard<'_>,
        device: &str,
        params: &[String],
        req: &Request,
        now: u64,
    ) -> Response {
        let id = params[0].clone();
        let claimed = match serde_json::from_slice::<Value>(&req.body)
            .ok()
            .and_then(|v| v.get("digest").and_then(Value::as_str).map(str::to_string))
        {
            Some(d) if unhex(&d).is_some_and(|b| b.len() == 32) => d.to_ascii_lowercase(),
            _ => return Response::err(400, "bad_request"),
        };
        let Some(up) = Self::owned_upload(&mut g, device, &id) else {
            return Response::err(404, "not_found");
        };
        if let Some(done) = &up.complete {
            // Idempotent: the same digest re-acknowledges, a different one is refused.
            return if ct_eq(done.digest.as_bytes(), claimed.as_bytes()) {
                Response::new(
                    200,
                    json!({ "uploadId": id, "digest": done.digest, "size": up.size }),
                )
            } else {
                Response::err(400, "hash_mismatch")
            };
        }
        if up.finalizing {
            return Response::err(409, "finalizing");
        }
        if now > up.expires_at {
            return Response::err(404, "not_found");
        }
        if (up.received.len() as u64) < up.chunk_count {
            let missing: Vec<u64> = (0..up.chunk_count)
                .filter(|i| !up.received.contains_key(i))
                .take(32)
                .collect();
            return Response::new(400, json!({ "error": "incomplete", "missing": missing }));
        }
        up.finalizing = true;
        let key: [u8; 32] = unhex(&up.key_hex)
            .and_then(|k| k.try_into().ok())
            .unwrap_or([0; 32]);
        let size = up.size;
        drop(g); // hash outside the lock so other devices are not stalled

        let part = self.part_path(&id);
        let hashed = (|| -> std::io::Result<(String, u64)> {
            let mut f = File::open(&part)?;
            let len = f.metadata()?.len();
            let mut h = blake3::Hasher::new_keyed(&key);
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok((h.finalize().to_hex().to_string(), len))
        })();

        let mut g = self.lock();
        let (actual, len) = match hashed {
            Ok(v) => v,
            Err(e) => {
                if let Some(u) = g.uploads.get_mut(&id) {
                    u.finalizing = false;
                }
                return self.storage_error(e);
            }
        };
        if len != size || !ct_eq(actual.as_bytes(), claimed.as_bytes()) {
            // The assembled object is wrong: discard the session so the client restarts clean.
            let _ = fs::remove_file(&part);
            let _ = fs::remove_file(self.meta_path(&id));
            g.uploads.remove(&id);
            self.emit(
                "UPLOAD_REJECTED",
                json!({ "uploadId": id, "reason": "hash_mismatch" }),
            );
            return Response::err(400, "hash_mismatch");
        }
        let committed = (|| -> std::io::Result<()> {
            File::open(&part)?.sync_all()?;
            let up = g
                .uploads
                .get_mut(&id)
                .expect("finalizing sessions are not swept");
            up.finalizing = false;
            up.complete = Some(UploadDone {
                digest: actual.clone(),
                completed_at: now,
            });
            // Intent is durable before the rename; `recover` finishes it after a crash.
            let snapshot = up.clone();
            self.save_upload(&snapshot)?;
            fs::rename(&part, self.object_path(&id))?;
            Ok(())
        })();
        if let Err(e) = committed {
            return self.storage_error(e);
        }
        self.emit(
            "UPLOAD_COMPLETE",
            json!({ "uploadId": id, "deviceId": device, "size": size, "digest": actual }),
        );
        Response::new(
            200,
            json!({ "uploadId": id, "digest": actual, "size": size }),
        )
    }

    // ---- compare-and-swap job mutation ------------------------------------

    fn job_update(
        &self,
        g: &mut Inner,
        device: &str,
        params: &[String],
        req: &Request,
        now: u64,
    ) -> Response {
        let job_id = &params[0];
        if job_id.len() > MAX_ID || job_id.chars().any(char::is_control) {
            return Response::err(400, "bad_request");
        }
        let if_match: u64 = match req
            .header("if-match")
            .and_then(|v| v.trim_matches('"').parse().ok())
        {
            Some(v) => v,
            None => return Response::err(428, "precondition_required"),
        };
        let idem = match req.header("idempotency-key") {
            Some(k) if !k.is_empty() && k.len() <= MAX_ID => k,
            _ => return Response::err(428, "precondition_required"),
        };
        let Ok(Value::Object(patch)) = serde_json::from_slice::<Value>(&req.body) else {
            return Response::err(400, "bad_request");
        };
        let request_hash =
            sha256_hex(format!("{job_id}\n{if_match}\n{}", sha256_hex(&req.body)).as_bytes());
        let ttl = self.cfg.idempotency_ttl_secs;
        g.state.idempotency.retain(|_, r| r.at + ttl >= now);
        let idem_key = format!("{device}:{idem}");
        if let Some(rec) = g.state.idempotency.get(&idem_key) {
            return if rec.request_hash == request_hash {
                Response::new(rec.status, rec.body.clone()).header("Idempotency-Replayed", "true")
            } else {
                Response::err(422, "idempotency_key_reuse")
            };
        }
        let current = g.state.jobs.get(job_id).map_or(0, |j| j.revision);
        if if_match != current {
            return Response::new(
                409,
                json!({ "error": "revision_conflict", "currentRevision": current }),
            );
        }
        let job = g.state.jobs.entry(job_id.clone()).or_insert_with(|| Job {
            revision: 0,
            fields: serde_json::Map::new(),
        });
        for (k, v) in patch {
            if v.is_null() {
                job.fields.remove(&k);
            } else {
                job.fields.insert(k, v);
            }
        }
        job.revision += 1;
        let body = json!({ "jobId": job_id, "revision": job.revision, "fields": job.fields });
        let revision = job.revision;
        g.state.idempotency.insert(
            idem_key,
            IdemRecord {
                request_hash,
                status: 200,
                body: body.clone(),
                at: now,
            },
        );
        if let Err(e) = self.save(g) {
            return self.storage_error(e);
        }
        self.emit(
            "JOB_UPDATED",
            json!({ "jobId": job_id, "revision": revision, "deviceId": device }),
        );
        Response::new(200, body).header("ETag", format!("\"{revision}\""))
    }

    // ---- OAuth callback class ---------------------------------------------

    fn handle_oauth(
        &self,
        route: &'static RouteDef,
        params: Vec<String>,
        req: &Request,
    ) -> Response {
        // This class never accepts device or webhook credentials.
        if req.header("authorization").is_some()
            || req.header(H_SIGNATURE).is_some()
            || req.header(H_PROVIDER_SIGNATURE).is_some()
        {
            return self.reject(route, "wrong_class", 400, "bad_request");
        }
        let Some(handler) = &self.oauth else {
            return Response::err(502, "oauth_unavailable");
        };
        let (mut code, mut state, mut error) = (None, None, None);
        for (k, v) in form_urlencoded::parse(req.query().as_bytes()) {
            match k.as_ref() {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                "error" => error = Some(v.into_owned()),
                _ => {}
            }
        }
        match handler.complete(&params[0], code, state, error) {
            OAuthOutcome::Completed => {
                self.emit("OAUTH_COMPLETED", json!({ "provider": params[0] }));
                Response::new(302, Value::Null)
                    .header("Location", self.cfg.oauth_success_redirect.clone())
            }
            OAuthOutcome::Rejected => self.reject(route, "oauth_rejected", 400, "bad_request"),
            OAuthOutcome::Unavailable => Response::err(502, "oauth_unavailable"),
        }
    }

    // ---- signed webhook class ---------------------------------------------

    fn handle_webhook(
        &self,
        route: &'static RouteDef,
        params: Vec<String>,
        req: &Request,
    ) -> Response {
        let now = self.clock.now_secs();
        let provider = &params[0];
        if req.header("authorization").is_some() || req.header(H_SIGNATURE).is_some() {
            return self.reject(route, "wrong_class", 401, "unauthenticated");
        }
        let Some(secret) = self.cfg.webhook_secrets.get(provider) else {
            return self.reject(route, "unknown_provider", 401, "unauthenticated");
        };
        let ts: u64 = match req
            .header(H_PROVIDER_TIMESTAMP)
            .and_then(|v| v.parse().ok())
        {
            Some(t) => t,
            None => return self.reject(route, "malformed_timestamp", 401, "unauthenticated"),
        };
        if ts.abs_diff(now) > self.cfg.webhook_tolerance_secs {
            return self.reject(route, "expired_timestamp", 401, "unauthenticated");
        }
        let sig_text = match req.header(H_PROVIDER_SIGNATURE) {
            Some(s) => s.strip_prefix("sha256=").unwrap_or(s).to_string(),
            None => return self.reject(route, "missing_credentials", 401, "unauthenticated"),
        };
        let Some(sig) = unhex(&sig_text) else {
            return self.reject(route, "malformed_signature", 401, "unauthenticated");
        };
        let mut msg = ts.to_string().into_bytes();
        msg.push(b'.');
        msg.extend_from_slice(&req.body);
        // ring's verify compares in constant time.
        if hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, secret), &msg, &sig).is_err() {
            return self.reject(route, "bad_signature", 401, "unauthenticated");
        }
        let dedupe =
            sha256_hex(format!("{provider}\n{}\n{ts}", sig_text.to_ascii_lowercase()).as_bytes());
        let mut g = self.lock();
        let horizon = self.cfg.webhook_tolerance_secs * 2;
        g.state.webhook_seen.retain(|_, t| *t + horizon >= now);
        if g.state.webhook_seen.contains_key(&dedupe) {
            return Response::err(409, "duplicate_delivery");
        }
        if g.state.webhook_inbox.len() >= self.cfg.max_webhook_inbox {
            return Response::err(429, "rate_limited");
        }
        g.state.webhook_seen.insert(dedupe.clone(), now);
        g.state.webhook_inbox.push(InboxItem {
            id: dedupe.clone(),
            provider: provider.clone(),
            received_at: now,
            body_b64: STANDARD.encode(&req.body),
        });
        // Durable before the 202: a delivery acknowledged to the provider survives a crash.
        if let Err(e) = self.save(&g) {
            g.state.webhook_seen.remove(&dedupe);
            g.state.webhook_inbox.pop();
            return self.storage_error(e);
        }
        drop(g);
        self.emit(
            "WEBHOOK_ACCEPTED",
            json!({ "provider": provider, "deliveryId": dedupe }),
        );
        Response::new(202, json!({ "accepted": true }))
    }
}

fn scope_name(s: rightkit_contracts::types::daemon_api::Scope) -> Option<&'static str> {
    use rightkit_contracts::types::daemon_api::Scope::*;
    Some(match s {
        Venture => "venture",
        Channel => "channel",
        Account => "account",
    })
}
