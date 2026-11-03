//! The driver: one `rightkit-automation` engine per session, with a policy that allows exactly the
//! consented app.
//!
//! Policy (design, section 2 step 4): `Policy::new([])` denies everything; `allow` adds the single
//! consented catalog app; `deny` blocks the host product and Pulse before the allow list is read; `protect_pid`
//! refuses this process. The engine's own gates then apply to every act: kill switch, allowlist, lease
//! fence, snapshot freshness, secure and credential fields, risk classes.
//!
//! The agent only ever sees `{ref, role, label, ops}`: never a value, never a screenshot, never the
//! clipboard. The tier ceiling is `Targeted` (per-pid events that never move the pointer); `Foreground`
//! and the in-app tier are not reachable from here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_agent_tools::backends::demo::{ActStep, DemoErrorCode, ElementOp, SnapshotElement};
use rightkit_automation::{
    ActOptions, Action, AppInfo, Automation, Button, Error as AutoError, EvidenceSink, Fence,
    Guard, Indicator, Lease, LeaseRequest, Limits, NativePlatform, Node, Policy, ResultKind,
    RiskClass, Status, Target, Tier,
};

use super::catalog::{self, CatalogEntry};

/// An error ready to go on the wire: a contract code, or a transport-style code the contract lets
/// pass through unchanged.
#[derive(Debug, Clone)]
pub struct DemoError {
    pub code: String,
    pub message: String,
}

impl DemoError {
    pub fn of(code: DemoErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.as_str().to_string(),
            message: message.into(),
        }
    }
    pub fn raw(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl From<DemoError> for rightkit_daemon::ServiceError {
    fn from(e: DemoError) -> Self {
        rightkit_daemon::ServiceError::new(e.code, e.message)
    }
}

/// Maps an engine refusal to the contract code that names the gate.
pub fn map_auto(e: AutoError) -> DemoError {
    use AutoError as E;
    match e {
        E::PermissionDenied(what) => DemoError::of(
            DemoErrorCode::PermissionMissing,
            format!("{what} permission is not granted"),
        ),
        E::Blocked(m) => {
            if rightkit_automation::killed() {
                DemoError::of(DemoErrorCode::SessionKilled, "the session was killed")
            } else {
                DemoError::of(DemoErrorCode::TargetBlocked, m)
            }
        }
        E::NeedsConfirmation(c) => DemoError::of(
            DemoErrorCode::TargetBlocked,
            format!(
                "a {} action needs a person's confirmation; the demo never confirms for itself",
                c.label()
            ),
        ),
        E::Denied(m) | E::NoTier(m) => DemoError::of(DemoErrorCode::TargetBlocked, m),
        E::FocusMoved { .. } => DemoError::of(
            DemoErrorCode::TargetBlocked,
            "the action would have moved focus off the target",
        ),
        E::UnknownSnapshot(_) => DemoError::of(
            DemoErrorCode::InvalidArguments,
            "the snapshot expired; call demo.snapshot again",
        ),
        E::Invalid(m) => DemoError::of(DemoErrorCode::InvalidArguments, m),
        E::NotFound(m) => {
            DemoError::of(DemoErrorCode::NotArmed, format!("the target is gone: {m}"))
        }
        E::LeaseHeld { .. } | E::StaleLease { .. } | E::OutOfOrder { .. } | E::Replayed { .. } => {
            DemoError::of(
                DemoErrorCode::NotArmed,
                format!("the control lease refused the input: {e}"),
            )
        }
        other => DemoError::raw("act_failed", other.to_string()),
    }
}

struct RefInfo {
    element: String,
    caps: Vec<String>,
    ops: Vec<ElementOp>,
}

#[derive(Default)]
struct State {
    target: Option<Target>,
    pid: Option<i32>,
    lease: Option<Lease>,
    snapshot: Option<String>,
    refs: HashMap<String, RefInfo>,
    /// Texts the agent typed this session. They become the canaries `bundle_verify` must not find.
    typed: Vec<String>,
}

pub struct Driver {
    auto: Arc<Automation<NativePlatform>>,
    entry: &'static CatalogEntry,
    state: Mutex<State>,
}

/// Shortest typed text kept as a canary: shorter values would match ordinary bundle text.
const MIN_CANARY: usize = 3;
const MAX_TEXT: usize = 4096;

impl Driver {
    pub fn new(
        entry: &'static CatalogEntry,
        evidence: Arc<dyn EvidenceSink>,
        indicator: Arc<dyn Indicator>,
    ) -> Self {
        let policy = Policy::new(std::iter::empty::<String>());
        policy.allow(entry.platform_id());
        for id in catalog::denied_ids() {
            policy.deny(id);
        }
        policy.protect_pid(std::process::id() as i32);
        let auto = Arc::new(rightkit_automation::native(policy));
        auto.add_evidence(evidence);
        auto.set_indicator(indicator);
        Self {
            auto,
            entry,
            state: Mutex::new(State::default()),
        }
    }

    pub fn entry(&self) -> &'static CatalogEntry {
        self.entry
    }

    pub fn os_status(&self) -> Status {
        self.auto.status()
    }

    /// The running app that is the consented catalog entry and owns `pid`.
    pub fn find_app(&self, pid: i32) -> Option<AppInfo> {
        self.auto
            .apps()
            .into_iter()
            .find(|a| a.pid == pid && a.app_id.eq_ignore_ascii_case(self.entry.platform_id()))
    }

    /// `Policy::check` on the resolved app, before anything else touches it (recording has no policy
    /// hook of its own).
    pub fn check(&self, app: &AppInfo) -> Result<(), DemoError> {
        self.auto.policy().check(app).map_err(map_auto)
    }

    /// Opens the visible session and, for an agent, takes the control lease.
    pub fn attach(
        &self,
        app: &AppInfo,
        window_id: u64,
        label: &str,
        holder: &str,
        lease: bool,
    ) -> Result<(), DemoError> {
        self.auto.begin_session(app.pid, label).map_err(map_auto)?;
        let lease = if lease {
            match self.auto.acquire_lease(
                app.pid,
                LeaseRequest::new(holder).ttl(Duration::from_secs(60)),
            ) {
                Ok(l) => Some(l),
                Err(e) => {
                    self.auto.end_session(app.pid);
                    return Err(map_auto(e));
                }
            }
        } else {
            None
        };
        let mut s = self.state.lock().unwrap();
        s.target = Some(Target::window(app.pid, window_id));
        s.pid = Some(app.pid);
        s.lease = lease;
        Ok(())
    }

    /// Keeps the lease alive; called from the host's tick.
    pub fn renew(&self) {
        let lease = self.state.lock().unwrap().lease.clone();
        if let Some(l) = lease {
            self.auto.renew_lease(&l);
        }
    }

    /// Checks that the target still runs; drops the session and lease of one that vanished.
    /// Returns the pids that exited.
    pub fn poll(&self) -> Vec<i32> {
        self.auto.poll_targets()
    }

    /// Releases the lease and ends the session. Idempotent.
    pub fn detach(&self) {
        let (lease, pid) = {
            let mut s = self.state.lock().unwrap();
            s.refs.clear();
            s.snapshot = None;
            (s.lease.take(), s.pid.take())
        };
        if let Some(l) = lease {
            self.auto.release_lease(&l);
        }
        if let Some(pid) = pid {
            self.auto.end_session(pid);
        }
    }

    /// Plain-text values the agent typed, for `bundle_verify`.
    pub fn canaries(&self) -> Vec<String> {
        self.state.lock().unwrap().typed.clone()
    }

    /// The elements the agent may act on. Refs are valid until the next snapshot.
    pub fn snapshot(&self) -> Result<Vec<SnapshotElement>, DemoError> {
        let target =
            self.state.lock().unwrap().target.clone().ok_or_else(|| {
                DemoError::of(DemoErrorCode::NotArmed, "the session has not started")
            })?;
        let snap = self
            .auto
            .snapshot(target, Limits::default())
            .map_err(map_auto)?;
        let mut elements = Vec::new();
        let mut refs = HashMap::new();
        collect(&snap.root, &mut elements, &mut refs);
        let mut s = self.state.lock().unwrap();
        s.snapshot = Some(snap.id);
        s.refs = refs;
        Ok(elements)
    }

    /// One act, through every engine gate. Returns the receipt id.
    pub fn act(&self, session: &str, step: &ActStep) -> Result<String, DemoError> {
        let invalid = |m: &str| DemoError::of(DemoErrorCode::InvalidArguments, m);
        let (pid, snapshot, info) = {
            let s = self.state.lock().unwrap();
            let pid = s.pid.ok_or_else(|| {
                DemoError::of(DemoErrorCode::NotArmed, "the session has not started")
            })?;
            let snapshot = s
                .snapshot
                .clone()
                .ok_or_else(|| invalid("no snapshot yet; call demo.snapshot first"))?;
            let info = s
                .refs
                .get(&step.element_ref)
                .ok_or_else(|| invalid("unknown ref; call demo.snapshot for current refs"))?;
            (
                pid,
                snapshot,
                (info.element.clone(), info.caps.clone(), info.ops.clone()),
            )
        };
        let (element, caps, ops) = info;
        if !ops.contains(&step.op) {
            return Err(invalid("that element does not accept this operation"));
        }
        let action = match step.op {
            ElementOp::Click => {
                if caps.iter().any(|c| c == "press") {
                    Action::Press
                } else {
                    Action::Click {
                        point: None,
                        button: Button::Left,
                        count: 1,
                    }
                }
            }
            ElementOp::Type => {
                let text = step
                    .text
                    .clone()
                    .ok_or_else(|| invalid("`text` is required for type"))?;
                if text.is_empty() || text.len() > MAX_TEXT {
                    return Err(invalid("`text` must be 1 to 4096 bytes"));
                }
                if text.len() >= MIN_CANARY {
                    self.state.lock().unwrap().typed.push(text.clone());
                }
                Action::TypeText(text)
            }
            ElementOp::Press => {
                let key = step
                    .key
                    .clone()
                    .ok_or_else(|| invalid("`key` is required for press"))?;
                Action::Keys(normalise_key(&key).map_err(|m| invalid(&m))?)
            }
        };
        let lease = self
            .auto
            .lease_status(pid)
            .ok_or_else(|| DemoError::of(DemoErrorCode::NotArmed, "the control lease is gone"))?;
        let opts = ActOptions {
            min_tier: Tier::Semantic,
            max_tier: Tier::Targeted,
            guard: Guard::Structure,
            // The demo never confirms a delete, send, purchase or close for the agent.
            confirm: Vec::<RiskClass>::new(),
            strict_focus: true,
            log_values: false,
            settle_ms: 150,
            fence: Some(Fence {
                epoch: lease.epoch,
                seq: lease.acked_seq + 1,
            }),
        };
        let outcome = self
            .auto
            .act(&snapshot, Some(&element), action, &opts)
            .map_err(map_auto)?;
        if outcome.done() {
            return Ok(receipt_id(session, outcome.receipt.seq));
        }
        if outcome.fresh.is_some() {
            return Err(DemoError::of(
                DemoErrorCode::InvalidArguments,
                "the target changed; call demo.snapshot again",
            ));
        }
        let why = outcome.receipt.error.clone().unwrap_or_default();
        match outcome.receipt.result {
            ResultKind::Blocked => {
                let gate = outcome
                    .receipt
                    .denied_by
                    .map(|d| d.label())
                    .unwrap_or("policy");
                Err(DemoError::of(
                    DemoErrorCode::TargetBlocked,
                    format!(
                        "refused by the {gate} gate{}",
                        if why.is_empty() {
                            String::new()
                        } else {
                            format!(": {why}")
                        }
                    ),
                ))
            }
            _ => Err(DemoError::raw("act_failed", why)),
        }
    }
}

pub fn receipt_id(session: &str, seq: u64) -> String {
    format!("{session}-r{seq}")
}

/// Chords the agent may press: letters, digits, `+`, space and the named keys. Quit and close chords
/// are refused here (the engine's risk classes only see labels, not chords).
fn normalise_key(key: &str) -> Result<String, String> {
    let k = key.trim().to_ascii_lowercase();
    if k.is_empty() || k.len() > 32 {
        return Err("`key` must be 1 to 32 characters".into());
    }
    if !k
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == ' ')
    {
        return Err("`key` may contain letters, digits, `+` and spaces only".into());
    }
    const REFUSED: &[&str] = &[
        "cmd+q",
        "cmd+w",
        "cmd+option+esc",
        "alt+f4",
        "ctrl+w",
        "ctrl+alt+delete",
    ];
    if k.split(' ').any(|chord| REFUSED.contains(&chord)) {
        return Err("quit and close chords are not available to the demo".into());
    }
    Ok(k)
}

fn collect(node: &Node, out: &mut Vec<SnapshotElement>, refs: &mut HashMap<String, RefInfo>) {
    let has = |c: &str| node.caps.iter().any(|x| x == c);
    let enabled = node.enabled != Some(false);
    let mut ops = Vec::new();
    if enabled && (has("press") || has("toggle") || has("select") || has("expand")) {
        ops.push(ElementOp::Click);
    }
    let typable = enabled && (has("set_value") || has("insert_text"));
    if typable {
        ops.push(ElementOp::Type);
    }
    if typable || (enabled && has("focus")) {
        ops.push(ElementOp::Press);
    }
    if !ops.is_empty() {
        let element_ref = format!("e{}", out.len() + 1);
        let label = node
            .name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(node.description.as_deref().filter(|s| !s.is_empty()))
            .or(node.identifier.as_deref().filter(|s| !s.is_empty()))
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect::<String>();
        refs.insert(
            element_ref.clone(),
            RefInfo {
                element: node.id.clone(),
                caps: node.caps.clone(),
                ops: ops.clone(),
            },
        );
        out.push(SnapshotElement {
            element_ref,
            role: node.role.clone(),
            label,
            ops,
        });
    }
    for child in &node.children {
        collect(child, out, refs);
    }
}
