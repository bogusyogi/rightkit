//! EFF-001 effect admission: every effectful control request traverses one
//! authority, validate → (policy) → approval → execute → settle, and a denial
//! is final before any side effect.
//!
//! Contract (CodeRight `docs/canon/daemon-master-canon-atoms.md`, EFF-001):
//! effects "converge on one semantic validate → transform → normalize → policy
//! → approval → resource → execute → verify → settle authority"; acceptance:
//! "denied path never executes". Stage names and order mirror CodeRight's
//! `EffectAdmissionStage`. A surface may skip a stage that does not apply but
//! may never reorder them or reach `Execute` after a denial. This crate
//! traverses `Validate`, `Policy` (only the process-wide kill switch,
//! `policy::kill`, checked before and after approval), `Approval`, `Execute`
//! and `Settle`; transform/normalize/resource/verify belong to the caller (its
//! hook can implement policy; see [`super::policy_hook::PolicyAdmission`] for
//! the allowlist).
//!
//! The executor is a closure handed to [`EffectGate::admit`]; it is only ever
//! invoked after the host's [`AdmissionHook`] returned
//! [`AdmissionDecision::Allow`] and the cancellation token was still clear.
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::control::events::{emit, ControlEvent, EventSink};

/// EFF-001 admission stages in canonical order (identical to CodeRight's
/// `EffectAdmissionStage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectAdmissionStage {
    Validate,
    Transform,
    Normalize,
    Policy,
    Approval,
    Resource,
    Execute,
    Verify,
    Settle,
}

impl EffectAdmissionStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Validate => "validate",
            Self::Transform => "transform",
            Self::Normalize => "normalize",
            Self::Policy => "policy",
            Self::Approval => "approval",
            Self::Resource => "resource",
            Self::Execute => "execute",
            Self::Verify => "verify",
            Self::Settle => "settle",
        }
    }
}

/// What kind of effect a request produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    /// Load a URL / move to another document.
    Navigate,
    /// Pointer, wheel, key or text input delivered to a target.
    Input,
    /// Semantic action on an element (AX press, select, focus).
    Action,
    /// Run script inside the target (e.g. webview `eval`).
    Script,
    /// Capture pixels/state to a file or buffer.
    Capture,
    /// Launch or terminate a process/window.
    Process,
    /// An app-registered typed command.
    Command,
}

impl EffectKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::Input => "input",
            Self::Action => "action",
            Self::Script => "script",
            Self::Capture => "capture",
            Self::Process => "process",
            Self::Command => "command",
        }
    }
}

/// A typed effect request presented to the admission hook.
///
/// `params` is the opaque request payload (usually JSON) so the hook can bind
/// its approval to the exact payload; it is never copied into events or
/// settlement records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRequest {
    /// Assigned by [`EffectGate::admit`]; any caller value is overwritten.
    pub id: u64,
    pub kind: EffectKind,
    /// Method / verb name, e.g. `click`, `type`, `eval`.
    pub method: String,
    /// Target identity, e.g. window label, `pid:1234`, element id.
    pub target: String,
    /// Opaque payload for payload-bound approval. Never emitted.
    pub params: String,
}

impl EffectRequest {
    pub fn new(kind: EffectKind, method: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            id: 0,
            kind,
            method: method.into(),
            target: target.into(),
            params: String::new(),
        }
    }
    pub fn with_params(mut self, params: impl Into<String>) -> Self {
        self.params = params.into();
        self
    }
}

/// Host decision at the approval stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    Allow,
    Deny { reason: String },
}

impl AdmissionDecision {
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }
}

/// Host-supplied admission authority. Closures
/// `Fn(&EffectRequest) -> AdmissionDecision` implement it.
///
/// A panic in either method fails closed (the request is denied).
pub trait AdmissionHook: Send + Sync {
    /// Structural validation before approval. Default accepts.
    fn validate(&self, _request: &EffectRequest) -> Result<(), String> {
        Ok(())
    }
    /// Approve or deny. Called only after validation passed.
    fn approve(&self, request: &EffectRequest) -> AdmissionDecision;
}

impl<F> AdmissionHook for F
where
    F: Fn(&EffectRequest) -> AdmissionDecision + Send + Sync,
{
    fn approve(&self, request: &EffectRequest) -> AdmissionDecision {
        self(request)
    }
}

/// Explicit allow-all hook. This is what a host gets when it configures no
/// hook, preserving pre-0.1.1 behaviour while still settling every effect.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

impl AdmissionHook for AllowAll {
    fn approve(&self, _: &EffectRequest) -> AdmissionDecision {
        AdmissionDecision::Allow
    }
}

/// Cooperative cancellation shared between caller and executor.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Why an executor did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    Failed(String),
    /// The executor observed cancellation and stopped.
    Cancelled,
}

impl From<String> for ExecError {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}
impl From<&str> for ExecError {
    fn from(s: &str) -> Self {
        Self::Failed(s.into())
    }
}

/// Terminal state recorded at the settle stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettleOutcome {
    Ok,
    Denied,
    Failed,
    Cancelled,
}

impl SettleOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Content-free settlement record (no params, no executor output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    pub id: u64,
    pub kind: EffectKind,
    pub method: String,
    pub target: String,
    pub outcome: SettleOutcome,
    /// Stages traversed, canonical order, always ending in `Settle`.
    pub stages: Vec<EffectAdmissionStage>,
    /// True iff the executor was invoked (false for every denial).
    pub executed: bool,
    /// True iff the executor panicked (outcome is then `Failed`).
    pub crashed: bool,
    /// Denial / failure reason. Hook- or executor-authored text.
    pub reason: Option<String>,
}

/// Result of [`EffectGate::admit`]: the settlement plus the executor's value
/// when the outcome is `Ok`.
#[derive(Debug)]
pub struct Effect<T> {
    pub settlement: Settlement,
    pub value: Option<T>,
}

impl<T> Effect<T> {
    pub fn is_ok(&self) -> bool {
        self.settlement.outcome == SettleOutcome::Ok
    }
    /// `Ok(value)` or `Err(reason)` with an outcome prefix (`denied: ...`).
    pub fn into_result(self) -> Result<T, String> {
        match self.value {
            Some(v) if self.settlement.outcome == SettleOutcome::Ok => Ok(v),
            _ => Err(format!(
                "{}: {}",
                self.settlement.outcome.label(),
                self.settlement.reason.unwrap_or_default()
            )),
        }
    }
}

/// Denial reason when the process-wide kill switch (`policy::kill`) is engaged.
pub const KILL_SWITCH_REASON: &str = "kill switch engaged";

/// Default number of settlements retained by [`EffectGate::settlements`].
pub const DEFAULT_JOURNAL_CAPACITY: usize = 1024;

/// The single admission authority for one control surface. Cheap to share
/// behind an `Arc`; `admit` is safe to call concurrently.
pub struct EffectGate {
    hook: Arc<dyn AdmissionHook>,
    events: Option<EventSink>,
    next_id: AtomicU64,
    journal: Mutex<VecDeque<Settlement>>,
    capacity: usize,
}

impl std::fmt::Debug for EffectGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectGate")
            .field("next_id", &self.next_id.load(Ordering::SeqCst))
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl Default for EffectGate {
    fn default() -> Self {
        Self::allow_all()
    }
}

impl EffectGate {
    pub fn new(hook: impl AdmissionHook + 'static) -> Self {
        Self::from_arc(Arc::new(hook))
    }
    pub fn from_arc(hook: Arc<dyn AdmissionHook>) -> Self {
        Self {
            hook,
            events: None,
            next_id: AtomicU64::new(1),
            journal: Mutex::new(VecDeque::new()),
            capacity: DEFAULT_JOURNAL_CAPACITY,
        }
    }
    /// Gate with [`AllowAll`]: still validates, settles and emits events.
    pub fn allow_all() -> Self {
        Self::new(AllowAll)
    }
    pub fn with_events(mut self, sink: EventSink) -> Self {
        self.events = Some(sink);
        self
    }
    /// Bound on retained settlements (oldest dropped first). `0` keeps none.
    pub fn with_journal_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Snapshot of retained settlements, oldest first.
    pub fn settlements(&self) -> Vec<Settlement> {
        lock(&self.journal).iter().cloned().collect()
    }

    /// Run `request` through validate → approval → execute → settle.
    ///
    /// `execute` is invoked at most once, and only when validation and the
    /// hook both admit the request and `cancel` is not yet set. Every path
    /// settles exactly once (recorded and returned) and emits lifecycle
    /// events. Executor panics are contained and settled as `Failed`.
    pub fn admit<T>(
        &self,
        mut request: EffectRequest,
        cancel: &CancelToken,
        execute: impl FnOnce(&CancelToken) -> Result<T, ExecError>,
    ) -> Effect<T> {
        request.id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut stages = vec![EffectAdmissionStage::Validate];

        // Validate (crate structural checks, then the host's).
        let validated = if request.method.trim().is_empty() {
            Err("effect method is empty".to_string())
        } else {
            guarded(|| self.hook.validate(&request))
                .unwrap_or_else(|| Err("admission hook panicked during validate".into()))
        };
        if let Err(reason) = validated {
            return self.deny(request, stages, EffectAdmissionStage::Validate, reason);
        }

        // Kill switch (`policy::kill`, process-wide): refused before the host hook runs.
        if crate::policy::killed() {
            stages.push(EffectAdmissionStage::Policy);
            return self.deny(
                request,
                stages,
                EffectAdmissionStage::Policy,
                KILL_SWITCH_REASON.into(),
            );
        }

        // Approval.
        stages.push(EffectAdmissionStage::Approval);
        let decision = guarded(|| self.hook.approve(&request))
            .unwrap_or_else(|| AdmissionDecision::deny("admission hook panicked during approval"));
        if let AdmissionDecision::Deny { reason } = decision {
            return self.deny(request, stages, EffectAdmissionStage::Approval, reason);
        }

        // Kill switch (`policy::kill`, process-wide): checked again after approval, which a
        // host hook may have held for a while, and before any side effect.
        if crate::policy::killed() {
            return self.deny(
                request,
                stages,
                EffectAdmissionStage::Policy,
                KILL_SWITCH_REASON.into(),
            );
        }

        // Cancelled after approval but before any side effect.
        if cancel.is_cancelled() {
            return self.settle(
                request,
                stages,
                SettleOutcome::Cancelled,
                false,
                false,
                Some("cancelled before execute".into()),
                None,
            );
        }

        // Execute.
        stages.push(EffectAdmissionStage::Execute);
        emit(
            &self.events,
            ControlEvent::EffectStarted {
                id: request.id,
                kind: request.kind,
                method: request.method.clone(),
            },
        );
        match catch_unwind(AssertUnwindSafe(|| execute(cancel))) {
            Ok(Ok(v)) => self.settle(
                request,
                stages,
                SettleOutcome::Ok,
                true,
                false,
                None,
                Some(v),
            ),
            Ok(Err(ExecError::Cancelled)) => self.settle(
                request,
                stages,
                SettleOutcome::Cancelled,
                true,
                false,
                Some("cancelled during execute".into()),
                None,
            ),
            Ok(Err(ExecError::Failed(reason))) => self.settle(
                request,
                stages,
                SettleOutcome::Failed,
                true,
                false,
                Some(reason),
                None,
            ),
            Err(panic) => {
                let reason = panic_text(&panic);
                self.settle(
                    request,
                    stages,
                    SettleOutcome::Failed,
                    true,
                    true,
                    Some(reason),
                    None,
                )
            }
        }
    }

    fn deny<T>(
        &self,
        request: EffectRequest,
        stages: Vec<EffectAdmissionStage>,
        stage: EffectAdmissionStage,
        reason: String,
    ) -> Effect<T> {
        self.settle_with(
            request,
            stages,
            SettleOutcome::Denied,
            false,
            false,
            Some(reason),
            None,
            Some(stage),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn settle<T>(
        &self,
        request: EffectRequest,
        stages: Vec<EffectAdmissionStage>,
        outcome: SettleOutcome,
        executed: bool,
        crashed: bool,
        reason: Option<String>,
        value: Option<T>,
    ) -> Effect<T> {
        self.settle_with(
            request, stages, outcome, executed, crashed, reason, value, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn settle_with<T>(
        &self,
        request: EffectRequest,
        mut stages: Vec<EffectAdmissionStage>,
        outcome: SettleOutcome,
        executed: bool,
        crashed: bool,
        reason: Option<String>,
        value: Option<T>,
        denied_at: Option<EffectAdmissionStage>,
    ) -> Effect<T> {
        stages.push(EffectAdmissionStage::Settle);
        let settlement = Settlement {
            id: request.id,
            kind: request.kind,
            method: request.method,
            target: request.target,
            outcome,
            stages,
            executed,
            crashed,
            reason,
        };
        if self.capacity > 0 {
            let mut j = lock(&self.journal);
            while j.len() >= self.capacity {
                j.pop_front();
            }
            j.push_back(settlement.clone());
        }
        let (id, kind, method) = (settlement.id, settlement.kind, settlement.method.clone());
        let event = match (denied_at, crashed) {
            (Some(stage), _) => ControlEvent::EffectDenied {
                id,
                kind,
                method,
                stage,
                reason: settlement.reason.clone().unwrap_or_default(),
            },
            (None, true) => ControlEvent::EffectCrashed { id, kind, method },
            (None, false) => ControlEvent::EffectStopped {
                id,
                kind,
                method,
                outcome,
                executed,
            },
        };
        emit(&self.events, event);
        Effect { settlement, value }
    }
}

fn guarded<R>(f: impl FnOnce() -> R) -> Option<R> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    let msg = p
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic".into());
    format!("executor panicked: {msg}")
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
