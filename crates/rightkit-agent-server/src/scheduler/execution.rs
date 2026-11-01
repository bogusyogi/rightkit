use crate::api::{lock_recover, DaemonState};
use rightkit_agent_engine_protocol::{ScheduleRunEvent, TranscriptMessage};
use rightkit_agent_engine_runtime_middleware::{MiddlewarePhase, RuntimeSpan};
use rightkit_agent_recipes::{render_recipe, RecipeRun, RecipeRunStatus};
use rightkit_workflow::trigger::CausalReceipt;
use rightkit_workflow::verifier_binding::{
    classify_verdict, gate_terminal_status, verifier_prompt, VerifierVerdict,
};
use rightkit_workflow::waits::TaskIsolationV1;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tracing::warn;

use super::delivery::{deliver_schedule_result, normalize_lexical};
use super::model::{
    ScheduleEntry, ScheduleFireOutcome, ScheduleRuntimeMetadata, ScriptOnlyExecutionAudit,
};
use super::persistence::{
    append_run_event, causal_message_with_isolation, child_task_receipt, load_runtime_metadata,
    load_schedules, release_queued_occurrence, result_receipt, run_receipt, trigger_receipt,
};
use super::time::{current_epoch_millis, current_epoch_minute};

/// Execute one bot request with profile metadata supplied by its versioned
/// workflow definition. No schedule row is required for manual bot runs.
pub(crate) async fn fire_bot_once(
    state: &DaemonState,
    root: &Path,
    entry: &ScheduleEntry,
    profile: crate::api_schedules::BotProfileV1,
) {
    let session_id = format!("bot-{}-{}", sanitize_id(&entry.id), current_epoch_millis());
    let start_span_id = record_scheduled_job_span(
        state,
        MiddlewarePhase::ScheduledJobStart,
        "started",
        &session_id,
        None,
        root,
        entry,
        None,
        serde_json::json!({"botId": profile.id}),
    );
    let metadata = ScheduleRuntimeMetadata {
        bot_id: Some(profile.id.clone()),
        profile: Some(profile),
        trigger: None,
        lifecycle: crate::api_schedules::ScheduleLifecycleState::conservative(),
    };
    let outcome = fire_schedule_inner(state, root, entry, &session_id, metadata).await;
    record_scheduled_job_span(
        state,
        MiddlewarePhase::ScheduledJobEnd,
        &outcome.status,
        &session_id,
        start_span_id.as_deref(),
        root,
        entry,
        Some(&outcome.message),
        serde_json::json!({"botRun": true}),
    );
}

include!("execution/agent.rs");
include!("execution/script.rs");
include!("execution/manual_and_events.rs");
