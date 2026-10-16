//! Host side of the outbound link (`rk-link/1`): the upgrade handshake, the table of live
//! connections, and the handling of every frame a device sends.
//!
//! The logic is transport-free. The HTTP adapter ([`crate::remote::http`]) upgrades the
//! connection, then pumps lines in (one [`Daemon::link_frame`] per line) and frames out
//! (the receiver of [`Accepted`]). Tests drive the same functions without a socket.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;

use super::{Daemon, Request, Response};
use crate::link::frame::{open_c2s, s2c_line, FrameError, HEARTBEAT_SECS, SCOPE, UPGRADE_TOKEN};
use crate::remote::routes::LINK_ROUTE;
use crate::remote::state::TaskStatus;
use crate::remote::util::{hex, random_bytes};

/// Something to write to a link's connection.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A frame body; the writer stamps `n` and the tag.
    Frame(Value),
    /// Send a `close` frame with this reason, then end the connection.
    Close(String),
}

/// What the adapter does after one inbound line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    Continue,
    /// End the connection; the reason is reported in `LINK_DOWN`.
    Close(String),
}

/// One live (or just-superseded) link connection of one device.
pub struct LinkSession {
    device: String,
    link_id: String,
    key: [u8; 32],
    public_key: Vec<u8>,
    tx: UnboundedSender<Out>,
    /// Sequence number of the last accepted machine-to-host frame.
    c2s: Mutex<u64>,
    /// Sequence number of the last host-to-machine frame written.
    s2c: Mutex<u64>,
    ready: AtomicBool,
    closed: AtomicBool,
    /// Wakes the reader of the connection when the session is closed from elsewhere.
    pub(crate) stop: Notify,
}

impl LinkSession {
    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn link_id(&self) -> &str {
        &self.link_id
    }

    /// Queue a frame body for the connection. Dropped once the session is closed.
    pub(crate) fn send(&self, frame: Value) {
        if !self.closed.load(Ordering::SeqCst) {
            let _ = self.tx.send(Out::Frame(frame));
        }
    }

    /// End the session: the connection writes a `close` frame and ends, its reader stops.
    /// Idempotent.
    pub fn close(&self, reason: &str) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Out::Close(reason.to_string()));
            self.stop.notify_one();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Number and tag a host-to-machine frame: the line to write. The sequence number is taken
    /// under a lock, so call it from the one writer in write order.
    pub fn encode(&self, mut frame: Value) -> String {
        let n = {
            let mut g = self.s2c.lock().unwrap_or_else(|e| e.into_inner());
            *g += 1;
            *g
        };
        frame["n"] = json!(n);
        s2c_line(&self.key, &self.link_id, &frame)
    }
}

/// The result of a successful upgrade request: the session, the receiver the connection writes
/// from, and the values for the `101` response.
pub struct Accepted {
    pub session: Arc<LinkSession>,
    pub(crate) rx: UnboundedReceiver<Out>,
    /// Connection id (response header `x-rk-link-id`).
    pub link_id: String,
    /// Host-to-machine session key, hex (response header `x-rk-link-key`).
    pub key_hex: String,
}

impl Accepted {
    /// Next queued item without waiting (for drivers that have no async runtime).
    pub fn try_next(&mut self) -> Option<Out> {
        self.rx.try_recv().ok()
    }

    pub fn into_parts(self) -> (Arc<LinkSession>, UnboundedReceiver<Out>) {
        (self.session, self.rx)
    }
}

impl Daemon {
    /// Authenticate an `rk-link/1` upgrade request and register the connection. The request is the
    /// ordinary signed device request (`GET /v1/link`, signature, nonce, timestamp); the device
    /// needs scope `link`. A second connection of the same device supersedes the first: the older
    /// session is closed with reason `superseded` and can complete nothing from then on.
    pub fn link_accept(&self, req: &Request) -> Result<Accepted, Response> {
        let route = &LINK_ROUTE;
        let upgrade = req
            .header("upgrade")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case(UPGRADE_TOKEN));
        let connection = req.header("connection").is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });
        if req.method != "GET" || !upgrade || !connection {
            return Err(self.reject(route, "bad_upgrade", 400, "bad_request"));
        }
        let now = self.clock.now_secs();
        let mut g = self.lock();
        let device = self.authenticate(&mut g, route, req, now)?;
        let Some(record) = g.state.devices.get(&device) else {
            return Err(self.reject(route, "unknown_device", 401, "unauthenticated"));
        };
        if !record.scopes.iter().any(|s| s == SCOPE) {
            return Err(self.reject(route, "insufficient_scope", 403, "forbidden"));
        }
        let public_key = URL_SAFE_NO_PAD
            .decode(&record.public_key)
            .unwrap_or_default();
        // The nonce is durable before the connection exists.
        if let Err(e) = self.flush(&mut g) {
            return Err(self.storage_error(e));
        }
        drop(g);

        let link_id = hex(&random_bytes::<16>());
        let key = random_bytes::<32>();
        let (tx, rx) = unbounded_channel();
        let session = Arc::new(LinkSession {
            device: device.clone(),
            link_id: link_id.clone(),
            key,
            public_key,
            tx,
            c2s: Mutex::new(0),
            s2c: Mutex::new(0),
            ready: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            stop: Notify::new(),
        });
        let old = self
            .links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(device.clone(), session.clone());
        if let Some(old) = old {
            old.close("superseded");
            self.emit(
                "LINK_DOWN",
                json!({ "deviceId": device, "linkId": old.link_id, "reason": "superseded" }),
            );
        }
        Ok(Accepted {
            session,
            rx,
            link_id,
            key_hex: hex(&key),
        })
    }

    /// Whether `s` is the current connection of its device (not superseded or closed).
    pub(super) fn link_is_current(&self, s: &Arc<LinkSession>) -> bool {
        !s.is_closed()
            && self
                .links
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&s.device)
                .is_some_and(|c| Arc::ptr_eq(c, s))
    }

    /// Sessions that completed `hello`, in device order.
    pub(super) fn ready_links(&self) -> Vec<Arc<LinkSession>> {
        self.links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|s| s.ready.load(Ordering::SeqCst) && !s.is_closed())
            .cloned()
            .collect()
    }

    /// Devices with a live, ready link.
    pub fn linked_devices(&self) -> Vec<String> {
        self.ready_links()
            .iter()
            .map(|s| s.device.clone())
            .collect()
    }

    /// Close the current connection of `device` (operator action; the device may reconnect).
    pub fn close_link(&self, device: &str, reason: &str) -> bool {
        let s = self
            .links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(device)
            .cloned();
        match s {
            Some(s) => {
                s.close(reason);
                true
            }
            None => false,
        }
    }

    /// Close every link (server shutdown).
    pub fn close_all_links(&self, reason: &str) {
        let all: Vec<Arc<LinkSession>> = self
            .links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for s in all {
            s.close(reason);
        }
    }

    /// The connection ended: close the session and, if it was the device's current one, drop it
    /// from the table (`LINK_DOWN`). Leases are kept; the device resumes them on reconnect.
    pub fn link_closed(&self, s: &Arc<LinkSession>, reason: &str) {
        s.close(reason);
        let was_current = {
            let mut l = self.links.lock().unwrap_or_else(|e| e.into_inner());
            let current = l.get(&s.device).is_some_and(|c| Arc::ptr_eq(c, s));
            if current {
                l.remove(&s.device);
            }
            current
        };
        if was_current {
            self.emit(
                "LINK_DOWN",
                json!({ "deviceId": s.device, "linkId": s.link_id, "reason": reason }),
            );
        }
    }

    /// Periodic housekeeping, called once per heartbeat by a connection's writer: re-queue lapsed
    /// leases (once each) and dispatch whatever is now ready. There is no timer thread.
    pub fn link_tick(&self) {
        let _ = self.tasks_sweep();
    }

    fn frame_refused(&self, s: &Arc<LinkSession>, reason: &str) -> FrameOutcome {
        self.emit(
            "LINK_FRAME_REJECTED",
            json!({ "deviceId": s.device, "linkId": s.link_id, "reason": reason }),
        );
        FrameOutcome::Close(reason.to_string())
    }

    /// Handle one inbound line from the device behind `s`. Every line is authenticated (device
    /// signature bound to this connection), must carry the next sequence number, and is refused
    /// (ending the connection) when the session has been superseded.
    pub fn link_frame(&self, s: &Arc<LinkSession>, line: &str) -> FrameOutcome {
        if !self.link_is_current(s) {
            return FrameOutcome::Close("superseded".into());
        }
        let v = match open_c2s(&s.public_key, &s.link_id, line) {
            Ok(v) => v,
            Err(FrameError::BadTag) => return self.frame_refused(s, "bad_tag"),
            Err(FrameError::Malformed) => return self.frame_refused(s, "malformed"),
        };
        let Some(n) = v.get("n").and_then(Value::as_u64) else {
            return self.frame_refused(s, "malformed");
        };
        {
            let mut last = s.c2s.lock().unwrap_or_else(|e| e.into_inner());
            let expected = *last + 1;
            if n != expected {
                drop(last);
                let why = if n < expected {
                    "replayed"
                } else {
                    "sequence_gap"
                };
                return self.frame_refused(s, why);
            }
            *last = n;
        }
        let t = v.get("t").and_then(Value::as_str).unwrap_or("");
        let ready = s.ready.load(Ordering::SeqCst);
        match (t, ready) {
            ("hello", false) => self.on_hello(s, &v),
            ("hello", true) => self.frame_refused(s, "duplicate_hello"),
            (_, false) => self.frame_refused(s, "expected_hello"),
            ("ping", true) => self.on_alive(s),
            ("ack", true) => self.on_ack(s, &v),
            ("result", true) => self.on_result(s, &v),
            ("request", true) => self.on_request(s, &v),
            _ => self.frame_refused(s, "unknown_frame"),
        }
    }

    /// A valid frame arrived: the device is alive, so its leases are renewed. A revoked device is
    /// cut off.
    fn on_alive(&self, s: &Arc<LinkSession>) -> FrameOutcome {
        if self.touch_leases(&s.device) {
            FrameOutcome::Continue
        } else {
            FrameOutcome::Close("revoked".into())
        }
    }

    fn on_hello(&self, s: &Arc<LinkSession>, v: &Value) -> FrameOutcome {
        if v.get("device").and_then(Value::as_str) != Some(s.device.as_str()) {
            return self.frame_refused(s, "bad_hello");
        }
        let resume_seq = v.get("resume_seq").and_then(Value::as_u64).unwrap_or(0);
        if !self.touch_leases(&s.device) {
            return FrameOutcome::Close("revoked".into());
        }
        s.ready.store(true, Ordering::SeqCst);
        self.emit(
            "LINK_UP",
            json!({ "deviceId": s.device, "linkId": s.link_id, "resumeSeq": resume_seq }),
        );
        s.send(json!({
            "t": "welcome",
            "device": s.device,
            "link_id": s.link_id,
            "heartbeat_secs": HEARTBEAT_SECS,
        }));
        self.resume_tasks(s);
        self.dispatch();
        FrameOutcome::Continue
    }

    fn on_ack(&self, s: &Arc<LinkSession>, v: &Value) -> FrameOutcome {
        let (Some(id), Some(epoch)) = (
            v.get("id").and_then(Value::as_str),
            v.get("epoch").and_then(Value::as_u64),
        ) else {
            return self.frame_refused(s, "malformed");
        };
        if !self.touch_leases(&s.device) {
            return FrameOutcome::Close("revoked".into());
        }
        if let Err(code) = self.ack_task(s, id, epoch) {
            s.send(json!({
                "t": "settled", "id": id, "epoch": epoch, "state": "refused", "code": code,
            }));
        }
        FrameOutcome::Continue
    }

    fn on_result(&self, s: &Arc<LinkSession>, v: &Value) -> FrameOutcome {
        let (Some(id), Some(epoch), Some(ok)) = (
            v.get("id").and_then(Value::as_str),
            v.get("epoch").and_then(Value::as_u64),
            v.get("ok").and_then(Value::as_bool),
        ) else {
            return self.frame_refused(s, "malformed");
        };
        if !self.touch_leases(&s.device) {
            return FrameOutcome::Close("revoked".into());
        }
        let outcome = if ok {
            json!({ "ok": true, "result": v.get("result").cloned().unwrap_or(Value::Null) })
        } else {
            json!({ "ok": false, "error": v.get("error").cloned().unwrap_or(Value::Null) })
        };
        let (state, code) = self.complete_task(s, id, epoch, ok, outcome);
        let mut reply = json!({ "t": "settled", "id": id, "epoch": epoch, "state": state });
        if let Some(code) = code {
            reply["code"] = json!(code);
        }
        s.send(reply);
        FrameOutcome::Continue
    }

    /// A machine-initiated call: the ordinary device contract, run for this device.
    fn on_request(&self, s: &Arc<LinkSession>, v: &Value) -> FrameOutcome {
        let (Some(id), Some(method)) = (
            v.get("id").and_then(Value::as_str),
            v.get("method").and_then(Value::as_str),
        ) else {
            return self.frame_refused(s, "malformed");
        };
        if !self.touch_leases(&s.device) {
            return FrameOutcome::Close("revoked".into());
        }
        let (status, body) = match self.calls.as_ref() {
            None => (404, json!({ "error": { "code": "not_found" } })),
            Some(calls) => {
                let scopes = self
                    .lock()
                    .state
                    .devices
                    .get(&s.device)
                    .map(|d| d.scopes.clone())
                    .unwrap_or_default();
                let params = v.get("params").cloned().unwrap_or_else(|| json!({}));
                let bytes = serde_json::to_vec(&params).unwrap_or_default();
                calls.run(&s.device, scopes, method, &bytes)
            }
        };
        s.send(json!({ "t": "response", "id": id, "status": status, "body": body }));
        FrameOutcome::Continue
    }

    /// Number of live links and tasks per state, for status.
    pub(super) fn link_counts(&self) -> (u64, u64, u64) {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner()).len() as u64;
        let g = self.lock();
        let queued = g
            .state
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Queued)
            .count() as u64;
        let leased = g
            .state
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Leased)
            .count() as u64;
        (links, queued, leased)
    }
}
