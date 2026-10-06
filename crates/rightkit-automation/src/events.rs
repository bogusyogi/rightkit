//! Structured lifecycle events for the caller's journal (start / stop / crash /
//! denied / lease) and content-free metrics. Nothing here carries typed text,
//! element values or screenshots, and nothing is ever uploaded: the host decides
//! where events go.

use crate::json::Obj;
use crate::model::now_ms;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Which gate refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeniedBy {
    /// Allowlist, deny list, kill switch, self-protection.
    Policy,
    /// The caller's [`crate::Admission`] hook.
    Admission,
    /// Stale / missing / out-of-order lease fence.
    Lease,
    /// Secure field, credential payload, unconfirmed risk.
    Safety,
}

impl DeniedBy {
    pub fn label(self) -> &'static str {
        match self {
            DeniedBy::Policy => "policy",
            DeniedBy::Admission => "admission",
            DeniedBy::Lease => "lease",
            DeniedBy::Safety => "safety",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleEvent {
    Started { session: String, pid: i32, app_id: String, label: String },
    Stopped { session: String, pid: i32 },
    /// The target process vanished while a session or lease was active.
    TargetExited { session: String, pid: i32, app_id: String },
    Denied { session: String, pid: i32, app_id: String, action: String, by: DeniedBy, reason: String },
    LeaseGranted { pid: i32, holder: String, epoch: u64, displaced: Option<String> },
    LeaseReleased { pid: i32, epoch: u64 },
    /// An input from a stale or out-of-order controller was refused.
    LeaseFenced { pid: i32, presented_epoch: u64, current_epoch: Option<u64> },
    KillSwitch,
}

impl LifecycleEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            LifecycleEvent::Started { .. } => "started",
            LifecycleEvent::Stopped { .. } => "stopped",
            LifecycleEvent::TargetExited { .. } => "target_exited",
            LifecycleEvent::Denied { .. } => "denied",
            LifecycleEvent::LeaseGranted { .. } => "lease_granted",
            LifecycleEvent::LeaseReleased { .. } => "lease_released",
            LifecycleEvent::LeaseFenced { .. } => "lease_fenced",
            LifecycleEvent::KillSwitch => "kill_switch",
        }
    }

    /// One JSON object, content-free.
    pub fn to_json(&self) -> String {
        let o = Obj::new().str("event", self.kind()).num("ts_ms", now_ms());
        match self {
            LifecycleEvent::Started { session, pid, app_id, label } => o.str("session", session).num("pid", *pid).str("app_id", app_id).str("label", label),
            LifecycleEvent::Stopped { session, pid } => o.str("session", session).num("pid", *pid),
            LifecycleEvent::TargetExited { session, pid, app_id } => o.str("session", session).num("pid", *pid).str("app_id", app_id),
            LifecycleEvent::Denied { session, pid, app_id, action, by, reason } => {
                o.str("session", session).num("pid", *pid).str("app_id", app_id).str("action", action).str("by", by.label()).str("reason", reason)
            }
            LifecycleEvent::LeaseGranted { pid, holder, epoch, displaced } => o.num("pid", *pid).str("holder", holder).num("epoch", *epoch).opt_str("displaced", displaced),
            LifecycleEvent::LeaseReleased { pid, epoch } => o.num("pid", *pid).num("epoch", *epoch),
            LifecycleEvent::LeaseFenced { pid, presented_epoch, current_epoch } => o
                .num("pid", *pid)
                .num("presented_epoch", *presented_epoch)
                .raw("current_epoch", current_epoch.map(|e| e.to_string()).unwrap_or_else(|| "null".into())),
            LifecycleEvent::KillSwitch => o,
        }
        .build()
    }
}

/// Receives lifecycle events (the caller's journal).
pub trait Journal: Send + Sync {
    fn event(&self, e: &LifecycleEvent);
}

#[derive(Default)]
pub struct MemoryJournal(pub Mutex<Vec<LifecycleEvent>>);
impl Journal for MemoryJournal {
    fn event(&self, e: &LifecycleEvent) {
        self.0.lock().unwrap().push(e.clone());
    }
}
impl MemoryJournal {
    pub fn all(&self) -> Vec<LifecycleEvent> {
        self.0.lock().unwrap().clone()
    }
}

/// Content-free counters. No text, no element names, no app payloads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metrics {
    pub sessions_started: u64,
    pub acts: u64,
    pub acts_done: u64,
    pub acts_failed: u64,
    pub denied: u64,
    pub replayed: u64,
    pub fenced: u64,
    /// Acts that completed per tier: semantic, in_app, targeted, foreground.
    pub by_tier: [u64; 4],
    pub deliveries: u64,
    pub screenshots: u64,
    pub screenshot_regions_masked: u64,
}

#[derive(Default)]
pub(crate) struct Counters {
    pub sessions_started: AtomicU64,
    pub acts: AtomicU64,
    pub acts_done: AtomicU64,
    pub acts_failed: AtomicU64,
    pub denied: AtomicU64,
    pub replayed: AtomicU64,
    pub fenced: AtomicU64,
    pub by_tier: [AtomicU64; 4],
    pub deliveries: AtomicU64,
    pub screenshots: AtomicU64,
    pub masked: AtomicU64,
}

impl Counters {
    pub fn snapshot(&self) -> Metrics {
        let g = |a: &AtomicU64| a.load(Ordering::SeqCst);
        Metrics {
            sessions_started: g(&self.sessions_started),
            acts: g(&self.acts),
            acts_done: g(&self.acts_done),
            acts_failed: g(&self.acts_failed),
            denied: g(&self.denied),
            replayed: g(&self.replayed),
            fenced: g(&self.fenced),
            by_tier: [g(&self.by_tier[0]), g(&self.by_tier[1]), g(&self.by_tier[2]), g(&self.by_tier[3])],
            deliveries: g(&self.deliveries),
            screenshots: g(&self.screenshots),
            screenshot_regions_masked: g(&self.masked),
        }
    }
}
