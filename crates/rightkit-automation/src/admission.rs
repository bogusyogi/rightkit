//! Admission pre-action hook (CodeRight EFF-001: validate -> approve -> execute
//! -> settle). The caller supplies one [`Admission`]; every effect passes it as a
//! typed [`EffectRequest`] *after* the crate's own validation (allowlist, lease,
//! freshness, secure-field and credential gates) and *before* any side effect.
//! A denial executes nothing and produces a `Blocked` receipt; `settle` is told
//! the outcome afterwards. With no hook installed every validated effect is admitted.

use crate::model::*;
use crate::policy::ResultKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    /// Reading an accessibility tree.
    Observe,
    /// An input action through the ladder.
    Act,
    /// Text delivery to a pinned editor.
    Deliver,
    Launch,
    Terminate,
    /// A window screenshot.
    Capture,
}

impl EffectKind {
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Observe => "observe",
            EffectKind::Act => "act",
            EffectKind::Deliver => "deliver",
            EffectKind::Launch => "launch",
            EffectKind::Terminate => "terminate",
            EffectKind::Capture => "capture",
        }
    }
}

/// Content-free description of one intended effect (no payload text, only its length).
#[derive(Debug, Clone)]
pub struct EffectRequest {
    pub kind: EffectKind,
    pub session: String,
    pub pid: i32,
    pub app_id: String,
    /// `press`, `type_text`, `keys`, `launch`, ... (see `Action::name`).
    pub action: String,
    /// Accessible name or role of the target element, when there is one.
    pub target: Option<String>,
    /// Highest tier this effect may use.
    pub max_tier: Tier,
    pub risk: Option<RiskClass>,
    pub payload_chars: Option<usize>,
    pub lease_epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admit {
    Allow,
    /// Allow, but never above this tier (e.g. forbid foreground input).
    AllowUpTo(Tier),
    Deny(String),
}

#[derive(Debug, Clone)]
pub struct Settled {
    pub result: ResultKind,
    pub tier_used: Option<Tier>,
    pub error: Option<String>,
}

pub trait Admission: Send + Sync {
    fn approve(&self, req: &EffectRequest) -> Admit;
    /// Called once the effect finished (or failed) after approval.
    fn settle(&self, _req: &EffectRequest, _outcome: &Settled) {}
}

/// Admits everything; the default.
pub struct AllowAll;
impl Admission for AllowAll {
    fn approve(&self, _req: &EffectRequest) -> Admit {
        Admit::Allow
    }
}
