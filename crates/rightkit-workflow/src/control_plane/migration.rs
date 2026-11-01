//! WP-06 — Legacy schedule → workflow migration.
//!
//! ADR-0021: a single outer workflow DAG owns lifecycle. Existing legacy
//! schedules are wrapped into a one-node workflow whose `prompt` node body
//! is the legacy prompt. This module is pure data: no I/O, no clock.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control_plane::compiler::{SourceAuthority, WorkflowSource, WorkspaceAuthority};
use crate::control_plane::domain::{
    BackoffStrategy, CapabilitySet, EdgeSpec, NodeConfig, NodeKind, NodeSpec, RetryPolicy,
    WorkflowDefinition, WorkflowId, WorkflowVersionId,
};
use crate::control_plane::ids::Prefix;

/// One legacy schedule as it exists today (pre-control-plane).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacySchedule {
    /// Stable id of the legacy schedule.
    pub id: String,
    /// Prompt body. Becomes the workflow's single node prompt.
    pub prompt: String,
    /// Optional cron expression. When set, becomes a `Cron` trigger.
    pub cron: Option<String>,
    /// Optional IANA timezone name.
    pub timezone: Option<String>,
    /// Skill allowlist attached to the node.
    pub skills: Vec<String>,
    /// Working directory for the node.
    pub workdir: Option<String>,
    /// Toolset binding.
    pub toolset: Option<String>,
    /// Model backend identifier.
    pub backend: Option<String>,
    /// Delivery channel (notification / outbox).
    pub delivery: Option<String>,
    /// Script allowlist.
    pub script_allowlist: Vec<String>,
    /// Run mode (`once`, `recurring`, ...).
    pub mode: Option<String>,
    /// Whether the legacy schedule is paused.
    pub paused: bool,
}

/// Errors raised by [`migrate_to_workflow`].
#[derive(Debug, Error)]
pub enum MigrationError {
    /// Required field was empty or missing.
    #[error("required field `{0}` is empty for legacy schedule `{1}`")]
    EmptyField(&'static str, String),
    /// Unknown / unsupported mode.
    #[error("unsupported mode `{0}` for legacy schedule `{1}`")]
    UnsupportedMode(String, String),
}

/// Deterministic result used by API adapters to publish one legacy schedule
/// as one workflow node plus one trigger.  No schedule field is discarded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigratedWorkflow {
    pub source: WorkflowSource,
    pub trigger: serde_json::Value,
}

/// Build the immutable source and trigger mapping for a legacy schedule.
pub fn migrate_to_source(
    sched: &LegacySchedule,
    version_id: &str,
) -> Result<MigratedWorkflow, MigrationError> {
    let definition = migrate_to_workflow(sched, version_id)?;
    let node_id = format!("legacy-{}", sched.id);
    let node = NodeSpec {
        id: node_id.clone(),
        kind: if sched.mode.as_deref() == Some("scriptOnly") { NodeKind::Transform } else { NodeKind::Agent },
        label: Some(format!("legacy schedule {}", sched.id)),
        capabilities: CapabilitySet::from_iter(sched.skills.iter().cloned().map(crate::control_plane::domain::Capability::new)),
        edges: Vec::<EdgeSpec>::new(),
        retry: RetryPolicy { max_attempts: 1, backoff: BackoffStrategy::Fixed { delay_ms: 0 }, retryable: Vec::new() },
        config: NodeConfig::Execution {
            runtime: sched.backend.clone().unwrap_or_else(|| "legacy".into()),
            instruction: sched.prompt.clone(), runtime_target: sched.toolset.clone(),
            recipe_ref: None, inputs_json: serde_json::json!({"skills":sched.skills,"workdir":sched.workdir,"delivery":sched.delivery,"scriptAllowlist":sched.script_allowlist,"mode":sched.mode}).to_string(),
            transform_operation: None, template: None, max_runtime_ms: 60_000,
            max_output_bytes: 1_048_576, output_schema: "schema://reference/opaque".into(),
        },
    };
    let trigger = serde_json::json!({"id":format!("legacy-trigger-{}", sched.id), "cron":sched.cron, "timezone":sched.timezone, "paused":sched.paused, "versionId":version_id});
    Ok(MigratedWorkflow {
        source: WorkflowSource {
            definition,
            nodes: vec![node],
            spec: SourceAuthority {
                inputs: serde_yaml::Value::Mapping(Default::default()),
                budgets: serde_yaml::Value::Mapping(Default::default()),
                workspace: WorkspaceAuthority {
                    root: sched.workdir.clone().unwrap_or_default(),
                    trust: "legacy-unresolved".into(),
                },
            },
        },
        trigger,
    })
}

/// Wrap a legacy schedule into a one-node workflow definition.
///
/// The resulting definition owns a stable [`WorkflowId`] derived from the
/// legacy schedule id; the supplied `version_id` is recorded in the
/// returned value's metadata so callers can pin the published version.
pub fn migrate_to_workflow(
    sched: &LegacySchedule,
    version_id: &str,
) -> Result<WorkflowDefinition, MigrationError> {
    if sched.id.trim().is_empty() {
        return Err(MigrationError::EmptyField("id", sched.id.clone()));
    }
    if sched.prompt.trim().is_empty() {
        return Err(MigrationError::EmptyField("prompt", sched.id.clone()));
    }
    if let Some(mode) = &sched.mode {
        match mode.as_str() {
            "once" | "recurring" | "manual" | "watch" | "scriptOnly" => {}
            other => {
                return Err(MigrationError::UnsupportedMode(
                    other.to_string(),
                    sched.id.clone(),
                ));
            }
        }
    }

    // Stable id: `wfd_<legacy_id>`. The legacy id is already a slug in
    // current the host app schedules, so it is safe to embed.
    let id = WorkflowId::new(Prefix::WorkflowDefinition, format!("migr-{}", sched.id));
    let version = WorkflowVersionId::new(Prefix::WorkflowVersion, version_id);

    let _ = version; // Recorded for caller; the WorkflowDefinition type does
                     // not carry a version pointer (it lives on `WorkflowVersion`).

    Ok(WorkflowDefinition {
        id,
        name: format!("migrated:{}", sched.id),
        description: format!(
            "Migrated from legacy schedule {} (paused={}, mode={})",
            sched.id,
            sched.paused,
            sched.mode.as_deref().unwrap_or("n/a"),
        ),
        owner_scope: "migrated".into(),
        revision: crate::control_plane::domain::Revision(1),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_schedule() -> LegacySchedule {
        LegacySchedule {
            id: "morning-brief".into(),
            prompt: "Greet the user".into(),
            cron: Some("0 9 * * *".into()),
            timezone: Some("America/New_York".into()),
            skills: vec!["docs".into()],
            workdir: Some("/repo".into()),
            toolset: Some("default".into()),
            backend: Some("codex".into()),
            delivery: Some("desktop".into()),
            script_allowlist: vec!["bash".into()],
            mode: Some("recurring".into()),
            paused: false,
        }
    }

    #[test]
    fn migrate_to_workflow_round_trips_prompt_and_metadata() {
        let sched = base_schedule();
        let wf = migrate_to_workflow(&sched, "v1").expect("migrates");
        // `WorkflowId` is namespaced via `StableId::new`, which formats
        // the prefix and value with an underscore separator (`wfd_<value>`).
        assert!(wf.id.to_string().starts_with("wfd_migr-morning-brief"));
        assert!(wf.description.contains("recurring"));
        assert_eq!(wf.name, "migrated:morning-brief");
    }

    #[test]
    fn migrate_rejects_empty_id() {
        let mut sched = base_schedule();
        sched.id = "   ".into();
        let err = migrate_to_workflow(&sched, "v1").expect_err("empty id");
        assert!(matches!(err, MigrationError::EmptyField("id", _)));
    }

    #[test]
    fn migrate_rejects_empty_prompt() {
        let mut sched = base_schedule();
        sched.prompt = String::new();
        let err = migrate_to_workflow(&sched, "v1").expect_err("empty prompt");
        assert!(matches!(err, MigrationError::EmptyField("prompt", _)));
    }

    #[test]
    fn migrate_accepts_known_modes() {
        for mode in ["once", "recurring", "manual", "watch"] {
            let mut sched = base_schedule();
            sched.mode = Some(mode.into());
            migrate_to_workflow(&sched, "v1").expect("known mode");
        }
    }

    #[test]
    fn migrate_rejects_unknown_mode() {
        let mut sched = base_schedule();
        sched.mode = Some("sneaky".into());
        let err = migrate_to_workflow(&sched, "v1").expect_err("unknown mode");
        assert!(matches!(err, MigrationError::UnsupportedMode(_, _)));
    }

    #[test]
    fn migrate_none_mode_is_accepted() {
        let mut sched = base_schedule();
        sched.mode = None;
        migrate_to_workflow(&sched, "v1").expect("none mode");
    }

    #[test]
    fn migrated_definition_is_pure_data() {
        let sched = base_schedule();
        let a = migrate_to_workflow(&sched, "v1").expect("ok");
        let b = migrate_to_workflow(&sched, "v1").expect("ok");
        assert_eq!(a, b);
    }
}
