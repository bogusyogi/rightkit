//! Taste engine — turns a role's [`TasteScore`](crate::coding::taste::TasteScore) into
//! concrete, confidence-gated improvement proposals.
//!
//! Proposals are **inert until approved**: this engine only suggests. It reads
//! the evaluation signal and emits typed recommendations — switch a fragile
//! role's model, stabilize a high-variance prompt, restrict a decaying role from
//! long tasks, enable context reset for meltdown-prone roles, or switch to JIT
//! memory when context efficiency is poor.

use crate::coding::taste::TasteScore;

/// The kind of change a proposal recommends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImprovementKind {
    /// Replace the role's model (it underperforms overall).
    ModelSwitch,
    /// Stabilize the role's prompt (high run-to-run variance).
    PromptStabilization,
    /// Restrict the role from long / very-long tasks (steep decay).
    RoutingRestriction,
    /// Enable context reset for this role (meltdown-prone).
    ContextReset,
    /// Switch the role to just-in-time memory (poor context efficiency).
    JitMemory,
}

impl ImprovementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ImprovementKind::ModelSwitch => "ModelSwitch",
            ImprovementKind::PromptStabilization => "PromptStabilization",
            ImprovementKind::RoutingRestriction => "RoutingRestriction",
            ImprovementKind::ContextReset => "ContextReset",
            ImprovementKind::JitMemory => "JitMemory",
        }
    }
}

/// One inert recommendation for a role.
#[derive(Debug, Clone, PartialEq)]
pub struct ImprovementProposal {
    pub role: String,
    pub kind: ImprovementKind,
    pub rationale: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct TasteEngineConfig {
    /// Drop proposals below this confidence.
    pub min_confidence: f64,
    /// RDS at/below which a role is "fragile" at long horizons.
    pub fragile_rds: f64,
    /// Overall score below which a model switch is proposed.
    pub low_overall: f64,
    /// Consistency below which prompt stabilization is proposed.
    pub low_consistency: f64,
    /// MOP rate above which context reset is proposed.
    pub high_mop_rate: f64,
}

impl Default for TasteEngineConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
            fragile_rds: -0.10,
            low_overall: 0.5,
            low_consistency: 0.6,
            high_mop_rate: 0.3,
        }
    }
}

/// Generate confidence-gated proposals for a role from its taste score. Needs
/// at least `min_samples` observations to say anything.
pub fn propose(
    role: &str,
    ts: &TasteScore,
    samples: usize,
    cfg: &TasteEngineConfig,
) -> Vec<ImprovementProposal> {
    let mut out = Vec::new();
    if samples < 3 {
        return out; // not enough evidence
    }
    let mk = |kind, rationale: String, confidence: f64| ImprovementProposal {
        role: role.to_string(),
        kind,
        rationale,
        confidence: confidence.clamp(0.0, 1.0),
    };

    if ts.overall() < cfg.low_overall {
        let conf = 0.5 + (cfg.low_overall - ts.overall());
        out.push(mk(
            ImprovementKind::ModelSwitch,
            format!(
                "overall taste {:.2} below {:.2}",
                ts.overall(),
                cfg.low_overall
            ),
            conf,
        ));
    }
    if ts.reliability_decay_slope <= cfg.fragile_rds {
        let conf = 0.5 + (-ts.reliability_decay_slope);
        out.push(mk(
            ImprovementKind::RoutingRestriction,
            format!(
                "reliability decays steeply at long horizons (RDS {:.2})",
                ts.reliability_decay_slope
            ),
            conf,
        ));
    }
    if ts.mop_rate > cfg.high_mop_rate {
        out.push(mk(
            ImprovementKind::ContextReset,
            format!("meltdown rate {:.2} on long tasks", ts.mop_rate),
            0.5 + ts.mop_rate,
        ));
    }
    if ts.consistency < cfg.low_consistency && ts.quality >= 0.6 {
        out.push(mk(
            ImprovementKind::PromptStabilization,
            format!(
                "capable but inconsistent (consistency {:.2})",
                ts.consistency
            ),
            0.5 + (cfg.low_consistency - ts.consistency),
        ));
    }

    out.retain(|p| p.confidence >= cfg.min_confidence);
    out
}
