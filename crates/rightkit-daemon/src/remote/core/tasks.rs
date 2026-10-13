//! Durable link tasks: `queued -> leased(device, epoch, expires) -> done | failed`.
//!
//! Invariants, all enforced here under the daemon's state lock:
//!
//! * One owner per task at a time. A lease binds the task to one device and an `epoch` that
//!   grows by one on every grant.
//! * A result is accepted only from the session that is its device's current connection, for
//!   the task's current lease epoch. Anything else is refused (`superseded`, `lease_lost`).
//!   The same `(task, epoch)` result delivered twice is a no-op (`duplicate`).
//! * A lease lapses only when `expires` passes without any frame from its owner (every valid
//!   frame renews it). The sweep that lapses it changes the task to `queued` in the same step
//!   it observes it as `leased`, so a lapse re-queues exactly once; the next grant bumps the
//!   epoch, so the old holder's late result is fenced.
//! * A disconnect releases nothing. The device resumes with `resume_seq` and gets the leased
//!   tasks it has not durably accepted again.
//!
//! The lease is persisted (`state.json`) before the `task` frame is sent. Lease renewals are not
//! persisted: after a host restart a lease is as old as its last write and lapses soon, which
//! errs towards re-running a task, never towards losing it.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::link::LinkSession;
use super::{Daemon, Inner};
use crate::remote::state::{Task, TaskStatus};
use crate::remote::util::{hex, random_bytes};

/// Tasks held (queued or leased) at most; finished tasks are kept for a day for idempotent
/// result delivery, then dropped.
const MAX_OPEN_TASKS: usize = 10_000;
const FINISHED_RETENTION_SECS: u64 = 86_400;
const MAX_LEASE_TTL_SECS: u64 = 86_400;
const DEFAULT_LEASE_TTL_SECS: u64 = 120;

/// A task to hand to a device.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub method: String,
    pub params: Value,
    /// Pin to one enrolled device; `None` picks the least loaded linked device.
    pub target: Option<String>,
    pub lease_ttl_secs: u64,
}

impl TaskSpec {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
            target: None,
            lease_ttl_secs: DEFAULT_LEASE_TTL_SECS,
        }
    }

    pub fn target(mut self, device_id: impl Into<String>) -> Self {
        self.target = Some(device_id.into());
        self
    }

    pub fn lease_ttl(mut self, secs: u64) -> Self {
        self.lease_ttl_secs = secs;
        self
    }
}

/// A snapshot of one task.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskInfo {
    pub id: String,
    pub seq: u64,
    pub status: TaskStatus,
    pub owner: Option<String>,
    pub epoch: u64,
    /// While leased: when the lease lapses (unix seconds). Once finished: when it finished.
    pub expires: u64,
    pub acked: bool,
    pub outcome: Option<Value>,
}

fn info(t: &Task) -> TaskInfo {
    TaskInfo {
        id: t.id.clone(),
        seq: t.seq,
        status: t.status,
        owner: t.owner.clone(),
        epoch: t.epoch,
        expires: t.expires,
        acked: t.acked,
        outcome: t.outcome.clone(),
    }
}

fn task_frame(t: &Task) -> Value {
    json!({
        "t": "task",
        "id": t.id,
        "seq": t.seq,
        "epoch": t.epoch,
        "method": t.method,
        "params": t.params,
        "lease_ttl": t.lease_ttl,
    })
}

/// A lapsed lease: `(task id, owner, epoch)`.
type Lapsed = (String, String, u64);

impl Daemon {
    /// Queue a task. It is leased and sent as soon as a linked device can take it.
    pub fn enqueue_task(&self, spec: TaskSpec) -> std::io::Result<String> {
        let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, m.to_string());
        if spec.method.is_empty()
            || spec.method.len() > 128
            || !spec.method.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(bad("task method must be 1-128 printable ASCII characters"));
        }
        let ttl = spec.lease_ttl_secs.clamp(1, MAX_LEASE_TTL_SECS);
        let now = self.clock.now_secs();
        let mut g = self.lock();
        if let Some(target) = &spec.target {
            let ok = g
                .state
                .devices
                .get(target)
                .is_some_and(|d| !d.revoked && d.scopes.iter().any(|s| s == "link"));
            if !ok {
                return Err(bad("target is not an enrolled, unrevoked link device"));
            }
        }
        let open = g
            .state
            .tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Queued | TaskStatus::Leased))
            .count();
        if open >= MAX_OPEN_TASKS {
            return Err(std::io::Error::other("task queue is full"));
        }
        g.state.task_seq += 1;
        let seq = g.state.task_seq;
        let id = format!("tsk_{}", hex(&random_bytes::<8>()));
        g.state.tasks.insert(
            id.clone(),
            Task {
                id: id.clone(),
                seq,
                target: spec.target.clone(),
                method: spec.method,
                params: spec.params,
                lease_ttl: ttl,
                status: TaskStatus::Queued,
                owner: None,
                epoch: 0,
                expires: 0,
                acked: false,
                created_at: now,
                outcome: None,
            },
        );
        if let Err(e) = self.save(&mut g) {
            // Not durable, so not queued.
            g.state.tasks.remove(&id);
            return Err(e);
        }
        drop(g);
        self.emit(
            "TASK_QUEUED",
            json!({ "taskId": id, "seq": seq, "target": spec.target }),
        );
        self.dispatch();
        Ok(id)
    }

    /// A snapshot of one task.
    pub fn task(&self, id: &str) -> Option<TaskInfo> {
        self.lock().state.tasks.get(id).map(info)
    }

    /// Tasks in `seq` order.
    pub fn tasks(&self) -> Vec<TaskInfo> {
        let g = self.lock();
        let mut v: Vec<TaskInfo> = g.state.tasks.values().map(info).collect();
        v.sort_by_key(|t| t.seq);
        v
    }

    /// Lapse every lease whose time has passed and dispatch what became ready. Returns how many
    /// leases lapsed. A lapsed lease is `queued` from this call on, so a second call lapses
    /// nothing.
    pub fn tasks_sweep(&self) -> std::io::Result<usize> {
        let now = self.clock.now_secs();
        let mut g = self.lock();
        let lapsed = self.sweep_locked(&mut g, now);
        self.flush(&mut g)?;
        drop(g);
        let n = lapsed.len();
        self.announce_lapsed(lapsed);
        self.dispatch();
        Ok(n)
    }

    pub(super) fn announce_lapsed(&self, lapsed: Vec<Lapsed>) {
        for (task, device, epoch) in lapsed {
            self.emit(
                "TASK_LEASE_LOST",
                json!({ "taskId": task, "deviceId": device, "epoch": epoch, "reason": "expired" }),
            );
        }
    }

    /// Under the lock: lapse expired leases (`leased -> queued`) and drop finished tasks past
    /// their retention.
    pub(super) fn sweep_locked(&self, g: &mut Inner, now: u64) -> Vec<Lapsed> {
        let mut lapsed = Vec::new();
        for t in g.state.tasks.values_mut() {
            if t.status == TaskStatus::Leased && t.expires <= now {
                lapsed.push((t.id.clone(), t.owner.clone().unwrap_or_default(), t.epoch));
                t.status = TaskStatus::Queued;
                t.owner = None;
                t.acked = false;
            }
        }
        let before = g.state.tasks.len();
        g.state.tasks.retain(|_, t| {
            matches!(t.status, TaskStatus::Queued | TaskStatus::Leased)
                || t.expires + FINISHED_RETENTION_SECS > now
        });
        if !lapsed.is_empty() || g.state.tasks.len() != before {
            g.dirty = true;
        }
        lapsed
    }

    /// Lease queued tasks to ready links and send the `task` frames. Pinned tasks go to their
    /// device when it is linked; `any` tasks go to the linked device holding the fewest leases.
    pub(super) fn dispatch(&self) {
        let sessions = self.ready_links();
        let now = self.clock.now_secs();
        let mut g = self.lock();
        let lapsed = self.sweep_locked(&mut g, now);
        let mut loads: BTreeMap<String, usize> = BTreeMap::new();
        for t in g.state.tasks.values() {
            if t.status == TaskStatus::Leased {
                if let Some(o) = &t.owner {
                    *loads.entry(o.clone()).or_default() += 1;
                }
            }
        }
        let mut queued: Vec<(u64, String)> = g
            .state
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Queued)
            .map(|t| (t.seq, t.id.clone()))
            .collect();
        queued.sort();
        let mut sends: Vec<(Arc<LinkSession>, Value, String, u64)> = Vec::new();
        if !sessions.is_empty() {
            for (_, id) in queued {
                let Some(t) = g.state.tasks.get_mut(&id) else {
                    continue;
                };
                let chosen = match &t.target {
                    Some(d) => sessions.iter().find(|s| s.device() == d.as_str()),
                    None => sessions.iter().min_by_key(|s| {
                        (
                            loads.get(s.device()).copied().unwrap_or(0),
                            s.device().to_string(),
                        )
                    }),
                };
                let Some(s) = chosen else {
                    continue;
                };
                t.epoch += 1;
                t.status = TaskStatus::Leased;
                t.owner = Some(s.device().to_string());
                t.expires = now + t.lease_ttl;
                t.acked = false;
                *loads.entry(s.device().to_string()).or_default() += 1;
                sends.push((s.clone(), task_frame(t), t.id.clone(), t.epoch));
            }
        }
        if !sends.is_empty() {
            g.dirty = true;
        }
        if let Err(e) = self.flush(&mut g) {
            // Nothing was sent: the leases stay in memory and lapse, so the tasks run later.
            drop(g);
            self.emit(
                "STORAGE_ERROR",
                json!({ "kind": format!("{:?}", e.kind()) }),
            );
            self.announce_lapsed(lapsed);
            return;
        }
        drop(g);
        self.announce_lapsed(lapsed);
        for (s, frame, id, epoch) in sends {
            self.emit(
                "TASK_LEASED",
                json!({ "taskId": id, "deviceId": s.device(), "epoch": epoch }),
            );
            s.send(frame);
        }
    }

    /// On `hello`: tasks this device holds with `seq <= resume_seq` are confirmed held (the device
    /// durably accepted them); the others are sent again with the same epoch.
    pub(super) fn resume_tasks(&self, s: &Arc<LinkSession>, resume_seq: u64) {
        let now = self.clock.now_secs();
        let mut frames = Vec::new();
        let lapsed;
        {
            let mut g = self.lock();
            lapsed = self.sweep_locked(&mut g, now);
            for t in g.state.tasks.values_mut() {
                if t.status == TaskStatus::Leased && t.owner.as_deref() == Some(s.device()) {
                    t.expires = now + t.lease_ttl;
                    if t.seq <= resume_seq {
                        t.acked = true;
                    } else {
                        frames.push(task_frame(t));
                    }
                }
            }
            let _ = self.flush(&mut g);
        }
        self.announce_lapsed(lapsed);
        frames.sort_by_key(|f| f["seq"].as_u64().unwrap_or(0));
        for f in frames {
            s.send(f);
        }
    }

    /// A frame arrived from `device`: renew its leases. `false` when the device was revoked.
    pub(super) fn touch_leases(&self, device: &str) -> bool {
        let now = self.clock.now_secs();
        let mut g = self.lock();
        if g.state.devices.get(device).is_none_or(|d| d.revoked) {
            return false;
        }
        for t in g.state.tasks.values_mut() {
            if t.status == TaskStatus::Leased && t.owner.as_deref() == Some(device) {
                t.expires = now + t.lease_ttl;
            }
        }
        true
    }

    /// The device confirms it durably holds `(id, epoch)`.
    pub(super) fn ack_task(
        &self,
        s: &Arc<LinkSession>,
        id: &str,
        epoch: u64,
    ) -> Result<(), &'static str> {
        if !self.link_is_current(s) {
            return Err("superseded");
        }
        let mut g = self.lock();
        match g.state.tasks.get_mut(id) {
            Some(t)
                if t.status == TaskStatus::Leased
                    && t.owner.as_deref() == Some(s.device())
                    && t.epoch == epoch =>
            {
                t.acked = true;
                Ok(())
            }
            Some(_) => Err("lease_lost"),
            None => Err("unknown_task"),
        }
    }

    /// Accept or refuse a result. Returns the `settled` state and, for a refusal, its code.
    pub(super) fn complete_task(
        &self,
        s: &Arc<LinkSession>,
        id: &str,
        epoch: u64,
        ok: bool,
        outcome: Value,
    ) -> (&'static str, Option<&'static str>) {
        let now = self.clock.now_secs();
        // Lock order is links, then state; `link_is_current` takes and releases `links` alone,
        // and a supersede between the check and the write is harmless because the lease check
        // below binds the result to the device and epoch anyway.
        if !self.link_is_current(s) {
            self.emit(
                "TASK_LEASE_LOST",
                json!({ "taskId": id, "deviceId": s.device(), "epoch": epoch,
                        "reason": "superseded_connection" }),
            );
            return ("refused", Some("superseded"));
        }
        let mut g = self.lock();
        let lapsed = self.sweep_locked(&mut g, now);
        let mut prev: Option<Task> = None;
        let verdict: Result<&'static str, &'static str> = match g.state.tasks.get_mut(id) {
            None => Err("unknown_task"),
            Some(t) => {
                let mine = t.owner.as_deref() == Some(s.device()) && t.epoch == epoch;
                match t.status {
                    TaskStatus::Leased if mine => {
                        prev = Some(t.clone());
                        t.status = if ok {
                            TaskStatus::Done
                        } else {
                            TaskStatus::Failed
                        };
                        t.outcome = Some(outcome);
                        t.expires = now;
                        t.acked = true;
                        Ok("accepted")
                    }
                    TaskStatus::Done | TaskStatus::Failed if mine => Ok("duplicate"),
                    _ => Err("lease_lost"),
                }
            }
        };
        let saved = match verdict {
            Ok("accepted") => self.save(&mut g),
            _ => self.flush(&mut g),
        };
        if saved.is_err() {
            // Not durable: put the lease back so the state in memory matches the disk.
            if let Some(p) = prev {
                g.state.tasks.insert(p.id.clone(), p);
            }
        }
        drop(g);
        self.announce_lapsed(lapsed);
        if let Err(e) = saved {
            self.emit(
                "STORAGE_ERROR",
                json!({ "kind": format!("{:?}", e.kind()) }),
            );
            // The device keeps the result and sends it again.
            return ("refused", Some("storage_error"));
        }
        match verdict {
            Ok("accepted") => {
                self.emit(
                    if ok { "TASK_DONE" } else { "TASK_FAILED" },
                    json!({ "taskId": id, "deviceId": s.device(), "epoch": epoch }),
                );
                ("accepted", None)
            }
            Ok(other) => (other, None),
            Err(code) => {
                if code == "lease_lost" {
                    self.emit(
                        "TASK_LEASE_LOST",
                        json!({ "taskId": id, "deviceId": s.device(), "epoch": epoch,
                                "reason": "stale_result" }),
                    );
                }
                ("refused", Some(code))
            }
        }
    }
}
