//! The transport-free daemon core. [`Daemon::handle`] takes a fully read
//! request and returns a response; the axum adapter and the tests both drive
//! exactly this function, so there is one authorisation path.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rightkit_contracts::types::daemon_api::AuthClassId;
use ring::hmac;
use serde_json::{json, Value};

mod link;
mod pairing;
mod tasks;

pub use self::link::{Accepted, FrameOutcome, LinkSession, Out};
pub use self::pairing::{PairedDevice, PairingError, PairingInfo, PairingOptions, PairingStage};
pub use self::tasks::{TaskInfo, TaskSpec};
pub use crate::remote::events::EventsPage;

use crate::record::write_atomic;
use crate::remote::call::DeviceCalls;
use crate::remote::config::DaemonConfig;
use crate::remote::events::EventLog;
use crate::remote::routes::{match_route, Matched, RouteDef, CALL_ROUTE_ID, LINK_ROUTE_ID};
use crate::remote::signing::{
    chunk_digest, object_digest, object_hasher, parse_digest_key, verify_request, H_NONCE,
    H_PROVIDER_SIGNATURE, H_PROVIDER_TIMESTAMP, H_SIGNATURE, H_TIMESTAMP,
};
use crate::remote::state::{
    Device, EnrolCode, IdemRecord, InboxItem, Job, State, TaskStatus, Upload, UploadDone,
};
use crate::remote::util::{ct_eq, hex, random_bytes, sha256_hex, unhex, Clock};

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
/// wraps `rightkit-oauth`'s server flow ([`crate::remote::oauth::FlowOAuthHandler`]).
pub trait OAuthCallbackHandler: Send + Sync {
    fn complete(
        &self,
        provider: &str,
        code: Option<String>,
        state: Option<String>,
        error: Option<String>,
    ) -> OAuthOutcome;
}

/// One enrolled device as [`Daemon::list_devices`] reports it. Never carries the public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device_id: String,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    /// The single `role:<name>` scope's name, when the device has exactly one.
    pub role: Option<String>,
    /// Second the device enrolled.
    pub enrolled_at: u64,
    pub revoked: bool,
    /// The device has a live, ready `rk-link/1` connection right now.
    pub linked: bool,
}

/// Runtime figures of a daemon, computed on call by [`Daemon::status`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DaemonStatus {
    /// Request counts, `state.json` write sizes and retained-state counts.
    pub metrics: DaemonMetrics,
    /// Resident memory, CPU time and I/O counters of the whole process (the daemon runs in it), or
    /// `None` when the host cannot report them. See `rightkit_child_process::ProcessUsage`.
    pub process: Option<rightkit_child_process::ProcessUsage>,
}

struct Inner {
    state: State,
    uploads: BTreeMap<String, Upload>,
    /// `state` holds changes that `state.json` does not yet. Set under the lock and cleared by a
    /// successful write, so one request's changes reach disk in a single write before it answers.
    dirty: bool,
}

/// Figures from [`Daemon::metrics`]. Byte counts are for `state.json`, which is rewritten in
/// full (and fsynced) on every durable change, so `last_state_bytes` is the per-write cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DaemonMetrics {
    /// Requests passed to [`Daemon::handle`] since open.
    pub requests: u64,
    /// `job.update` requests answered 200 since open.
    pub job_updates: u64,
    /// Successful `state.json` writes since open.
    pub state_saves: u64,
    /// Size of the most recent `state.json` write.
    pub last_state_bytes: u64,
    /// Largest `state.json` write since open.
    pub max_state_bytes: u64,
    pub devices: u64,
    pub jobs: u64,
    /// Idempotency records retained (pruned after `idempotency_ttl_secs`).
    pub idempotency_records: u64,
    /// Lifecycle events held in memory (capped at 10,000; `events.jsonl` keeps all).
    pub events_retained: u64,
    /// Replay-table nonces retained across all devices (pruned after `replay_window_secs`).
    pub nonce_records: u64,
    /// Live link connections.
    pub links: u64,
    /// Link tasks waiting for a device.
    pub tasks_queued: u64,
    /// Link tasks currently leased to a device.
    pub tasks_leased: u64,
}

#[derive(Debug, Default)]
struct Counters {
    requests: AtomicU64,
    job_updates: AtomicU64,
    state_saves: AtomicU64,
    last_state_bytes: AtomicU64,
    max_state_bytes: AtomicU64,
}

/// Why [`Daemon::forget_device`] did not remove a device.
#[derive(Debug)]
#[non_exhaustive]
pub enum ForgetDeviceError {
    /// The device is not revoked. Revoke it first; a live device is never forgotten.
    NotRevoked,
    /// The state could not be written; the record is unchanged.
    Storage(std::io::Error),
}

impl std::fmt::Display for ForgetDeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRevoked => {
                write!(
                    f,
                    "the device is not revoked; revoke it before forgetting it"
                )
            }
            Self::Storage(e) => write!(f, "could not write the daemon state: {e}"),
        }
    }
}

impl std::error::Error for ForgetDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            Self::NotRevoked => None,
        }
    }
}

pub struct Daemon {
    cfg: DaemonConfig,
    clock: Arc<dyn Clock>,
    events: Arc<EventLog>,
    oauth: Option<Arc<dyn OAuthCallbackHandler>>,
    calls: Option<DeviceCalls>,
    /// Live `rk-link/1` connections by device id (at most one per device).
    links: Mutex<BTreeMap<String, Arc<LinkSession>>>,
    /// Short-code pairing window and sessions (memory only).
    pairing: Mutex<pairing::PairingState>,
    inner: Mutex<Inner>,
    counters: Counters,
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
        // One bad upload record must not stop the daemon: it is skipped and reported, and every
        // other session is loaded. A record that does not parse is set aside as `.json.bad`. A
        // record that cannot be read is left where it is: a refused read (on Windows an open can
        // be refused for a moment while a file is being replaced) is no proof the file is bad.
        let mut uploads = BTreeMap::new();
        let mut skipped: Vec<(String, &'static str, String)> = Vec::new();
        for entry in fs::read_dir(cfg.state_dir.join("uploads"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let file = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    skipped.push((file, "unreadable", error.kind().to_string()));
                    continue;
                }
            };
            match serde_json::from_slice::<Upload>(&bytes) {
                Ok(up) => {
                    uploads.insert(up.id.clone(), up);
                }
                Err(_) => {
                    // The parser's message can quote the record, so it is not logged.
                    let moved = fs::rename(&path, path.with_extension("json.bad")).is_ok();
                    let detail = if moved { "set aside" } else { "left in place" };
                    skipped.push((file, "unparsable", detail.to_string()));
                }
            }
        }
        let d = Self {
            cfg,
            clock,
            events,
            oauth,
            calls: None,
            links: Mutex::new(BTreeMap::new()),
            pairing: Mutex::new(pairing::PairingState::default()),
            inner: Mutex::new(Inner {
                state,
                uploads,
                dirty: false,
            }),
            counters: Counters::default(),
        };
        for (file, reason, detail) in skipped {
            d.emit(
                "UPLOAD_RECORD_SKIPPED",
                json!({ "file": file, "reason": reason, "detail": detail }),
            );
        }
        d.recover()?;
        Ok(d)
    }

    /// Serve `POST /v1/call/{method}` from `calls`. Without it the route answers 404, as a 1.0.0
    /// daemon does.
    pub fn with_calls(mut self, calls: DeviceCalls) -> Self {
        self.calls = Some(calls);
        self
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
        self.save(&mut g)?;
        Ok(code)
    }

    pub fn revoke_device(&self, device_id: &str) -> std::io::Result<bool> {
        let mut g = self.lock();
        let Some(d) = g.state.devices.get_mut(device_id) else {
            return Ok(false);
        };
        d.revoked = true;
        self.save(&mut g)?;
        drop(g);
        self.emit("DEVICE_REVOKED", json!({ "deviceId": device_id }));
        // A revoked device keeps no link; its leases lapse and re-queue.
        self.close_link(device_id, "revoked");
        Ok(true)
    }

    /// Remove a REVOKED device and every record that ties its id to this host, so the same key
    /// can enrol again (enrolment answers 409 `already_enrolled` for any device id still in
    /// state, revoked or not) and comes back as a brand-new device, with nothing of the old one.
    ///
    /// In-process administration only: no route, link frame or call reaches this method, and it
    /// must stay that way, since it turns a revoked credential back into an enrollable one.
    ///
    /// * `Ok(true)`: the device was removed and `DEVICE_FORGOTTEN` was logged (once).
    /// * `Ok(false)`: no such device. Calling again after success lands here, so it is idempotent.
    /// * `Err(ForgetDeviceError::NotRevoked)`: the device is live. Call
    ///   [`revoke_device`](Self::revoke_device) first; a live device is never forgotten.
    /// * `Err(ForgetDeviceError::Storage)`: `state.json` could not be written; nothing changed.
    ///
    /// Removed together, in one durable write: the device record (public key, scopes, label);
    /// its idempotency records; its in-flight upload sessions (metadata and partial data; a
    /// committed object stays, it is host data); tasks leased to it that are not pinned to it
    /// (they go back to the queue); tasks pinned to it that have not finished (they fail with
    /// `device_forgotten`, since nobody else may run them); and any open link.
    ///
    /// Replay nonces still inside the replay window are KEPT on purpose: forgetting must not
    /// reopen the timestamp window to requests the old record already saw, which an attacker
    /// holding captured traffic could otherwise replay once the key enrols again. Nonces older
    /// than the window are dropped (they can no longer match a valid timestamp), as are any
    /// such leftovers of earlier forgotten devices. A device re-enrols with a fresh enrolment
    /// code, as for any new device, and gets a new record with the scopes of that code; nothing
    /// of the old record, its scopes included, is carried over.
    pub fn forget_device(&self, device_id: &str) -> Result<bool, ForgetDeviceError> {
        let now = self.clock.now_secs();
        let window = self.cfg.replay_window_secs;
        let mut g = self.lock();
        match g.state.devices.get(device_id) {
            None => return Ok(false),
            Some(d) if !d.revoked => return Err(ForgetDeviceError::NotRevoked),
            Some(_) => {}
        }
        // Snapshot everything this touches, so a failed write leaves memory equal to disk.
        let old_devices = g.state.devices.clone();
        let old_nonces = g.state.nonces.clone();
        let old_idem = g.state.idempotency.clone();
        let old_tasks = g.state.tasks.clone();

        g.state.devices.remove(device_id);
        {
            let State {
                devices, nonces, ..
            } = &mut g.state;
            nonces.retain(|id, seen| {
                seen.retain(|_, t| *t + window >= now);
                devices.contains_key(id) || !seen.is_empty()
            });
        }
        let idem_prefix = format!("{device_id}:");
        g.state
            .idempotency
            .retain(|k, _| !k.starts_with(&idem_prefix));
        let mut requeued: Vec<(String, u64)> = Vec::new();
        let mut failed: Vec<(String, u64)> = Vec::new();
        for t in g.state.tasks.values_mut() {
            let live = matches!(t.status, TaskStatus::Queued | TaskStatus::Leased);
            if !live {
                continue;
            }
            if t.target.as_deref() == Some(device_id) {
                failed.push((t.id.clone(), t.epoch));
                t.status = TaskStatus::Failed;
                t.owner = None;
                t.acked = false;
                t.expires = now;
                t.outcome = Some(json!({ "ok": false, "error": "device_forgotten" }));
            } else if t.status == TaskStatus::Leased && t.owner.as_deref() == Some(device_id) {
                requeued.push((t.id.clone(), t.epoch));
                t.status = TaskStatus::Queued;
                t.owner = None;
                t.acked = false;
            }
        }
        if let Err(e) = self.save(&mut g) {
            // Not durable, so not done: put everything back.
            g.state.devices = old_devices;
            g.state.nonces = old_nonces;
            g.state.idempotency = old_idem;
            g.state.tasks = old_tasks;
            g.dirty = true;
            return Err(ForgetDeviceError::Storage(e));
        }
        // Durable: now the upload sessions (their files are not part of `state.json`).
        let doomed: Vec<String> = g
            .uploads
            .values()
            .filter(|u| u.device_id == device_id && u.complete.is_none() && !u.finalizing)
            .map(|u| u.id.clone())
            .collect();
        for id in &doomed {
            let _ = fs::remove_file(self.part_path(id));
            let _ = fs::remove_file(self.meta_path(id));
            g.uploads.remove(id);
        }
        drop(g);
        self.emit("DEVICE_FORGOTTEN", json!({ "deviceId": device_id }));
        for (task, epoch) in requeued {
            self.emit(
                "TASK_LEASE_LOST",
                json!({ "taskId": task, "deviceId": device_id, "epoch": epoch,
                        "reason": "forgotten" }),
            );
        }
        for (task, epoch) in failed {
            self.emit(
                "TASK_FAILED",
                json!({ "taskId": task, "deviceId": device_id, "epoch": epoch,
                        "reason": "device_forgotten" }),
            );
        }
        // Defensive: a revoked device has no link, but never leave one open for a forgotten id.
        self.close_link(device_id, "forgotten");
        self.dispatch();
        Ok(true)
    }

    pub fn device_ids(&self) -> Vec<String> {
        self.lock().state.devices.keys().cloned().collect()
    }

    /// Every enrolled device, revoked ones included (a screen shows both), oldest enrolment
    /// first and by id within a second. Reads state and live links; changes nothing. A device
    /// is `linked` only while its link is open and past the handshake.
    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        let linked: std::collections::BTreeSet<String> =
            self.linked_devices().into_iter().collect();
        let g = self.lock();
        let mut out: Vec<DeviceInfo> = g
            .state
            .devices
            .iter()
            .map(|(id, d)| DeviceInfo {
                device_id: id.clone(),
                label: d.label.clone(),
                scopes: d.scopes.clone(),
                role: crate::contract::role_of(d.scopes.iter().map(String::as_str))
                    .map(str::to_string),
                enrolled_at: d.enrolled_at,
                revoked: d.revoked,
                linked: linked.contains(id),
            })
            .collect();
        out.sort_by(|a, b| {
            a.enrolled_at
                .cmp(&b.enrolled_at)
                .then_with(|| a.device_id.cmp(&b.device_id))
        });
        out
    }

    /// Retained lifecycle events after a cursor, the live-update primitive for an operator
    /// screen: keep the `next` of the last page and ask again with it. See
    /// [`EventLog::since`](crate::remote::EventLog::since) for the cursor rules (a cursor from
    /// another run comes back with `reset: true`).
    pub fn events_since(&self, epoch: Option<&str>, after: u64, limit: usize) -> EventsPage {
        self.events.since(epoch, after, limit)
    }

    /// [`events_since`](Self::events_since) that waits up to `timeout` (at most 60 s) for
    /// something newer than the cursor. Blocks the calling thread.
    pub fn wait_events(
        &self,
        epoch: Option<&str>,
        after: u64,
        limit: usize,
        timeout: std::time::Duration,
    ) -> EventsPage {
        self.events.wait_since(epoch, after, limit, timeout)
    }

    /// Take accepted webhook deliveries for processing: `(id, provider, body)`.
    pub fn drain_webhooks(&self) -> std::io::Result<Vec<(String, String, Vec<u8>)>> {
        let mut g = self.lock();
        let items: Vec<InboxItem> = std::mem::take(&mut g.state.webhook_inbox);
        self.save(&mut g)?;
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

    /// Rewrite `state.json` from memory and fsync it. A successful write leaves nothing dirty.
    fn save(&self, g: &mut Inner) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(&g.state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_atomic(&self.cfg.state_dir.join("state.json"), &bytes)?;
        g.dirty = false;
        let n = bytes.len() as u64;
        self.counters.state_saves.fetch_add(1, Ordering::Relaxed);
        self.counters.last_state_bytes.store(n, Ordering::Relaxed);
        self.counters
            .max_state_bytes
            .fetch_max(n, Ordering::Relaxed);
        Ok(())
    }

    /// Write `state.json` only when a change has not reached it yet.
    fn flush(&self, g: &mut Inner) -> std::io::Result<()> {
        if g.dirty {
            self.save(g)?;
        }
        Ok(())
    }

    /// Make the request's changes durable, then answer with `resp`. A failed write answers 500
    /// instead, so no acknowledged change is missing from disk.
    fn finish(&self, g: &mut Inner, resp: Response) -> Response {
        match self.flush(g) {
            Ok(()) => resp,
            Err(e) => self.storage_error(e),
        }
    }

    /// Request, write and retained-state figures plus process resource usage, read now. No polling
    /// thread runs for this.
    pub fn status(&self) -> DaemonStatus {
        DaemonStatus {
            metrics: self.metrics(),
            process: rightkit_child_process::process_usage().ok(),
        }
    }

    /// Request, write and retained-state figures for this daemon instance.
    pub fn metrics(&self) -> DaemonMetrics {
        let (links, tasks_queued, tasks_leased) = self.link_counts();
        let g = self.lock();
        let c = &self.counters;
        DaemonMetrics {
            requests: c.requests.load(Ordering::Relaxed),
            job_updates: c.job_updates.load(Ordering::Relaxed),
            state_saves: c.state_saves.load(Ordering::Relaxed),
            last_state_bytes: c.last_state_bytes.load(Ordering::Relaxed),
            max_state_bytes: c.max_state_bytes.load(Ordering::Relaxed),
            devices: g.state.devices.len() as u64,
            jobs: g.state.jobs.len() as u64,
            idempotency_records: g.state.idempotency.len() as u64,
            nonce_records: g.state.nonces.values().map(|m| m.len() as u64).sum(),
            events_retained: self.events.retained() as u64,
            links,
            tasks_queued,
            tasks_leased,
        }
    }

    fn save_upload(&self, up: &Upload) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(up)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_atomic(&self.meta_path(&up.id), &bytes)
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
        if Self::is_pair_path(path) {
            return self.cfg.max_json_body;
        }
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
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        // Short-code pairing (`/v1/pair`): not in the daemon-api route table, like `/v1/link`.
        if Self::is_pair_path(req.path()) {
            if req.body.len() > self.cfg.max_json_body {
                return Response::err(413, "body_too_large");
            }
            return self
                .handle_pairing(&req)
                .unwrap_or_else(|| Response::err(404, "not_found"));
        }
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
            AuthClassId::DeviceCall => self.handle_device(route, params, &req),
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
        if route.id == CALL_ROUTE_ID && self.calls.is_none() {
            return Response::err(404, "not_found");
        }
        let device = match self.authenticate(&mut g, route, req, now) {
            Ok(d) => d,
            Err(r) => return r,
        };
        match route.id {
            // The nonce is made durable, then the lock is released before the handler runs.
            CALL_ROUTE_ID => match self.flush(&mut g) {
                Ok(()) => {
                    let scopes = g
                        .state
                        .devices
                        .get(&device)
                        .map(|d| d.scopes.clone())
                        .unwrap_or_default();
                    drop(g);
                    self.device_call(&device, scopes, &params, req)
                }
                Err(e) => self.storage_error(e),
            },
            // Hashing runs outside the lock, so the nonce must be on disk before the lock is given up.
            "upload.complete" => match self.flush(&mut g) {
                Ok(()) => self.upload_complete(g, &device, &params, req, now),
                Err(e) => self.storage_error(e),
            },
            "job.update" => {
                let mut applied = None;
                let resp = self.job_update(&mut g, &device, &params, req, now, &mut applied);
                let resp = self.finish(&mut g, resp);
                // Counted and announced only when the change is on disk.
                if let (Some(revision), 200) = (applied, resp.status) {
                    self.counters.job_updates.fetch_add(1, Ordering::Relaxed);
                    self.emit(
                        "JOB_UPDATED",
                        json!({ "jobId": &params[0], "revision": revision, "deviceId": device }),
                    );
                }
                resp
            }
            "upload.init" => {
                let resp = self.upload_init(&mut g, &device, req, now);
                self.finish(&mut g, resp)
            }
            "upload.chunk" => {
                let resp = self.upload_chunk(&mut g, &device, &params, req, now);
                self.finish(&mut g, resp)
            }
            "upload.status" => {
                let resp = self.upload_status(&mut g, &device, &params, now);
                self.finish(&mut g, resp)
            }
            _ => self.finish(&mut g, Response::err(404, "not_found")),
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
        verify_request(
            key,
            &req.method,
            &req.path_and_query,
            ts,
            nonce,
            &req.body,
            sig,
        )
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
        // A device call's scope is checked per method against the method table instead.
        let allowed = route.id == CALL_ROUTE_ID
            || route.id == LINK_ROUTE_ID
            || route
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
        // Not written here: the request's `finish` (or `job_update`) writes it once, with the
        // handler's own change, before the answer leaves.
        g.dirty = true;
        Ok(device_id)
    }

    /// Run `POST /v1/call/{method}` for an authenticated device. A refusal by the method table
    /// is reported like any auth rejection (event `AUTH_REJECTED`), with the error-object body.
    fn device_call(
        &self,
        device: &str,
        scopes: Vec<String>,
        params: &[String],
        req: &Request,
    ) -> Response {
        let Some(calls) = self.calls.as_ref() else {
            return Response::err(404, "not_found");
        };
        let (status, body) = calls.run(device, scopes, &params[0], &req.body);
        if status == 403 && body["error"]["code"] == "forbidden" {
            self.emit(
                "AUTH_REJECTED",
                json!({
                    "reason": "method_not_permitted",
                    "routeId": CALL_ROUTE_ID,
                    "class": AuthClassId::DeviceCall,
                }),
            );
        }
        Response::new(status, body)
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
        let key: [u8; 32] = parse_digest_key(&up.key_hex).unwrap_or([0; 32]);
        let digest = chunk_digest(&key, &req.body);
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
        let key: [u8; 32] = parse_digest_key(&up.key_hex).unwrap_or([0; 32]);
        let size = up.size;
        drop(g); // hash outside the lock so other devices are not stalled

        let part = self.part_path(&id);
        let hashed = (|| -> std::io::Result<(String, u64)> {
            let mut f = File::open(&part)?;
            let len = f.metadata()?.len();
            let mut h = object_hasher(&key);
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok((object_digest(&h), len))
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
            // Opened for writing: Windows refuses to flush a read-only handle.
            OpenOptions::new().write(true).open(&part)?.sync_all()?;
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

    /// Apply a compare-and-swap job patch. Leaves the write to `handle_device`; a fresh update sets
    /// `applied` to its revision, a replay or a refusal leaves it `None`.
    fn job_update(
        &self,
        g: &mut Inner,
        device: &str,
        params: &[String],
        req: &Request,
        now: u64,
        applied: &mut Option<u64>,
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
        g.dirty = true;
        *applied = Some(revision);
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
        if let Err(e) = self.save(&mut g) {
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
