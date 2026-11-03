//! `MechanismClass` — the finite label set for PL-010 mechanism-first
//! execution resolution.
//!
//! The resolver (`super::resolver::MechanismResolver`) answers one question
//! for every root task or post-decomposition child node: **does this need a
//! model at all?** Every answer binds one of these classes. The label set is
//! closed: a bounded classifier may only propose a value from this enum, never
//! free-form text, and only a value that a registered executor can actually
//! bind.
//!
//! # Research lineage (design-informed, no code donor)
//!
//! PL-010 is an `ORIGINAL` atom (canon: "No compatible donor; research-informed
//! (PAL/LLM+P/MRKL/LLMCompiler/LATM)"). The classes carry those mechanisms'
//! contract shapes without lifting any third-party source:
//!
//! - **MRKL** routes a query to specialized solver modules or to the LM
//!   reasoner — the shape of [`MechanismClass::Scripted`], [`MechanismClass::Process`]
//!   and [`MechanismClass::SourceResolver`] as first-class route targets.
//! - **PAL** (program-aided language) lets the model emit a program but the
//!   runtime execute it deterministically — the shape of
//!   [`MechanismClass::StructuredCodeOp`] with a schema-verified output.
//! - **LLM+P** translates a task into a formal problem solved by a classical
//!   planner — the shape of deterministic resolution over structured signals
//!   before any model is consulted.
//! - **LLMCompiler** executes a planned DAG of function calls on a
//!   deterministic executor and falls back to the LLM only on failure — the
//!   shape of the resolution order (deterministic first, bounded classifier
//!   last) and of `super::decision::MechanismEscalationReceipt`.
//! - **LATM** reuses a made tool instead of re-asking the model — the shape of
//!   the exact-capability-match stage projecting from the existing tool
//!   registry (no second registry).

use serde::{Deserialize, Serialize};

/// The closed set of execution mechanisms a node may be bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MechanismClass {
    /// A deterministic built-in orchestration capability (status probes,
    /// catalog reads, no-op fixtures). No model, no external process.
    Builtin,
    /// A deterministic scripted capability (a registered script/recipe/tool
    /// run verbatim). MRKL "solver module" / LATM "reused tool" shape.
    Scripted,
    /// The model may *write* the operation, but the runtime executes it and
    /// verifies the output deterministically against a contract (PAL shape).
    StructuredCodeOp,
    /// A deterministic process execution (spawn/execute with typed receipts).
    Process,
    /// A deterministic answer derived from authoritative sources
    /// (`source.read` / Ledger/Cortex bindings); verified by digest coverage,
    /// never by model judgment (LLM+P shape).
    SourceResolver,
    /// A semantic model must run. This is the ONLY class that hands off to
    /// model routing (`host_router`); model selection itself stays there.
    SemanticModel,
    /// A human is the executor. Never auto-escalated to by heuristics — only
    /// an explicit binding or an explicit approval signal may bind it.
    Human,
    /// The requirement names a mechanism the system does not support. Terminal
    /// for this resolution; never a model fallback.
    Unsupported,
}

impl std::fmt::Display for MechanismClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl MechanismClass {
    /// Stable snake_case label, identical to the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Scripted => "scripted",
            Self::StructuredCodeOp => "structured_code_op",
            Self::Process => "process",
            Self::SourceResolver => "source_resolver",
            Self::SemanticModel => "semantic_model",
            Self::Human => "human",
            Self::Unsupported => "unsupported",
        }
    }

    /// True when the class executes without any model inference. The resolver
    /// may bind these deterministically; it may never *fabricate* a binding
    /// for one when no executor exists.
    pub fn is_deterministic(self) -> bool {
        matches!(
            self,
            Self::Builtin
                | Self::Scripted
                | Self::StructuredCodeOp
                | Self::Process
                | Self::SourceResolver
        )
    }

    /// True when binding this class means model routing runs next.
    pub fn requires_model_routing(self) -> bool {
        self == Self::SemanticModel
    }
}
