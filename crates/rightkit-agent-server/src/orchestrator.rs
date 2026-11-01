//! The daemon-side orchestration entry point — turns a goal into a running
//! multi-agent conductor run.
//!
//! This is where the pure pieces meet the real engine:
//! - [`plan_goal`] asks the conductor's model to decompose the goal into a task
//!   graph (structured JSON), parsed by [`parse_plan`].
//! - [`SpawnWorker`] is the real [`Worker`] — it runs each task by spawning a
//!   `SessionEngine` agent through [`DaemonState`]'s existing spawn path (which
//!   already applies the agent's persona + tool ceiling and streams sub-agent
//!   events to the conductor's conduit).
//! - [`DaemonState::run_conductor`] ties planning → `run_graph` → completion,
//!   streaming `TaskGraphUpdated` as the graph advances.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rightkit_agent_engine_core::llm::LlmClient;
use rightkit_agent_orchestration::catalog::{build_catalog, roster_entries};
use rightkit_agent_orchestration::conductor_day_from_iso;
use rightkit_agent_orchestration::evolution::DurationAwareRouter;
use rightkit_agent_orchestration::orchestrator::RunOutcome;
use rightkit_agent_orchestration::registry::AgentRegistry;
use rightkit_agent_orchestration::router::AgentRouter;
use rightkit_agent_orchestration::trigger::{parse_announced_path, Path as RoutePath, TRIGGER_RULE};
use rightkit_agent_orchestration::{
    run_graph_resumable_with_conductor_and_handoff_archive, EvolutionHarness, FailureKind,
    GraphCheckpointStore, HandoffArchiveConfig, JurorVerdict, JuryDecision, JurySystem, Worker,
    WorkerOutcome, WorkerResult, WorkerTask,
};
use rightkit_agent_engine_protocol::ArtifactRow as ProtocolArtifact;
use rightkit_agent_engine_protocol::{ProtocolEvent, TaskNode, TaskState, TranscriptMessage};
use rightkit_agent_engine_tasks::TaskGraph;

use crate::api::{
    load_workspace_config_with_env, lock_recover, now_iso, now_millis, push_session_artifact,
    update_session_model, DaemonState, LiveSession,
};
use crate::provider_client::ProviderLlmClient;
use rightkit_llm::agent_providers::client::capacity::measure_o200k_base as token_count;

include!("orchestrator/worker.rs");
include!("orchestrator/planning.rs");
include!("orchestrator/router.rs");
include!("orchestrator/mechanism_host.rs");
include!("orchestrator/run_conductor.rs");
include!("orchestrator/run_conductor/core_helpers.rs");
include!("orchestrator/run_conductor/finalize_goal.rs");
#[cfg(test)]
include!("orchestrator/run_conductor/finalize_goal_tests.rs");
include!("orchestrator/run_conductor/victory_gate.rs");
include!("orchestrator/run_conductor/routing_direct.rs");
include!("orchestrator/run_conductor/plan_jury.rs");
include!("orchestrator/run_conductor/benchmark_promotion.rs");
include!("orchestrator/helpers.rs");
