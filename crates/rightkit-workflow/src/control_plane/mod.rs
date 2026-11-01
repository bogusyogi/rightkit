//! # host_workflow — autonomous workflow control plane (pure domain)
//!
//! This crate implements the host app's **durable outer workflow DAG**. It is the
//! only lifecycle authority for autonomous workflows. The chat, the BREWS
//! `TaskGraph`, the conductor, native sessions, Codex/Claude lanes, and
//! recipes operate *inside* one bounded workflow node — they may adapt
//! tactics but may not widen authority or alter outer lifecycle semantics.
//!
//! See [`docs/../plans/pending/2026-08-06-autonomous-workflow-control-plane.md`](https://example.invalid/autonomous-workflow-control-plane-plan)
//! and current workflow authority under `docs/pending/workflow-control-plane.md`.
//!
//! ## Crate discipline (WP-00)
//!
//! At this stage the crate **must** stay a pure data + pure-function domain:
//!
//! - **No** Axum, `tokio` (process execution), `rusqlite`, provider,
//!   `driver_host`, or Tauri dependencies.
//! - Deterministic IDs and a deterministic test clock are the only stateful
//!   test seams; production callers obtain a real `Clock` / `IdGenerator` at
//!   the daemon boundary (WP-03).
//! - No production dispatch is enabled from this crate yet — the supervisor
//!   and queue live in `engine/host-app/src/workflow` (WP-03).
//!
//! ## Module layout
//!
//! The structure below anticipates WP-01 / WP-02 / WP-03. Modules may compile
//! as `mod` skeletons now and grow later; we do not silently implement later
//! work packages from WP-00.
//!
//! - [`domain`] — explicit IDs, types, states, commands, events, and failure
//!   classes (filled in by WP-01).
//! - [`compiler`] — deterministic graph validation and canonical hashing
//!   (WP-01).
//! - [`reducer`] — pure command/event reducer (WP-01).
//! - [`clock`] / [`ids`] — deterministic test seams (this package).
//! - [`feature_gate`] — explicit reference to the daemon's workflow feature
//!   gate, so callers cannot accidentally bypass it.
//!
//! ## Tests
//!
//! `cargo test -p host_workflow` runs the pure-domain suites. The crate
//! has zero I/O and zero side effects in its API, so unit tests are
//! sufficient at this stage. Property and integration tests land in WP-01.

#![forbid(unsafe_code)]
#![allow(missing_docs)]

pub mod a2a;
pub mod approvals;
pub mod bindings;
pub mod clock;
pub mod compiler;
pub mod context;
pub mod control;
pub mod domain;
pub mod effect;
pub mod executors;
pub mod expression;
pub mod feature_gate;
pub mod ids;
pub mod import_export;
pub mod interop;
pub mod migration;
pub mod outbox;
pub mod privacy;
pub mod reducer;
pub mod reference_workflows;
pub mod telemetry;
pub mod trigger;
pub mod verifier_binding;
pub mod waits;

/// Re-exports for callers wiring this crate into a daemon.
pub mod prelude {
    pub use crate::control_plane::a2a::{
        classify_output, validate_admission, validate_descriptor, A2aError, A2aExecutorDescriptor,
        A2aIdentityProof, A2aIdentityVerifier, A2aResult, RejectUnverifiedIdentity,
    };
    pub use crate::control_plane::approvals::{
        apply_decision, is_expired, is_hash_bound, ApprovalDecision, ApprovalError,
        ApprovalOutcome, ApprovalRequest,
    };
    pub use crate::control_plane::bindings::{
        Binding, BindingError, BindingKind, BindingPath, BindingPathSegment, NodeOutputState,
        ScopeContext, ScopeError, ScopeValidator, CAP_READ_ENV, CAP_READ_SECRET,
    };
    pub use crate::control_plane::clock::{Clock, SystemClock, TestClock};
    pub use crate::control_plane::compiler::{
        compile, validate_source_authority, CompilerError, CompilerOutcome, SourceAuthority,
        WorkflowSource, WorkspaceAuthority,
    };
    pub use crate::control_plane::context::{
        ContextBudget, ContextBuilder, ContextError, ContextManifest, ContextProvenance,
        ContextSection, IterationKind, LAST_ERROR_MAX_LEN, MAX_SECTIONS,
    };
    pub use crate::control_plane::control::{ControlError, ControlPlan, MAX_ITERATION_CARDINALITY};
    pub use crate::control_plane::domain::{
        BackoffStrategy, Capability, CapabilitySet, CompiledPlan, ContentHash, EdgeSpec,
        FailureClass, JournalEvent, LifecyclePhase, LifecycleState, NodeConfig, NodeKind, NodeSpec,
        RetryPolicy, Revision, RunId, WorkflowDefinition, WorkflowId, WorkflowVersion,
        WorkflowVersionId,
    };
    pub use crate::control_plane::effect::{
        is_v1_allowed, EffectAdapterRegistry, EffectIntent, EffectOutcome, EffectReceipt,
    };
    pub use crate::control_plane::executors::{
        ExecutorAdapter, ExecutorError, NodeOutput, PendingReason,
    };
    pub use crate::control_plane::expression::{
        parse as parse_expr, parse_json_binding, CmpOp, EvalContext, Expr, ExpressionError,
        ParseError,
    };
    pub use crate::control_plane::feature_gate::{workflow_feature_default, FeatureGate};
    pub use crate::control_plane::ids::{IdGenerator, NamespacedId, Prefix, StableId};
    pub use crate::control_plane::import_export::{
        build as build_package, quarantine as quarantine_package, verify_checksum, verify_import,
        ImportError, PackageManifest, PackageSignatureVerifier, QuarantineStatus,
        RejectUnsignedPackage, WorkflowPackage,
    };
    pub use crate::control_plane::interop::{
        map_status, project_run_to_mcp, validate_external_task_create_request, McpBoundaryError,
        McpTaskStatus, McpTaskView, WorkflowRunRow as McpWorkflowRunRow,
    };
    pub use crate::control_plane::migration::{
        migrate_to_source, migrate_to_workflow, LegacySchedule, MigratedWorkflow, MigrationError,
    };
    pub use crate::control_plane::outbox::{mark_delivered, OutboxItem, OutboxKind, OutboxStatus};
    pub use crate::control_plane::reducer::{reduce, ReducerError, ReducerOutcome};
    pub use crate::control_plane::telemetry::{
        correlation_scope, redact, TelemetryError, TelemetryEvent, TelemetryKind,
    };
    pub use crate::control_plane::trigger::{
        is_dst_safe, next_occurrence, DedupeStrategy, MisfirePolicy, OccurrencePolicy,
        OverlapPolicy, Trigger, TriggerError, TriggerKind,
    };
    pub use crate::control_plane::waits::{
        matches_event, matches_event_filter, DurableWait, WaitKind, WaitResume,
    };
}

#[cfg(test)]
mod skeleton_tests {
    //! Skeleton tests proving the crate is wired and the public API surface
    //! is reachable. Real domain tests arrive with WP-01.

    use super::prelude::*;

    #[test]
    fn skeleton_compiles_and_exposes_prelude() {
        // The feature gate default is `false`. Any release that flips this to
        // `true` before the WP-08 release gate is a contract violation.
        assert!(!workflow_feature_default());
        let gate = FeatureGate::default();
        assert!(!gate.is_enabled());
        let gate = FeatureGate::from_env_or_default();
        assert!(!gate.is_enabled());
    }

    #[test]
    fn clock_can_be_seeded_deterministically() {
        let mut clock = TestClock::new(1_000_000);
        assert_eq!(clock.now_unix_ms(), 1_000_000);
        clock.advance_ms(250);
        assert_eq!(clock.now_unix_ms(), 1_000_250);
    }

    #[test]
    fn ids_are_deterministic_for_seeded_generator() {
        let mut a = IdGenerator::seeded("test-seed-A");
        let mut b = IdGenerator::seeded("test-seed-A");
        assert_eq!(
            a.next_id(Prefix::WorkflowDefinition),
            b.next_id(Prefix::WorkflowDefinition)
        );
        // A different seed produces a different id sequence.
        let mut c = IdGenerator::seeded("test-seed-B");
        assert_ne!(
            a.next_id(Prefix::WorkflowDefinition),
            c.next_id(Prefix::WorkflowDefinition)
        );
    }
}
