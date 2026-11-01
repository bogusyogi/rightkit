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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::context::{
        ContextBudget, ContextBuilder, ContextManifest, ContextSection, IterationKind,
    };
    use crate::control_plane::domain::{
        BackoffStrategy, Capability, CapabilitySet, EdgeSpec, FailureClass, NodeKind,
        NodeSpec as DomainNodeSpec, RetryPolicy,
    };
    use crate::control_plane::domain::{NodeId, RunId};
    use crate::control_plane::ids::Prefix;
    use serde_json::json;

    // --- helpers -------------------------------------------------------------

    fn run_id(s: &str) -> RunId {
        RunId::new(Prefix::WorkflowRun, s)
    }

    fn node_id(s: &str) -> NodeId {
        NodeId::new(Prefix::NodeRun, s)
    }

    fn spec(kind: NodeKind) -> DomainNodeSpec {
        DomainNodeSpec {
            id: format!("n_test_{kind:?}"),
            kind,
            label: None,
            capabilities: CapabilitySet::from_iter([Capability::new("fs.read")]),
            edges: Vec::<EdgeSpec>::new(),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffStrategy::Fixed { delay_ms: 100 },
                retryable: Vec::new(),
            },
            config: Default::default(),
        }
    }

    fn manifest_with_cap(caps: &[&str]) -> ContextManifest {
        let cap_set = CapabilitySet::from_iter(caps.iter().map(|s| Capability::new(*s)));
        ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n1"), 0)
            .with_capabilities(cap_set)
            .with_budget(ContextBudget {
                deadline_unix_ms: Some(60_000),
                ..ContextBudget::unbounded()
            })
            .build()
            .expect("build manifest")
    }

    /// Minimal adapter that returns a deterministic Success payload for
    /// any kind. Used to exercise the trait surface.
    struct StubOkAdapter;

    impl ExecutorAdapter for StubOkAdapter {
        fn name(&self) -> &'static str {
            "stub_ok"
        }
        fn execute(
            &self,
            _spec: &DomainNodeSpec,
            _manifest: &ContextManifest,
        ) -> Result<NodeOutput, ExecutorError> {
            Ok(NodeOutput::Success {
                output_json: json!({"ok": true}),
                artifacts: vec!["artifact-1".to_string()],
                cost_usd_micros: 0,
                tokens_used: 0,
            })
        }
    }

    /// Minimal adapter that returns a deterministic retryable failure.
    struct StubRetryAdapter;

    impl ExecutorAdapter for StubRetryAdapter {
        fn name(&self) -> &'static str {
            "stub_retry"
        }
        fn execute(
            &self,
            _spec: &DomainNodeSpec,
            _manifest: &ContextManifest,
        ) -> Result<NodeOutput, ExecutorError> {
            Ok(NodeOutput::Failure {
                class: FailureClass::TransientProvider,
                message: "transient blip".to_string(),
                retryable: true,
            })
        }
    }

    /// Adapter that only supports `NodeKind::Transform`.
    struct TransformOnlyAdapter;

    impl ExecutorAdapter for TransformOnlyAdapter {
        fn name(&self) -> &'static str {
            "transform_only"
        }
        fn supports(&self, kind: NodeKind) -> bool {
            matches!(kind, NodeKind::Transform)
        }
        fn execute(
            &self,
            _spec: &DomainNodeSpec,
            _manifest: &ContextManifest,
        ) -> Result<NodeOutput, ExecutorError> {
            Ok(NodeOutput::Success {
                output_json: json!({"kind": "transform"}),
                artifacts: vec![],
                cost_usd_micros: 0,
                tokens_used: 0,
            })
        }
    }

    // --- tests ---------------------------------------------------------------

    #[test]
    fn stub_ok_adapter_returns_success_for_any_kind() {
        let adapter = StubOkAdapter;
        let manifest = manifest_with_cap(&["fs.read"]);
        for kind in [
            NodeKind::Agent,
            NodeKind::Recipe,
            NodeKind::Transform,
            NodeKind::Map,
        ] {
            let out = adapter.execute(&spec(kind), &manifest).expect("execute ok");
            assert!(out.is_success(), "expected success for {kind:?}");
            match out {
                NodeOutput::Success {
                    output_json,
                    artifacts,
                    ..
                } => {
                    assert_eq!(output_json, json!({"ok": true}));
                    assert_eq!(artifacts, vec!["artifact-1".to_string()]);
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn stub_retry_adapter_reports_retryable_failure() {
        let adapter = StubRetryAdapter;
        let manifest = manifest_with_cap(&[]);
        let out = adapter
            .execute(&spec(NodeKind::Agent), &manifest)
            .expect("execute");
        assert!(!out.is_success());
        assert!(out.is_retryable_failure());
        match out {
            NodeOutput::Failure {
                class, retryable, ..
            } => {
                assert_eq!(class, FailureClass::TransientProvider);
                assert!(retryable);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn transform_only_adapter_rejects_unsupported_kind_via_capability_check() {
        let adapter = TransformOnlyAdapter;
        let manifest = manifest_with_cap(&[]);
        // Supports Transform.
        assert!(adapter
            .check_capability(&spec(NodeKind::Transform), &manifest)
            .is_ok());
        // Does not support Agent.
        let err = adapter
            .check_capability(&spec(NodeKind::Agent), &manifest)
            .expect_err("Agent must be rejected");
        match err {
            ExecutorError::UnsupportedKind(name) => assert_eq!(name, "transform_only"),
            other => panic!("expected UnsupportedKind, got {other:?}"),
        }
    }

    #[test]
    fn node_output_kind_labels_are_stable() {
        let cases: &[(&str, NodeOutput)] = &[
            (
                "success",
                NodeOutput::Success {
                    output_json: json!(null),
                    artifacts: vec![],
                    cost_usd_micros: 0,
                    tokens_used: 0,
                },
            ),
            (
                "failure",
                NodeOutput::Failure {
                    class: FailureClass::TransientProvider,
                    message: "x".into(),
                    retryable: false,
                },
            ),
            (
                "timeout",
                NodeOutput::Timeout {
                    elapsed_ms: 100,
                    partial: None,
                },
            ),
            (
                "cancelled",
                NodeOutput::Cancelled {
                    reason: "lease lost".into(),
                },
            ),
            (
                "pending",
                NodeOutput::Pending {
                    reason_kind: PendingReason::AwaitingApproval,
                    note: "x".into(),
                    will_retry_after_unix_ms: None,
                },
            ),
        ];
        for (expected, output) in cases {
            assert_eq!(output.kind_label(), *expected, "wrong label for {output:?}");
        }
    }

    #[test]
    fn node_output_serde_round_trip_preserves_variant() {
        let outputs = [
            NodeOutput::Success {
                output_json: json!({"a": 1}),
                artifacts: vec!["x".into()],
                cost_usd_micros: 100,
                tokens_used: 7,
            },
            NodeOutput::Failure {
                class: FailureClass::PolicyDenied,
                message: "denied".into(),
                retryable: false,
            },
            NodeOutput::Timeout {
                elapsed_ms: 60_000,
                partial: Some(json!({"partial": true})),
            },
            NodeOutput::Cancelled {
                reason: "user".into(),
            },
            NodeOutput::Pending {
                reason_kind: PendingReason::AwaitingTimer,
                note: "wake in 30s".into(),
                will_retry_after_unix_ms: Some(60_000),
            },
        ];
        for output in outputs {
            let json = serde_json::to_string(&output).expect("serialize");
            let back: NodeOutput = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(output, back);
        }
    }

    #[test]
    fn adapter_default_supports_returns_true_for_every_kind() {
        let adapter = StubOkAdapter;
        let kinds = [
            NodeKind::Agent,
            NodeKind::Recipe,
            NodeKind::Transform,
            NodeKind::Condition,
            NodeKind::Join,
            NodeKind::Map,
            NodeKind::Loop,
            NodeKind::WaitTimer,
            NodeKind::WaitEvent,
            NodeKind::Approval,
            NodeKind::Effect,
            NodeKind::Subworkflow,
        ];
        for kind in kinds {
            assert!(
                adapter.supports(kind),
                "default supports() should be true for {kind:?}"
            );
        }
    }

    #[test]
    fn adapter_required_capabilities_default_is_empty() {
        let adapter = StubOkAdapter;
        let caps = adapter.required_capabilities(&spec(NodeKind::Agent));
        assert!(caps.is_empty());
    }

    #[test]
    fn pending_reason_serializes_as_snake_case_string() {
        let p = NodeOutput::Pending {
            reason_kind: PendingReason::AwaitingEvent,
            note: "ping".into(),
            will_retry_after_unix_ms: None,
        };
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(json.contains("awaiting_event"), "got: {json}");
        let back: NodeOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn timeout_with_partial_output_round_trips() {
        let t = NodeOutput::Timeout {
            elapsed_ms: 5000,
            partial: Some(json!({"items": [1, 2, 3]})),
        };
        assert_eq!(t.kind_label(), "timeout");
        assert!(!t.is_success());
        assert!(!t.is_retryable_failure());
        let json = serde_json::to_string(&t).expect("serialize");
        let back: NodeOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    #[test]
    fn iteration_section_is_visible_to_adapter_through_manifest() {
        // Adapters that need the iteration index read it from the manifest.
        // This is a smoke test: the adapter sees the projection, returns
        // success.
        let manifest = ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n1"), 0)
            .with_capabilities(CapabilitySet::empty())
            .push(ContextSection::Iteration {
                index: 7,
                kind: IterationKind::Map,
                item: json!("alpha"),
            })
            .expect("push iteration")
            .build()
            .expect("build");
        let iter = manifest.iteration().expect("iteration section");
        match iter {
            ContextSection::Iteration { index, kind, item } => {
                assert_eq!(*index, 7);
                assert_eq!(*kind, IterationKind::Map);
                assert_eq!(*item, json!("alpha"));
            }
            _ => panic!("expected iteration section"),
        }
    }
}
