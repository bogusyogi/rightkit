//! Short-code pairing, host side (`rk-pair/1`). The protocol and its security argument are in
//! [`crate::remote::pairing`]. Here: the owner opens a pairing window in process, a joining
//! device runs the three unauthenticated requests below, the owner types the code shown on the
//! device into the host, and only a matching code enrols the device's key.
//!
//! * `POST /v1/pair`: `{publicKey, commit, label?}` -> `201 {pairingId, nonce, expiresAt}`.
//! * `POST /v1/pair/{id}/reveal`: `{nonce}` -> `200 {state}`.
//! * `GET /v1/pair/{id}` -> `200 {state, deviceId?}`, `state` one of `committed`,
//!   `awaiting_confirmation`, `confirmed`, `rejected`, `failed`.
//!
//! These routes are unauthenticated by nature (the device has no credential yet), so every
//! bound below is enforced here and none is left to the caller:
//!
//! * nothing is served unless the owner opened a window ([`Daemon::open_pairing`]); it expires,
//!   serves one successful pairing, and is gone after a restart (pairing state is memory only);
//! * a window accepts at most `max_begins` sessions, `max_pending` at once, each living
//!   `session_secs`;
//! * a failure (commitment mismatch, wrong code) burns its session, and `max_failures` in one
//!   window, or `max_wrong_codes` against one session, ends it: an attacker gets a handful of
//!   1-in-10^6 guesses, then the owner must open a new window;
//! * the code never appears in an event, a response or the log: only
//!   [`Daemon::pairing_code`], in process, returns it, and enrolment needs the owner to type the
//!   code read from the device.
//!
//! Lock order: the pairing lock, then the state lock, never the other way round.

use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};

use super::{Daemon, Request, Response};
use crate::remote::pairing::{
    pairing_code, pairing_commit, CODE_DIGITS, MAX_LABEL_CHARS, NONCE_BYTES,
};
use crate::remote::state::Device;
use crate::remote::util::{ct_eq, hex, random_bytes, sha256_hex, unhex};

/// How a pairing window is opened. Defaults are small on purpose.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PairingOptions {
    /// Scopes the enrolled device gets.
    pub scopes: Vec<String>,
    /// How long the window stays open, in seconds.
    pub window_secs: u64,
    /// Bytes mixed into the code that both ends must know, for example a hash of the TLS
    /// certificate the device pinned. Empty when there is none.
    pub host_binding: Vec<u8>,
    /// How long one session lives from its start, in seconds.
    pub session_secs: u64,
    /// Sessions that may be open at once.
    pub max_pending: usize,
    /// Sessions the window accepts in total.
    pub max_begins: u32,
    /// Failures (all sessions) after which the window closes.
    pub max_failures: u32,
    /// Wrong codes against one session after which it is burned.
    pub max_wrong_codes: u32,
}

impl PairingOptions {
    /// A window of `window_secs` that enrols one device with `scopes`.
    pub fn new(scopes: &[&str], window_secs: u64) -> Self {
        Self {
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            window_secs,
            host_binding: Vec::new(),
            session_secs: 120,
            max_pending: 3,
            max_begins: 10,
            max_failures: 5,
            max_wrong_codes: 3,
        }
    }

    /// Mix `binding` into the code (see [`PairingOptions::host_binding`]).
    pub fn with_host_binding(mut self, binding: impl Into<Vec<u8>>) -> Self {
        self.host_binding = binding.into();
        self
    }
}

/// Where a pairing session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairingStage {
    /// The device sent its commitment and has not revealed its nonce.
    Committed,
    /// The nonce matched; the code can be read ([`Daemon::pairing_code`]) and confirmed.
    AwaitingConfirmation,
    /// The device was enrolled.
    Confirmed,
    /// The owner declined.
    Rejected,
    /// A check failed or the session or window ended.
    Failed,
}

impl PairingStage {
    fn wire(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
    fn open(self) -> bool {
        matches!(self, Self::Committed | Self::AwaitingConfirmation)
    }
}

/// A session as the owner sees it. No code, no nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingInfo {
    pub id: String,
    pub label: Option<String>,
    /// The id the device would be enrolled under.
    pub device_id: String,
    pub stage: PairingStage,
    pub expires_at: u64,
}

/// A device enrolled by [`Daemon::confirm_pairing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedDevice {
    pub device_id: String,
    pub scopes: Vec<String>,
    pub label: Option<String>,
}

/// Why an administrative pairing call did not do its job.
#[derive(Debug)]
#[non_exhaustive]
pub enum PairingError {
    /// A window is already open; close it first.
    AlreadyOpen,
    /// The options are unusable (no scopes, or a zero limit).
    InvalidOptions(&'static str),
    /// No window is open (never opened, expired, used, locked or closed).
    Closed,
    /// No such session, or it expired.
    UnknownSession,
    /// The device has not revealed its nonce yet, so there is no code to check.
    NotReady,
    /// The typed code is wrong. `remaining` further tries are left on this session.
    WrongCode { remaining: u32 },
    /// Too many wrong codes: the session is burned.
    SessionBurned,
    /// The key is already a device here (revoked or not). Forget it first to pair it again.
    AlreadyEnrolled,
    /// The state could not be written; nothing was enrolled.
    Storage(std::io::Error),
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyOpen => write!(f, "a pairing window is already open"),
            Self::InvalidOptions(why) => write!(f, "invalid pairing options: {why}"),
            Self::Closed => write!(f, "no pairing window is open"),
            Self::UnknownSession => write!(f, "no such pairing session"),
            Self::NotReady => write!(f, "the device has not completed its half yet"),
            Self::WrongCode { remaining } => {
                write!(f, "the code does not match ({remaining} tries left)")
            }
            Self::SessionBurned => write!(f, "too many wrong codes; the session is closed"),
            Self::AlreadyEnrolled => write!(f, "that key is already a device here"),
            Self::Storage(e) => write!(f, "could not write the daemon state: {e}"),
        }
    }
}

impl std::error::Error for PairingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            _ => None,
        }
    }
}

struct Window {
    opts: PairingOptions,
    expires_at: u64,
    begins: u32,
    failures: u32,
}

struct Session {
    public_key: Vec<u8>,
    label: Option<String>,
    commit: Vec<u8>,
    nonce_h: [u8; NONCE_BYTES],
    code: Option<String>,
    stage: PairingStage,
    device_id: Option<String>,
    wrong: u32,
    expires_at: u64,
}

/// Pairing state. Memory only.
#[derive(Default)]
pub(super) struct PairingState {
    window: Option<Window>,
    sessions: BTreeMap<String, Session>,
}

/// How long a finished session stays readable by the device's `GET`.
const FINISHED_KEEP_SECS: u64 = 60;
const MAX_PAIR_PATH_ID: usize = 64;

impl PairingState {
    /// Drop dead sessions and lapse an expired window. Returns whether the window lapsed.
    fn tidy(&mut self, now: u64) -> bool {
        self.sessions.retain(|_, s| s.expires_at >= now);
        let lapsed = self.window.as_ref().is_some_and(|w| w.expires_at < now);
        if lapsed {
            self.window = None;
            self.fail_open_sessions();
        }
        lapsed
    }

    fn fail_open_sessions(&mut self) {
        for s in self.sessions.values_mut() {
            if s.stage.open() {
                s.stage = PairingStage::Failed;
                s.code = None;
            }
        }
    }

    /// Count a failure; close the window when the budget is spent. Returns whether it closed.
    fn record_failure(&mut self) -> bool {
        let Some(w) = self.window.as_mut() else {
            return false;
        };
        w.failures += 1;
        if w.failures >= w.opts.max_failures {
            self.window = None;
            self.fail_open_sessions();
            return true;
        }
        false
    }

    fn burn(&mut self, id: &str, now: u64) {
        if let Some(s) = self.sessions.get_mut(id) {
            s.stage = PairingStage::Failed;
            s.code = None;
            s.expires_at = s.expires_at.min(now + FINISHED_KEEP_SECS);
        }
    }
}

/// Normalise what a person typed: keep digits only.
fn digits_only(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

impl Daemon {
    fn pairing_lock(&self) -> std::sync::MutexGuard<'_, PairingState> {
        self.pairing.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn announce_lapse(&self, lapsed: bool) {
        if lapsed {
            self.emit("PAIRING_CLOSED", json!({ "reason": "expired" }));
        }
    }

    // ---- administration (in-process, never exposed on a route) -------------

    /// Open the pairing window. In-process administration only: no route reaches it.
    pub fn open_pairing(&self, opts: PairingOptions) -> Result<(), PairingError> {
        if opts.scopes.is_empty() {
            return Err(PairingError::InvalidOptions("no scopes"));
        }
        if opts.window_secs == 0
            || opts.session_secs == 0
            || opts.max_pending == 0
            || opts.max_begins == 0
            || opts.max_failures == 0
            || opts.max_wrong_codes == 0
        {
            return Err(PairingError::InvalidOptions("a limit is zero"));
        }
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        self.announce_lapse(lapsed);
        if p.window.is_some() {
            return Err(PairingError::AlreadyOpen);
        }
        let (scopes, window_secs) = (opts.scopes.clone(), opts.window_secs);
        p.window = Some(Window {
            expires_at: now + opts.window_secs,
            opts,
            begins: 0,
            failures: 0,
        });
        drop(p);
        self.emit(
            "PAIRING_OPENED",
            json!({ "scopes": scopes, "windowSecs": window_secs }),
        );
        Ok(())
    }

    /// Close the window and fail every open session. `true` when one was open.
    pub fn close_pairing(&self) -> bool {
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        let was = p.window.take().is_some();
        p.fail_open_sessions();
        drop(p);
        self.announce_lapse(lapsed);
        if was {
            self.emit("PAIRING_CLOSED", json!({ "reason": "closed" }));
        }
        was
    }

    /// The sessions in progress, for the owner to choose from.
    pub fn pending_pairings(&self) -> Vec<PairingInfo> {
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        let out: Vec<PairingInfo> = p
            .sessions
            .iter()
            .filter(|(_, s)| s.stage.open())
            .map(|(id, s)| PairingInfo {
                id: id.clone(),
                label: s.label.clone(),
                device_id: format!("dev_{}", &sha256_hex(&s.public_key)[..16]),
                stage: s.stage,
                expires_at: s.expires_at,
            })
            .collect();
        drop(p);
        self.announce_lapse(lapsed);
        out
    }

    /// The code this host computed for a session whose device revealed its nonce: show it on
    /// the host so the owner can compare it with the device's. `None` before the reveal and
    /// after the session ended. In-process only; never put it in a log or an event.
    pub fn pairing_code(&self, id: &str) -> Option<String> {
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        let code = p
            .sessions
            .get(id)
            .filter(|s| s.stage == PairingStage::AwaitingConfirmation)
            .and_then(|s| s.code.clone());
        drop(p);
        self.announce_lapse(lapsed);
        code
    }

    /// The owner decides: `typed` is the code read off the device's screen. A match enrols the
    /// device's key with the window's scopes and closes the window (one pairing per window); a
    /// mismatch costs a try, and `max_wrong_codes` of them burn the session. The comparison is
    /// constant time.
    pub fn confirm_pairing(&self, id: &str, typed: &str) -> Result<PairedDevice, PairingError> {
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        let had_window = p.window.is_some();
        let result = self.confirm_locked(&mut p, id, typed, now);
        let locked = had_window && result.is_err() && p.window.is_none();
        drop(p);
        self.announce_lapse(lapsed);
        match &result {
            Ok(dev) => {
                self.emit(
                    "PAIRING_CONFIRMED",
                    json!({ "pairingId": id, "deviceId": dev.device_id }),
                );
                self.emit(
                    "ENROLLED",
                    json!({ "deviceId": dev.device_id, "scopes": dev.scopes }),
                );
            }
            Err(PairingError::WrongCode { .. }) => {
                self.emit(
                    "PAIRING_FAILED",
                    json!({ "pairingId": id, "reason": "wrong_code" }),
                );
            }
            Err(PairingError::SessionBurned) => {
                self.emit(
                    "PAIRING_FAILED",
                    json!({ "pairingId": id, "reason": "wrong_code_limit" }),
                );
            }
            _ => {}
        }
        if locked {
            self.emit("PAIRING_CLOSED", json!({ "reason": "locked" }));
        }
        result
    }

    fn confirm_locked(
        &self,
        p: &mut PairingState,
        id: &str,
        typed: &str,
        now: u64,
    ) -> Result<PairedDevice, PairingError> {
        let Some(window) = p.window.as_ref() else {
            return Err(PairingError::Closed);
        };
        let (max_wrong, scopes) = (window.opts.max_wrong_codes, window.opts.scopes.clone());
        let Some(s) = p.sessions.get_mut(id) else {
            return Err(PairingError::UnknownSession);
        };
        match s.stage {
            PairingStage::Committed => return Err(PairingError::NotReady),
            PairingStage::AwaitingConfirmation => {}
            _ => return Err(PairingError::UnknownSession),
        }
        let want = s.code.clone().unwrap_or_default();
        let got = digits_only(typed);
        if got.len() != CODE_DIGITS || !ct_eq(got.as_bytes(), want.as_bytes()) {
            s.wrong += 1;
            let wrong = s.wrong;
            if wrong >= max_wrong {
                p.burn(id, now);
                p.record_failure();
                return Err(PairingError::SessionBurned);
            }
            p.record_failure();
            return Err(PairingError::WrongCode {
                remaining: max_wrong - wrong,
            });
        }
        // The code matched: enrol. State lock after pairing lock.
        let key = s.public_key.clone();
        let label = s.label.clone();
        let device_id = format!("dev_{}", &sha256_hex(&key)[..16]);
        {
            let mut g = self.lock();
            if g.state.devices.contains_key(&device_id) {
                return Err(PairingError::AlreadyEnrolled);
            }
            g.state.devices.insert(
                device_id.clone(),
                Device {
                    public_key: URL_SAFE_NO_PAD.encode(&key),
                    label: label.clone(),
                    scopes: scopes.clone(),
                    enrolled_at: now,
                    revoked: false,
                },
            );
            if let Err(e) = self.save(&mut g) {
                g.state.devices.remove(&device_id);
                g.dirty = true;
                return Err(PairingError::Storage(e));
            }
        }
        if let Some(s) = p.sessions.get_mut(id) {
            s.stage = PairingStage::Confirmed;
            s.code = None;
            s.device_id = Some(device_id.clone());
            s.expires_at = now + FINISHED_KEEP_SECS;
        }
        // One pairing per window.
        p.window = None;
        p.fail_open_sessions();
        Ok(PairedDevice {
            device_id,
            scopes,
            label,
        })
    }

    /// The owner declines a session (a stranger, or a code that did not match). `true` when a
    /// session in progress was ended.
    pub fn reject_pairing(&self, id: &str) -> bool {
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        let mut done = false;
        if let Some(s) = p.sessions.get_mut(id) {
            if s.stage.open() {
                s.stage = PairingStage::Rejected;
                s.code = None;
                s.expires_at = s.expires_at.min(now + FINISHED_KEEP_SECS);
                done = true;
            }
        }
        drop(p);
        self.announce_lapse(lapsed);
        if done {
            self.emit(
                "PAIRING_FAILED",
                json!({ "pairingId": id, "reason": "rejected" }),
            );
        }
        done
    }

    // ---- the routes ----------------------------------------------------------

    /// Whether `path` belongs to the pairing routes (`/v1/pair`, `/v1/pair/...`).
    pub(super) fn is_pair_path(path: &str) -> bool {
        path == "/v1/pair" || path.starts_with("/v1/pair/")
    }

    /// Serve a pairing route. `None` when the path is not one.
    pub(super) fn handle_pairing(&self, req: &Request) -> Option<Response> {
        let path = req.path();
        if !Self::is_pair_path(path) {
            return None;
        }
        let segs: Vec<&str> = path.trim_start_matches("/v1/pair").split('/').collect();
        // `[""]` for /v1/pair; `["", id]` for /v1/pair/{id}; `["", id, "reveal"]`.
        let post = req.method == "POST";
        let resp = match segs.as_slice() {
            [""] if post => self.pair_begin(req),
            ["", id] if valid_id(id) && req.method == "GET" => self.pair_status(id),
            ["", id, "reveal"] if valid_id(id) && post => self.pair_reveal(id, req),
            [""] => Response::err(405, "method_not_allowed"),
            ["", id] | ["", id, "reveal"] if valid_id(id) => {
                Response::err(405, "method_not_allowed")
            }
            _ => Response::err(404, "not_found"),
        };
        Some(resp)
    }

    fn pair_begin(&self, req: &Request) -> Response {
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
        let commit = match body.get("commit").and_then(Value::as_str).and_then(unhex) {
            Some(c) if c.len() == 32 => c,
            _ => return Response::err(400, "bad_request"),
        };
        let label = match body.get("label") {
            None | Some(Value::Null) => None,
            Some(Value::String(l)) if l.chars().count() <= MAX_LABEL_CHARS => Some(l.clone()),
            _ => return Response::err(400, "bad_request"),
        };
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        self.announce_lapse(lapsed);
        let open = p.sessions.values().filter(|s| s.stage.open()).count();
        let Some(w) = p.window.as_mut() else {
            return Response::err(403, "pairing_closed");
        };
        if w.begins >= w.opts.max_begins {
            return Response::err(429, "too_many_attempts");
        }
        if open >= w.opts.max_pending {
            return Response::err(429, "too_many_pending");
        }
        w.begins += 1;
        let expires_at = (now + w.opts.session_secs).min(w.expires_at);
        let id = format!("pr_{}", hex(&random_bytes::<16>()));
        let nonce_h = random_bytes::<NONCE_BYTES>();
        p.sessions.insert(
            id.clone(),
            Session {
                public_key: key,
                label,
                commit,
                nonce_h,
                code: None,
                stage: PairingStage::Committed,
                device_id: None,
                wrong: 0,
                expires_at,
            },
        );
        drop(p);
        self.emit("PAIRING_STARTED", json!({ "pairingId": id }));
        Response::new(
            201,
            json!({ "pairingId": id, "nonce": hex(&nonce_h), "expiresAt": expires_at }),
        )
    }

    fn pair_reveal(&self, id: &str, req: &Request) -> Response {
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(_) => return Response::err(400, "bad_request"),
        };
        let nonce_d = match body.get("nonce").and_then(Value::as_str).and_then(unhex) {
            Some(n) if n.len() == NONCE_BYTES => n,
            _ => return Response::err(400, "bad_request"),
        };
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        self.announce_lapse(lapsed);
        let host_binding = match p.window.as_ref() {
            Some(w) => w.opts.host_binding.clone(),
            None => {
                return match p.sessions.get(id) {
                    Some(_) => Response::err(409, "bad_state"),
                    None => Response::err(404, "not_found"),
                }
            }
        };
        let Some(s) = p.sessions.get_mut(id) else {
            return Response::err(404, "not_found");
        };
        if s.stage != PairingStage::Committed {
            return Response::err(409, "bad_state");
        }
        let expected = pairing_commit(&s.public_key, &nonce_d, s.label.as_deref());
        if !ct_eq(expected.as_bytes(), hex(&s.commit).as_bytes()) {
            p.burn(id, now);
            let locked = p.record_failure();
            drop(p);
            self.emit(
                "PAIRING_FAILED",
                json!({ "pairingId": id, "reason": "commit_mismatch" }),
            );
            if locked {
                self.emit("PAIRING_CLOSED", json!({ "reason": "locked" }));
            }
            return Response::err(403, "commit_mismatch");
        }
        s.code = Some(pairing_code(
            &s.public_key,
            &nonce_d,
            &s.nonce_h,
            s.label.as_deref(),
            &host_binding,
        ));
        s.stage = PairingStage::AwaitingConfirmation;
        let label = s.label.clone();
        drop(p);
        self.emit("PAIRING_READY", json!({ "pairingId": id, "label": label }));
        Response::new(
            200,
            json!({ "state": PairingStage::AwaitingConfirmation.wire() }),
        )
    }

    fn pair_status(&self, id: &str) -> Response {
        let now = self.clock.now_secs();
        let mut p = self.pairing_lock();
        let lapsed = p.tidy(now);
        self.announce_lapse(lapsed);
        let Some(s) = p.sessions.get(id) else {
            return Response::err(404, "not_found");
        };
        let mut body = json!({ "state": s.stage.wire() });
        if s.stage == PairingStage::Confirmed {
            body["deviceId"] = json!(s.device_id);
        }
        Response::new(200, body)
    }
}

/// A pairing id as issued: `pr_` and 32 hex digits. Anything else is not a session.
fn valid_id(id: &str) -> bool {
    id.len() <= MAX_PAIR_PATH_ID
        && id
            .strip_prefix("pr_")
            .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}
