//! WP-05 — `ExecutorAdapter` trait + typed [`NodeOutput`] enum.
//!
//! ADR-0021 says the outer workflow DAG is the **only** lifecycle authority.
//! Inner executors (native, Codex, Claude, conductor, recipes) may adapt
//! tactics inside a single bounded node, but they may not widen authority,
//! bypass the reducer, or rewrite outer lifecycle semantics. This module is
//! the *contract* between the outer DAG and those inner executors.
//!
//! ## Crate discipline
//!
//! This module is **pure domain**. It depends on `serde`, `serde_json`,
//! `thiserror`, and `tracing` only — no `tokio` (process), no `rusqlite`, no
//! providers, no `driver_host`, no Tauri. Adapters in the daemon binary
//! (`engine/host-app/src/workflow/executors.rs`) implement this trait
//! using whatever async strategy they need; the trait itself stays sync so
//! the workflow crate remains free of runtime dependencies.
//!
//! ## Adapter contract
//!
//! 1. **One manifest in, one [`NodeOutput`] out.** The adapter receives an
//!    immutable [`ContextManifest`]. It must
//!    never mutate the manifest, never log secret values, and never
//!    persist raw transcript content.
//! 2. **Honor the budget.** If the adapter exhausts
//!    [`ContextBudget::deadline_unix_ms`](crate::control_plane::context::ContextBudget::deadline_unix_ms)
//!    or any of the cost / token / retry ceilings, it must return
//!    [`NodeOutput::Timeout`] or [`NodeOutput::Failure`] with the matching
//!    [`FailureClass`]. An adapter that *ignores* the budget is a contract
//!    violation.
//! 3. **Typed results only.** [`NodeOutput`] is a closed enum. An adapter
//!    that needs to surface a domain-specific failure must extend
//!    [`FailureClass`] (in WP-01's vocabulary) — there is no "free-form
//!    string error" escape hatch.
//! 4. **Capability gating.** The adapter is responsible for checking that
//!    the manifest's [`CapabilitySet`](crate::control_plane::domain::CapabilitySet)
//!    contains the capability it needs (e.g. `network.fetch`,
//!    `effect.publish`). Adapters that cannot enforce their own capability
//!    check must return [`ExecutorError::CapabilityDenied`].
//! 5. **Cancellation.** Adapters must honor a cooperative cancellation
//!    signal (WP-06 territory). For now, the adapter trait returns
//!    synchronously; the WP-06 wiring adds a `CancellationToken` parameter.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control_plane::context::ContextManifest;
use crate::control_plane::domain::{Capability, FailureClass, NodeKind, NodeSpec};

// ─── NodeOutput ──────────────────────────────────────────────────────────────

/// One bounded inner-executor result.
///
/// The outer workflow reducer consumes [`NodeOutput`] (via
/// [`crate::control_plane::reducer::reduce`]) to decide what journal event to emit next.
/// This is intentionally a *closed* enum — there is no "free-form
/// error string" variant. New failure shapes are added by extending
/// [`FailureClass`], not by smuggling strings through the trait.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "value")]
pub enum NodeOutput {
    /// The executor produced a successful result. The outer reducer emits
    /// `node_completed` and advances the run lifecycle.
    Success {
        /// Structured, schema-bound node result. The daemon serializes this
        /// into the `workflow_node_states.output_json` projection.
        output_json: serde_json::Value,
        /// Artifact ids (relative to the workspace) the node produced.
        /// The reducer forwards these to the run's artifact index.
        artifacts: Vec<String>,
        /// Cost in micros of a US dollar. Mirrors
        /// `host_budget::BudgetLedger::record_cost` so the same units
        /// roll into the workflow budget.
        cost_usd_micros: u64,
        /// Total tokens consumed across all provider calls in this node.
        tokens_used: u64,
    },
    /// The executor finished with a *typed* failure. The outer reducer
    /// consults the node's retry policy and the `FailureClass` taxonomy to
    /// decide whether to retry, escalate, or terminate the run.
    Failure {
        /// Typed failure class. Must be one of the [`FailureClass`]
        /// variants; no free-form strings.
        class: FailureClass,
        /// Human-readable message (bounded by the reducer; the adapter
        /// should keep it under ~1 KiB).
        message: String,
        /// Whether the failure class permits automatic retry. Adapters
        /// that know the answer set this to `true`/`false` explicitly;
        /// the reducer does not have to re-derive it.
        retryable: bool,
    },
    /// The executor hit its wall-clock deadline before completing. The
    /// outer reducer maps this to [`FailureClass::DeadlineExceeded`] and
    /// decides whether to retry or terminate.
    Timeout {
        /// Elapsed wall-clock milliseconds at the moment the adapter
        /// timed out.
        elapsed_ms: i64,
        /// Optional partial output — the reducer may forward this on
        /// retry so the next attempt can resume.
        partial: Option<serde_json::Value>,
    },
    /// The executor was cancelled (cooperatively or by the supervisor
    /// losing the lease). The reducer maps this to
    /// [`FailureClass::Cancelled`] and emits a `node_failed` event.
    Cancelled {
        /// Why the executor was cancelled. One short sentence; no
        /// transcripts, no model output.
        reason: String,
    },
    /// The executor is *not* ready to produce a result yet (e.g. it is
    /// waiting on an approval, a durable timer, or an external event).
    /// The reducer pauses the run and emits a `node_waiting` journal
    /// event; a future attempt will resume from this state.
    ///
    /// This variant is **not** a transient retry signal. Adapters that
    /// hit a transient error should return [`NodeOutput::Failure`] with
    /// [`FailureClass::TransientProvider`] / [`FailureClass::TransientNetwork`]
    /// instead, so the reducer can apply its bounded retry policy.
    Pending {
        /// Stable lowercase discriminator so the journal can index the
        /// wait reason without committing to a free-form string.
        reason_kind: PendingReason,
        /// Free-form short note (bounded; no transcript content).
        note: String,
        /// Optional wall-clock hint for when the executor expects to
        /// become ready again. The reducer does not promise to honor it;
        /// it is an observability field.
        will_retry_after_unix_ms: Option<i64>,
    },
}

/// Closed set of `Pending` reason kinds. The reducer keys journal
/// projections and wait timers on this enum; free-form strings are not
/// permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    /// Waiting on a durable timer (`WaitTimer` node).
    AwaitingTimer,
    /// Waiting on an external event (`WaitEvent` node).
    AwaitingEvent,
    /// Waiting on a principal's approval (`Approval` node).
    AwaitingApproval,
    /// Waiting on a subworkflow to complete (`Subworkflow` node).
    AwaitingSubworkflow,
    /// Waiting on an effect reconciliation outcome (`Effect` node).
    AwaitingEffect,
    /// Adapter-specific hold; the adapter must set `note` so a reviewer can
    /// diagnose why the executor paused.
    AdapterHold,
}

impl NodeOutput {
    /// Stable lowercase discriminator for logging and journal indexing.
    pub const fn kind_label(&self) -> &'static str {
        match self {
            NodeOutput::Success { .. } => "success",
            NodeOutput::Failure { .. } => "failure",
            NodeOutput::Timeout { .. } => "timeout",
            NodeOutput::Cancelled { .. } => "cancelled",
            NodeOutput::Pending { .. } => "pending",
        }
    }

    /// True when this output represents a terminal-success state for the
    /// attempt. The reducer treats `true` as "advance lifecycle".
    pub fn is_success(&self) -> bool {
        matches!(self, NodeOutput::Success { .. })
    }

    /// True when this output represents a *retryable* failure. Adapters
    /// that know a transient condition caused the failure set
    /// [`NodeOutput::Failure`] with `retryable: true`; the reducer then
    /// consults the node's [`RetryPolicy`](crate::control_plane::domain::RetryPolicy) to
    /// decide whether to schedule another attempt.
    pub fn is_retryable_failure(&self) -> bool {
        matches!(
            self,
            NodeOutput::Failure {
                retryable: true,
                ..
            }
        )
    }
}

// ─── ExecutorError ───────────────────────────────────────────────────────────

/// Typed failures an adapter can surface. These are *adapter-side* errors
/// (the adapter itself broke), distinct from
/// [`NodeOutput::Failure`] which is *node-side* (the node's task failed
/// gracefully).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecutorError {
    /// The adapter does not know how to execute `kind`. The reducer
    /// maps this to a `node_failed` event with
    /// [`FailureClass::InvalidDefinition`] (the workflow was wired against
    /// an adapter that does not exist).
    #[error("adapter does not support node kind `{0}`")]
    UnsupportedKind(&'static str),
    /// The adapter needed a capability that the manifest did not carry.
    /// The reducer maps this to [`FailureClass::PolicyDenied`].
    #[error("adapter `{adapter}` requires capability `{required}` for kind `{kind:?}`")]
    CapabilityDenied {
        /// Name of the adapter that refused.
        adapter: &'static str,
        /// Capability that was missing.
        required: String,
        /// Node kind the adapter was asked to execute.
        kind: NodeKind,
    },
    /// The adapter refused to run because the manifest was malformed
    /// (missing required section, invalid constant hash, etc.). The
    /// reducer maps this to [`FailureClass::InvalidBinding`].
    #[error("adapter `{adapter}` rejected manifest: {reason}")]
    InvalidManifest {
        /// Name of the adapter that refused.
        adapter: &'static str,
        /// Why the manifest was rejected.
        reason: String,
    },
    /// The adapter hit an internal / unexpected error. The reducer maps
    /// this to [`FailureClass::ExecutorUnavailable`] and may retry.
    #[error("adapter `{adapter}` internal error: {message}")]
    Internal {
        /// Name of the adapter that errored.
        adapter: &'static str,
        /// Human-readable message.
        message: String,
    },
}

impl ExecutorError {
    /// Convenience: build an [`ExecutorError::Internal`] with a static
    /// adapter name.
    pub fn internal(adapter: &'static str, message: impl Into<String>) -> Self {
        ExecutorError::Internal {
            adapter,
            message: message.into(),
        }
    }
}

// ─── ExecutorAdapter ─────────────────────────────────────────────────────────

/// The contract between the outer workflow DAG and a bounded inner
/// executor.
///
/// Adapters are *stateless with respect to the run* — every piece of state
/// they need is in the [`ContextManifest`] they receive. An adapter that
/// wants to remember something across attempts must record it in the
/// manifest's `output_json` and let the reducer commit it to the journal.
///
/// The trait is synchronous so the workflow crate stays free of runtime
/// dependencies. Daemon-side adapters (`engine/host-app/src/workflow/executors.rs`)
/// are free to do `tokio::runtime::Handle::current().block_on(...)` or
/// `tokio::task::block_in_place(...)` internally to bridge to async work.
pub trait ExecutorAdapter: Send + Sync {
    /// Stable lowercase identifier used for logging, metrics, and the
    /// `executor_id` column of the journal. Examples: `"native"`,
    /// `"codex"`, `"claude"`, `"conductor"`, `"recipe"`.
    fn name(&self) -> &'static str;

    /// True when the adapter knows how to execute `kind`. The reducer
    /// looks up the adapter by kind at dispatch time; an adapter that
    /// returns `false` for `kind` is skipped, and the dispatcher falls
    /// through to the next registered adapter.
    ///
    /// Defaults to "supports every kind" so adapters that have a single
    /// implementation path do not have to override this method. Adapters
    /// that genuinely support only a subset (e.g. a "transform-only"
    /// adapter) override it to narrow the surface.
    fn supports(&self, kind: NodeKind) -> bool {
        let _ = kind;
        true
    }

    /// Run one node attempt against `manifest`. The adapter MUST honor
    /// the manifest's [`ContextBudget`](crate::control_plane::context::ContextBudget) and
    /// MUST return a typed [`NodeOutput`] (or a typed [`ExecutorError`])
    /// for every code path. There is no panic / unwrap escape hatch in
    /// production adapters.
    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<NodeOutput, ExecutorError>;

    /// Optional capability check. The default implementation verifies the
    /// adapter knows how to dispatch the kind and refuses with
    /// [`ExecutorError::UnsupportedKind`] otherwise; adapters that need
    /// extra capabilities override this.
    fn check_capability(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<(), ExecutorError> {
        if !self.supports(spec.kind) {
            return Err(ExecutorError::UnsupportedKind(self.name()));
        }
        let _ = manifest;
        Ok(())
    }

    /// Convenience: list of capabilities the adapter needs to run *any*
    /// kind. The reducer uses this to verify the manifest's capability
    /// set before dispatch; defaults to `&[]` so adapters that need no
    /// special capabilities do not have to override.
    fn required_capabilities(&self, _spec: &NodeSpec) -> Vec<Capability> {
        Vec::new()
    }
}
