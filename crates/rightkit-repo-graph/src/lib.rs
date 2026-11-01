//! Native Blueprint foundation and in-process service lifecycle.
//!
//! Blueprint is exposed as typed Rust modules. Resident ownership is provided
//! by [`service`]; bounded explicit work uses [`service::OneShotExecutor`]
//! without requiring resident ownership.

/// Diagnostic line on stderr that can never panic. `eprintln!` panics when stderr is a closed pipe
/// (a resident outliving its supervisor, or a service with no console); in a watcher thread that
/// killed the thread and left Blueprint `watcher_unavailable` (#555). A failed write is dropped.
macro_rules! stderr_line {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub mod api;
pub mod cli;
pub mod contracts;
pub mod doc_truth;
pub mod engine;
pub mod export;
pub mod findings;
pub mod freshness;
pub mod graph;
pub mod hybrid_task;
pub mod identity;
pub mod index;
pub mod migrations;
pub mod model;
pub mod query;
pub mod security;
pub mod service;
pub mod skeletonization;
pub mod store;
pub mod watch;

pub use api::{
    BlueprintApi, BlueprintBounds, BlueprintError, BlueprintOperation, BlueprintRequest,
    BlueprintResponse, BlueprintWireRequest, BlueprintWireResponse, Bounds, CancellationToken,
    Deadline, FreshnessBinding, FreshnessState, GenerationBinding, GraphEdge, GraphNode,
    RequestContext,
};
pub use cli::{
    architecture_orientation, expand, manual_refresh, recall, run_one_shot, run_query, search,
    status,
};
pub use engine::{native_blueprint_operation, NativeBlueprintOperation};
pub use export::{build_evidence_pack, execute_export, findings_to_sarif};
pub use findings::execute_findings;
pub use graph::GraphGeneration;
pub use model::{ImpactFrontierClass, Operation, RepositoryScope, RequestScope, PROTOCOL_VERSION};
pub use service::{
    execute_one_shot, BlueprintService, EventCollector, LifecycleEvent, LifecycleEventKind,
    LifecycleEventSink, LifecycleState, NativeService, OneShotExecutor, Readiness, ServiceConfig,
    ServiceError, ServiceStatus, Supervisor,
};
pub use skeletonization::{
    expand_region, skeletonize, skeletonize_for_task, Region, SkeletonResult,
    DEFAULT_SIZE_THRESHOLD,
};

/// Map a stable error code to lifecycle degradation without requiring callers
/// to parse implementation-specific error text.
pub fn lifecycle_state_for_error(error: &str) -> LifecycleState {
    let value = error.to_ascii_lowercase();
    if value.contains("hub_inactive") || value.contains("membrane_not_running") {
        return LifecycleState::HubInactive;
    }
    if value.contains("resident_owner_active") {
        return LifecycleState::ResidentOwnerActive;
    }
    if value.contains("watcher") || value.contains("snapshot") {
        return LifecycleState::WatcherUnavailable;
    }
    if value.contains("stale") {
        return LifecycleState::Stale;
    }
    if value.contains("not_started") || value.contains("not_configured") {
        return LifecycleState::NotConfigured;
    }
    LifecycleState::TransportUnavailable
}

pub mod dependency_dag;

pub mod merkle_ledger;

pub mod bm25_index;

pub mod entry_points;

pub mod freshness_receipt;

pub mod delta_store;
pub mod static_provider;

pub mod contract_registry;
pub mod conventions;
pub mod evidence_authority;
pub mod liveness;

pub mod architecture_model;
pub mod framework_intelligence;
mod git_command;
pub mod git_source_observation;

pub mod bootstrap;
pub mod confidence_tiers;
pub mod store_delta;

pub mod lib_redaction;
pub mod module_resolution;
pub mod recall_circuit;

pub mod lib_operations_doctor;
pub mod lib_operations_repair;
pub mod lib_operations_support_bundle;
pub mod lib_rules_evaluate;
pub mod lib_rules_parser;

pub mod analytics;
pub mod change_impact;
pub mod lib_application_document_truth;
pub mod lib_application_errors;
pub mod lib_application_federate;
pub mod lib_application_snapshots;
pub mod lib_findings_specifier;
pub mod lib_generated_docs;
pub mod orientation_projection;
pub mod process_projection;
pub mod reanchor;
pub mod signature_projection;
pub mod test_recommendation;

pub mod providers;

pub mod architecture_views;
pub mod freshness_observation;
pub mod lib_cli_languages;
pub mod lib_cli_rules;
mod qa_env;
