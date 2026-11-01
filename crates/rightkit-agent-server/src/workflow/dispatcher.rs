//! Workflow dispatcher for the autonomous workflow control plane.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use rightkit_agent_engine_core::conduit::ConduitReceive;
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

use rightkit_agent_engine_storage::{
    JournalAppend, ProjectionUpdate, WorkflowCommandRow, WorkflowJournalWriter, WorkflowQueueRow,
    WorkflowRepository, WorkflowRunRow,
};
use rightkit_workflow::control_plane::a2a::workspace_contains;
use rightkit_workflow::control_plane::context::ContextManifest;
use rightkit_workflow::control_plane::domain::{
    run_id, workflow_id, workflow_version_id, ContentHash, FailureClass, LifecyclePhase,
    LifecycleState, Revision,
};
use rightkit_workflow::control_plane::executors::{
    ExecutorAdapter, ExecutorError, NodeOutput as AdapterNodeOutput,
};
use rightkit_workflow::control_plane::prelude::{CompiledPlan, NodeConfig, NodeKind, NodeSpec};
use rightkit_workflow::control_plane::reducer::Command;

use crate::workflow::executors::{A2aAdapter, A2aAttempt};

/// Result of executing one node. Carries the node's typed output plus the
/// accounting fields the daemon needs to advance lifecycle / budget tracking.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NodeOutput {
    /// Structured, schema-bound node result. Serialized into the
    /// `workflow_node_states` projection by the executor that returns it.
    pub output_json: serde_json::Value,
    /// Artifact ids (relative to the workspace) the node produced.
    pub artifacts: Vec<String>,
    /// Cost in micros of a US dollar. Mirrors the
    /// `coderight_budget::BudgetLedger::record_cost` shape so the same units
    /// roll into the workflow budget.
    pub cost_usd_micros: u64,
    /// Total tokens consumed across all provider calls in this node.
    pub tokens_used: u64,
}

/// Typed failure surfaced by runtime dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DispatcherError {
    #[error("node executor unsupported: {0}")]
    NodeUnsupported(String),
    #[error("runtime dispatch failed: {0}")]
    Runtime(String),
    #[error("node execution failed ({class:?}, retryable={retryable}): {message}")]
    ExecutionFailure {
        class: FailureClass,
        retryable: bool,
        message: String,
    },
    #[error("durable wait: {0}")]
    Waiting(String),
    #[error("approval required: {0}")]
    ApprovalRequired(String),
    #[error("effect intent required: {0}")]
    EffectIntent(String),
}

/// Production dispatcher. It uses the daemon's governed session path rather
/// than constructing a SessionEngine directly, preserving policy, telemetry,
/// driver-host, & cancellation semantics.
#[derive(Clone)]
pub struct RuntimeNodeDispatcher {
    state: Arc<crate::api::DaemonState>,
    timeout: std::time::Duration,
    a2a: Option<Arc<A2aAdapter>>,
}

struct SessionInterruptGuard {
    live: Arc<crate::api::LiveSession>,
    completed: bool,
}

impl Drop for SessionInterruptGuard {
    fn drop(&mut self) {
        if !self.completed {
            if let Some(tx) = &self.live.interrupt_tx {
                let _ = tx.send(true);
            }
        }
    }
}

impl RuntimeNodeDispatcher {
    pub fn new(state: Arc<crate::api::DaemonState>) -> Self {
        Self::new_with_a2a_adapter(state, None)
    }

    /// Construct the production dispatcher with a daemon-startup injected
    /// A2A adapter. `None` deliberately keeps A2A closed when no transport and
    /// trust verifier were configured.
    pub fn new_with_a2a_adapter(
        state: Arc<crate::api::DaemonState>,
        a2a: Option<Arc<A2aAdapter>>,
    ) -> Self {
        Self {
            state,
            timeout: std::time::Duration::from_secs(300),
            a2a,
        }
    }

    /// Inject daemon-owned A2A transport after startup has supplied trusted
    /// verifier & transport bindings. `None` keeps remote execution closed.
    pub fn with_a2a_adapter(mut self, a2a: Option<Arc<A2aAdapter>>) -> Self {
        self.a2a = a2a;
        self
    }

    async fn execute_a2a(
        &self,
        spec: &NodeSpec,
        ctx: &NodeDispatchContext,
    ) -> Result<NodeOutput, DispatcherError> {
        let NodeConfig::Execution {
            runtime,
            max_runtime_ms,
            max_output_bytes,
            output_schema,
            ..
        } = &spec.config
        else {
            return Err(DispatcherError::ExecutionFailure {
                class: FailureClass::InvalidBinding,
                retryable: false,
                message: "A2A node lacks compiled execution config".into(),
            });
        };
        if runtime != "a2a" {
            return Err(DispatcherError::ExecutionFailure {
                class: FailureClass::InvalidBinding,
                retryable: false,
                message: "A2A dispatcher received a non-A2A runtime".into(),
            });
        }
        let adapter = self.a2a.as_ref().ok_or_else(|| {
            // Missing daemon wiring is an admission failure, not an ordinary
            // runtime error: do not let a compiled A2A node enter execution
            // with an implicit local/fallback executor.
            DispatcherError::ExecutionFailure {
                class: FailureClass::InvalidBinding,
                retryable: false,
                message: "A2A adapter is not configured; refusing remote execution".into(),
            }
        })?;
        if !self.state.has_license_feature("a2a") {
            return Err(DispatcherError::ExecutionFailure {
                class: FailureClass::PolicyDenied,
                retryable: false,
                message: "A2A execution requires an active a2a license feature".into(),
            });
        }
        let target_matches = matches!(
            &spec.config,
            NodeConfig::Execution {
                runtime_target: Some(target),
                ..
            } if target.as_str() == adapter.executor_id()
        );
        if !target_matches {
            return Err(DispatcherError::ExecutionFailure {
                class: FailureClass::InvalidBinding,
                retryable: false,
                message: "A2A node target does not match injected executor".into(),
            });
        }
        if !workspace_contains(&ctx.workspace_root, adapter.workspace_root()) {
            return Err(DispatcherError::ExecutionFailure {
                class: FailureClass::InvalidBinding,
                retryable: false,
                message: "A2A descriptor workspace escapes workflow workspace".into(),
            });
        }
        adapter
            .validate_workflow_admission(spec, &ctx.manifest, &ctx.workspace_root)
            .map_err(|error| map_a2a_executor_error(error, "A2A admission"))?;
        let timeout = execution_timeout(ctx.manifest.budget(), *max_runtime_ms)?;
        let adapter = Arc::clone(adapter);
        let cancellation_adapter = adapter.clone();
        let spec = spec.clone();
        let manifest = ctx.manifest.clone();
        let attempt = A2aAttempt::from_manifest(&manifest);
        let execution_attempt = attempt.clone();
        let execution = tokio::task::spawn_blocking(move || {
            adapter
                .check_capability(&spec, &manifest)
                .map_err(|error| map_a2a_executor_error(error, "A2A capability"))?;
            adapter
                .execute_attempt(&spec, &manifest, &execution_attempt)
                .map_err(|error| map_a2a_executor_error(error, "A2A execution"))
        });
        let adapter_output = match tokio::time::timeout(timeout, execution).await {
            Ok(result) => result.map_err(|error| DispatcherError::ExecutionFailure {
                class: FailureClass::ExecutorUnavailable,
                retryable: true,
                message: format!("A2A execution task: {error}"),
            })??,
            Err(_) => {
                let cancellation = cancellation_adapter
                    .cancel(&attempt)
                    .map_err(|error| error.to_string());
                return Err(DispatcherError::ExecutionFailure {
                    class: FailureClass::DeadlineExceeded,
                    retryable: false,
                    message: match cancellation {
                        Ok(()) => "A2A node deadline exceeded; cancellation requested".into(),
                        Err(error) => {
                            format!("A2A node deadline exceeded; cancellation failed: {error}")
                        }
                    },
                });
            }
        };
        match adapter_output {
            AdapterNodeOutput::Success {
                output_json,
                artifacts,
                cost_usd_micros,
                tokens_used,
            } => {
                let mut output = validate_output(output_json, *max_output_bytes, output_schema)
                    .map_err(|error| DispatcherError::ExecutionFailure {
                        class: FailureClass::InvalidOutput,
                        retryable: false,
                        message: format!("A2A output validation failed: {error}"),
                    })?;
                validate_a2a_artifacts(&artifacts).map_err(|error| {
                    DispatcherError::ExecutionFailure {
                        class: FailureClass::InvalidOutput,
                        retryable: false,
                        message: format!("A2A output validation failed: {error}"),
                    }
                })?;
                output.artifacts = artifacts;
                output.cost_usd_micros = cost_usd_micros;
                output.tokens_used = tokens_used;
                Ok(output)
            }
            AdapterNodeOutput::Failure {
                class,
                message,
                retryable,
            } => Err(DispatcherError::ExecutionFailure {
                class,
                retryable,
                message,
            }),
            AdapterNodeOutput::Timeout { elapsed_ms, .. } => {
                Err(DispatcherError::ExecutionFailure {
                    class: FailureClass::DeadlineExceeded,
                    retryable: false,
                    message: format!("A2A node timed out after {elapsed_ms} ms"),
                })
            }
            AdapterNodeOutput::Cancelled { reason } => Err(DispatcherError::ExecutionFailure {
                class: FailureClass::Cancelled,
                retryable: false,
                message: format!("A2A node cancelled: {reason}"),
            }),
            AdapterNodeOutput::Pending { note, .. } => Err(DispatcherError::Waiting(format!(
                "A2A node pending: {note}"
            ))),
        }
    }

    async fn execute_governed(
        &self,
        spec: &NodeSpec,
        ctx: &NodeDispatchContext,
    ) -> Result<NodeOutput, DispatcherError> {
        let NodeConfig::Execution {
            runtime,
            instruction,
            runtime_target,
            recipe_ref,
            inputs_json,
            max_runtime_ms,
            max_output_bytes,
            output_schema,
            ..
        } = &spec.config
        else {
            return Err(DispatcherError::Runtime(
                "execution node lacks compiled config".into(),
            ));
        };
        if spec.kind == NodeKind::Recipe && runtime != "recipe" {
            return Err(DispatcherError::Runtime(
                "recipe nodes require the recipe runtime".into(),
            ));
        }
        let timeout = execution_timeout(ctx.manifest.budget(), *max_runtime_ms)?;
        if spec.kind == NodeKind::Subworkflow {
            let pin = runtime_target
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    DispatcherError::Runtime("subworkflow requires a published version pin".into())
                })?;
            return self
                .execute_subworkflow(
                    pin,
                    inputs_json,
                    ctx,
                    timeout,
                    *max_output_bytes,
                    output_schema,
                )
                .await;
        }
        let session_id = format!(
            "workflow-{}-{}",
            spec.id,
            rightkit_agent_engine_protocol::uuid_v7()
        );
        let workspace = ctx.workspace_root.as_str();
        let (live, prompt) =
            if runtime == "recipe" || spec.kind == NodeKind::Recipe {
                let id = recipe_ref
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| DispatcherError::Runtime("recipe_ref is required".into()))?;
                let installed = {
                    let store =
                        self.state.recipes.lock().map_err(|_| {
                            DispatcherError::Runtime("recipe store unavailable".into())
                        })?;
                    store
                        .get(id)
                        .or_else(|| {
                            store
                                .list()
                                .into_iter()
                                .find(|item| item.source_uri.as_deref() == Some(id))
                        })
                        .ok_or_else(|| {
                            DispatcherError::Runtime(format!("installed recipe `{id}` not found"))
                        })?
                };
                if !installed.trusted
                    || !installed.latest_validation.as_ref().is_some_and(|run| {
                        run.verdict != rightkit_agent_recipes::RecipeVerdict::Block
                    })
                {
                    return Err(DispatcherError::Runtime(format!(
                        "recipe `{id}` is not trusted and validated"
                    )));
                }
                let params = serde_json::from_str::<BTreeMap<String, String>>(inputs_json)
                    .map_err(|error| DispatcherError::Runtime(format!("recipe inputs: {error}")))?;
                let rendered = rightkit_agent_recipes::render_recipe(&installed.recipe, &params)
                    .map_err(|error| {
                        DispatcherError::Runtime(format!("recipe render: {}", error.message))
                    })?;
                let live = self
                    .state
                    .create_recipe_session_with_toolset_policy(
                        &session_id,
                        workspace,
                        &rendered.title,
                        rendered.settings.toolset.as_deref(),
                        rendered.skills,
                        rendered.settings.execution_backend,
                    )
                    .map_err(|_| {
                        DispatcherError::Runtime("recipe runtime policy rejected session".into())
                    })?;
                let recipe_run = rightkit_agent_recipes::RecipeRun {
                    id: format!("rr-{}", rightkit_agent_engine_protocol::uuid_v7()),
                    recipe_id: installed.id,
                    session_id: session_id.clone(),
                    status: rightkit_agent_recipes::RecipeRunStatus::Running,
                    params: rendered.params.clone(),
                    rendered_prompt_hash: rendered.rendered_prompt_hash.clone(),
                    result_artifact_id: None,
                    created_at: crate::api::now_iso(),
                    completed_at: None,
                };
                let snapshot = {
                    let mut store =
                        self.state.recipes.lock().map_err(|_| {
                            DispatcherError::Runtime("recipe store unavailable".into())
                        })?;
                    store.add_run(recipe_run);
                    store.snapshot()
                };
                crate::api_recipes::persist_recipe_store_snapshot(&self.state, &snapshot)
                    .map_err(|error| DispatcherError::Runtime(format!("recipe run: {error}")))?;
                (live, rendered.rendered_prompt)
            } else {
                if !matches!(
                    runtime.as_str(),
                    "native" | "codex" | "claude" | "conductor"
                ) {
                    return Err(DispatcherError::Runtime(format!(
                        "runtime `{runtime}` cannot execute an agent node"
                    )));
                }
                if instruction.trim().is_empty() {
                    return Err(DispatcherError::Runtime(
                        "execution instruction is empty".into(),
                    ));
                }
                let options = crate::api::SessionRuntimeOptions {
                    driver_kind: runtime.clone(),
                    thinking_budget_tokens: ctx
                        .manifest
                        .budget()
                        .max_tokens
                        .map(|value| value.min(u32::MAX as u64) as u32),
                    max_iterations: ctx
                        .manifest
                        .budget()
                        .max_inner_retries
                        .map(|value| value.saturating_add(1)),
                    mcp_channel: "workflow".into(),
                    spec_id: Some(ctx.version_id.clone()),
                    budget_ceiling_usd: ctx
                        .manifest
                        .budget()
                        .max_cost_usd_micros
                        .map(|value| value as f64 / 1_000_000.0),
                    ..Default::default()
                };
                let live = self.state.create_session_with_options(
                    &session_id,
                    runtime_target.as_deref(),
                    workspace,
                    spec.label.as_deref().unwrap_or("Workflow node"),
                    None,
                    options,
                );
                let prompt = if inputs_json.trim() == "{}" {
                    instruction.clone()
                } else {
                    format!("{instruction}\n\nInputs:\n{inputs_json}")
                };
                (live, prompt)
            };
        let mut guard = SessionInterruptGuard {
            live: live.clone(),
            completed: false,
        };
        if runtime == "conductor" {
            let outcome = tokio::time::timeout(
                timeout,
                self.state.run_conductor(&session_id, &prompt, false),
            )
            .await
            .map_err(|_| DispatcherError::Runtime("conductor timed out".into()))?
            .map_err(DispatcherError::Runtime)?;
            guard.completed = true;
            let output = serde_json::json!({"completed":outcome.completed,"failed":outcome.failed,"results":outcome.results.into_iter().map(|result| serde_json::json!({"agent":result.agent,"success":result.success,"output":result.output})).collect::<Vec<_>>()});
            return validate_output(output, *max_output_bytes, output_schema);
        }
        let mut events = live.conduit.subscribe();
        self.state
            .submit_text_with_trust(&session_id, &prompt, ctx.prompt_trust.clone(), "workflow")
            .await
            .map_err(DispatcherError::Runtime)?;
        let result = tokio::time::timeout(self.timeout.min(timeout), async {
            loop {
                match events.recv().await {
                    ConduitReceive::Event(
                        rightkit_agent_engine_protocol::ProtocolEvent::AssistantMessageCompleted {
                            message,
                            ..
                        },
                    ) => {
                        let content = match message {
                            rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                                content, ..
                            } => content,
                            _ => String::new(),
                        };
                        return Ok(content);
                    }
                    ConduitReceive::Event(rightkit_agent_engine_protocol::ProtocolEvent::RunFailed {
                        error,
                        ..
                    }) => {
                        return Err(error);
                    }
                    ConduitReceive::Event(
                        rightkit_agent_engine_protocol::ProtocolEvent::RunInterrupted { .. },
                    ) => {
                        return Err("workflow session interrupted".into());
                    }
                    ConduitReceive::Lagged { missed } => {
                        return Err(format!(
                            "workflow session conduit lagged ({missed} events missed); resync required"
                        ));
                    }
                    ConduitReceive::Closed => {
                        return Err("workflow session conduit closed".into());
                    }
                    ConduitReceive::Event(_) => {}
                }
            }
        })
        .await
        .map_err(|_| DispatcherError::Runtime("workflow session timed out".into()))?
        .map_err(DispatcherError::Runtime)?;
        guard.completed = true;
        let output = if output_schema == "schema://reference/opaque" {
            serde_json::json!({"text": result, "sessionId": session_id})
        } else {
            serde_json::from_str(&result).map_err(|error| {
                DispatcherError::Runtime(format!("schema-bound output is not JSON: {error}"))
            })?
        };
        validate_output(output, *max_output_bytes, output_schema)
    }

    async fn execute_subworkflow(
        &self,
        pin: &str,
        input: &str,
        ctx: &NodeDispatchContext,
        timeout: std::time::Duration,
        max_output_bytes: u64,
        output_schema: &str,
    ) -> Result<NodeOutput, DispatcherError> {
        serde_json::from_str::<Value>(input)
            .map_err(|error| DispatcherError::Runtime(format!("subworkflow inputs: {error}")))?;
        let wf =
            self.state.workflow_api_state.as_ref().ok_or_else(|| {
                DispatcherError::Runtime("workflow coordinator unavailable".into())
            })?;
        let (workflow_pin, version_pin) = pin.split_once('@').ok_or_else(|| {
            DispatcherError::Runtime("subworkflow pin must be workflow@version".into())
        })?;
        let child_id = {
            let storage = wf.storage.lock().await;
            let repo = WorkflowRepository::new(storage.conn());
            let version = repo
                .find_version(version_pin)
                .map_err(|error| DispatcherError::Runtime(error.to_string()))?
                .filter(|version| {
                    version.workflow_id == workflow_pin && version.publication_state == "published"
                })
                .ok_or_else(|| {
                    DispatcherError::Runtime(format!(
                        "subworkflow version `{pin}` is not published"
                    ))
                })?;
            let root = serde_json::from_str::<Value>(&version.compiled_plan_json)
                .ok()
                .and_then(|plan| {
                    plan.get("entry_node")
                        .or_else(|| plan.get("entryNode"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .ok_or_else(|| {
                    DispatcherError::Runtime("subworkflow compiled plan lacks root".into())
                })?;
            let binding = format!(
                "{}:{}:{}",
                ctx.manifest.provenance().run_id,
                ctx.manifest.provenance().node_id,
                version.id
            );
            let digest = ContentHash::sha256(binding.as_bytes());
            let suffix = &digest.as_str()["sha256:".len()..];
            let child_id = format!("wfr_sub_{suffix}");
            let command_id = format!("wfc_sub_{suffix}");
            let now = crate::api::now_iso();
            let snapshot = LifecycleState {
                workflow_id: workflow_id(version.workflow_id.clone()),
                version_id: workflow_version_id(version.id.clone()),
                revision: Revision(0),
                phase: LifecyclePhase::Pending,
            };
            let reducer_command = Command {
                command_id: command_id.clone(),
                kind: "start".into(),
                run_id: run_id(&child_id),
                expected_revision: Revision(0),
                payload_json: input.into(),
            };
            let run = WorkflowRunRow {
                id: child_id.clone(),
                workflow_id: version.workflow_id,
                workflow_version_id: version.id,
                parent_run_id: Some(ctx.manifest.provenance().run_id.to_string()),
                lifecycle_phase: "pending".into(),
                lifecycle_revision: 0,
                initiated_by_kind: "subworkflow".into(),
                initiated_by_ref: Some(ctx.manifest.provenance().node_id.to_string()),
                trigger_payload_json: None,
                input_json: input.into(),
                started_at: now.clone(),
                terminal_at: None,
                terminal_failure_class: None,
                runs_root_durable: true,
            };
            let command = WorkflowCommandRow {
                run_id: child_id.clone(),
                command_id,
                expected_revision: 0,
                command_kind: "start".into(),
                payload_json: input.into(),
                created_at: now.clone(),
            };
            let event = JournalAppend {
                run_id: &child_id,
                event_kind: "run_started",
                node_id: None,
                payload_json: input,
                created_at: &now,
            };
            let projection = ProjectionUpdate {
                run_id: &child_id,
                run_phase: Some("running"),
                ..Default::default()
            };
            let queue = WorkflowQueueRow {
                run_id: child_id.clone(),
                node_id: root,
                available_at: now.clone(),
                priority: 0,
                claimed_by: None,
                claim_fence: None,
            };
            WorkflowJournalWriter::new(storage.conn())
                .admit_run_from_snapshot(
                    &snapshot,
                    &reducer_command,
                    &run,
                    &command,
                    "{}",
                    &event,
                    &projection,
                    &queue,
                )
                .map_err(|error| DispatcherError::Runtime(error.to_string()))?;
            child_id
        };
        let terminal = tokio::time::timeout(timeout, async {
            loop {
                let storage = wf.storage.lock().await;
                let row = WorkflowRepository::new(storage.conn())
                    .find_run(&child_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "child run disappeared".to_string())?;
                if matches!(
                    row.lifecycle_phase.as_str(),
                    "succeeded" | "failed" | "cancelled"
                ) {
                    return Ok::<_, String>(row);
                }
                drop(storage);
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        })
        .await
        .map_err(|_| DispatcherError::Runtime("subworkflow timed out".into()))?
        .map_err(DispatcherError::Runtime)?;
        if terminal.lifecycle_phase != "succeeded" {
            return Err(DispatcherError::Runtime(format!(
                "subworkflow ended {}",
                terminal.lifecycle_phase
            )));
        }
        validate_output(
            serde_json::json!({"childRunId":child_id,"status":"succeeded"}),
            max_output_bytes,
            output_schema,
        )
    }
}

fn execution_timeout(
    budget: &rightkit_workflow::control_plane::context::ContextBudget,
    max_runtime_ms: u64,
) -> Result<std::time::Duration, DispatcherError> {
    let mut milliseconds = max_runtime_ms;
    if let Some(deadline) = budget.deadline_unix_ms {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| DispatcherError::Runtime(error.to_string()))?
            .as_millis() as i64;
        let remaining = deadline - now;
        if remaining <= 0 {
            return Err(DispatcherError::Runtime("manifest deadline expired".into()));
        }
        milliseconds = milliseconds.min(remaining as u64);
    }
    Ok(std::time::Duration::from_millis(milliseconds))
}

fn validate_output(
    value: Value,
    max_bytes: u64,
    schema_json: &str,
) -> Result<NodeOutput, DispatcherError> {
    let bytes =
        serde_json::to_vec(&value).map_err(|error| DispatcherError::Runtime(error.to_string()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(DispatcherError::Runtime(
            "node output exceeds compiled byte limit".into(),
        ));
    }
    if schema_json == "schema://reference/opaque" {
        return Ok(NodeOutput {
            output_json: value,
            artifacts: Vec::new(),
            cost_usd_micros: 0,
            tokens_used: 0,
        });
    }
    if schema_json.starts_with("schema://") {
        let known = [
            "github/",
            "seo/",
            "workflow/",
            "effect/",
            "review/",
            "morning-briefing/",
            "pr-triage/",
            "document-onboarding/",
        ]
        .iter()
        .any(|namespace| schema_json.starts_with(&format!("schema://{namespace}")));
        if !known || !value.is_object() {
            return Err(DispatcherError::Runtime(format!(
                "unresolved or non-object output schema `{schema_json}`"
            )));
        }
        return Ok(NodeOutput {
            output_json: value,
            artifacts: Vec::new(),
            cost_usd_micros: 0,
            tokens_used: 0,
        });
    }
    let schema: Value = serde_json::from_str(schema_json)
        .map_err(|error| DispatcherError::Runtime(format!("inline output schema: {error}")))?;
    let compiled = jsonschema::validator_for(&schema)
        .map_err(|error| DispatcherError::Runtime(format!("output schema: {error}")))?;
    // jsonschema 0.50: `iter_errors` replaces the old iterator-returning
    // `validate`, so every violation is still reported, not just the first.
    let errors: Vec<String> = compiled
        .iter_errors(&value)
        .map(|error| error.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(DispatcherError::Runtime(format!(
            "output schema rejected result: {}",
            errors.join("; ")
        )));
    }
    Ok(NodeOutput {
        output_json: value,
        artifacts: Vec::new(),
        cost_usd_micros: 0,
        tokens_used: 0,
    })
}

fn validate_a2a_artifacts(artifacts: &[String]) -> Result<(), String> {
    for artifact in artifacts {
        let normalized = artifact.replace('\\', "/");
        let bytes = normalized.as_bytes();
        let drive_path = bytes.len() >= 2 && bytes[1] == b':';
        if normalized.trim().is_empty()
            || normalized.starts_with('/')
            || drive_path
            || normalized.contains('\0')
            || normalized.split('/').any(|component| component == "..")
        {
            return Err(format!(
                "artifact path is outside workflow workspace: `{artifact}`"
            ));
        }
    }
    Ok(())
}

fn execute_transform(spec: &NodeSpec) -> Result<NodeOutput, DispatcherError> {
    let NodeConfig::Execution {
        transform_operation: Some(operation),
        template,
        inputs_json,
        max_output_bytes,
        output_schema,
        ..
    } = &spec.config
    else {
        return Err(DispatcherError::Runtime(
            "transform lacks authored operation".into(),
        ));
    };
    let input: Value = serde_json::from_str(inputs_json)
        .map_err(|error| DispatcherError::Runtime(format!("transform inputs: {error}")))?;
    let value = match operation.as_str() {
        "identity" | "passthrough" | "read-ledger-window" => input,
        "json-to-markdown" => Value::String(format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(&input)
                .map_err(|error| DispatcherError::Runtime(error.to_string()))?
        )),
        "format" | "format-converter" => Value::String(
            template
                .as_deref()
                .unwrap_or("{{input}}")
                .replace("{{input}}", &input.to_string()),
        ),
        "log-to-memory" => serde_json::json!({"memoryEntry": input}),
        other => {
            return Err(DispatcherError::Runtime(format!(
                "unknown deterministic transform `{other}`"
            )));
        }
    };
    validate_output(value, *max_output_bytes, output_schema)
}

#[async_trait]
impl ExecuteNode for RuntimeNodeDispatcher {
    async fn execute_node(
        &self,
        spec: &NodeSpec,
        ctx: &NodeDispatchContext,
    ) -> Result<NodeOutput, DispatcherError> {
        match spec.kind {
            NodeKind::Agent | NodeKind::Recipe | NodeKind::Subworkflow => {
                if matches!(&spec.config, NodeConfig::Execution { runtime, .. } if runtime == "a2a")
                {
                    self.execute_a2a(spec, ctx).await
                } else {
                    self.execute_governed(spec, ctx).await
                }
            }
            NodeKind::Transform => execute_transform(spec),
            NodeKind::Condition | NodeKind::Join | NodeKind::Map | NodeKind::Loop => {
                match (&spec.kind, &spec.config) {
                    (NodeKind::Condition, NodeConfig::Condition { .. })
                    | (NodeKind::Join, NodeConfig::Join { .. })
                    | (NodeKind::Map, NodeConfig::Map { .. })
                    | (NodeKind::Loop, NodeConfig::Loop { .. }) => {}
                    _ => {
                        return Err(DispatcherError::Runtime(
                            "control node lacks typed configuration".into(),
                        ));
                    }
                }
                let mut inputs = ctx.run_input.as_object().cloned().unwrap_or_default();
                // Materialized iteration children retain their parent control
                // id plus a stable `__iter_N` suffix.  Feed the compiled
                // bounds into the pure planner instead of trusting trigger
                // input to widen cardinality.
                let iteration_index = iteration_index_from_id(&spec.id);
                match &spec.config {
                    NodeConfig::Map { max_items } => {
                        inputs.insert("max_iterations".into(), Value::from(*max_items));
                    }
                    NodeConfig::Loop { max_iterations, .. } => {
                        inputs.insert("max_iterations".into(), Value::from(*max_iterations));
                    }
                    _ => {}
                }
                let eval = rightkit_workflow::control_plane::expression::EvalContext {
                    inputs,
                    node_outputs: ctx.node_outputs.clone().into_iter().collect(),
                    iteration_index: iteration_index
                        .or_else(|| ctx.iteration_index.map(|value| value as usize)),
                    ..Default::default()
                };
                let control =
                    rightkit_workflow::control_plane::control::ControlPlan::plan_for(&spec.kind);
                control
                    .check_entry(&ctx.compiled_plan, &eval)
                    .map_err(|error| DispatcherError::Runtime(format!("control entry: {error}")))?;
                let selected = control
                    .resolve_outgoing(&ctx.compiled_plan, &spec.id, &eval)
                    .map_err(|error| {
                        DispatcherError::Runtime(format!("control resolve: {error}"))
                    })?;
                Ok(NodeOutput {
                    output_json: serde_json::json!({"selected": selected}),
                    artifacts: Vec::new(),
                    cost_usd_micros: 0,
                    tokens_used: 0,
                })
            }
            NodeKind::WaitTimer | NodeKind::WaitEvent => Err(DispatcherError::Waiting(format!(
                "{} is a durable waiting node",
                spec.id
            ))),
            NodeKind::Approval => Err(DispatcherError::ApprovalRequired(format!(
                "{} awaits approval",
                spec.id
            ))),
            NodeKind::Effect => Err(DispatcherError::EffectIntent(format!(
                "{} requires an effect adapter",
                spec.id
            ))),
        }
    }
}

/// Parse the replay-stable suffix emitted by `ControlPlan`.
fn iteration_index_from_id(id: &str) -> Option<usize> {
    id.rsplit_once("__iter_")
        .and_then(|(_, suffix)| suffix.parse::<usize>().ok())
}

fn map_a2a_executor_error(error: ExecutorError, context: &str) -> DispatcherError {
    let class = match &error {
        ExecutorError::UnsupportedKind(_) | ExecutorError::InvalidManifest { .. } => {
            FailureClass::InvalidBinding
        }
        ExecutorError::CapabilityDenied { .. } => FailureClass::PolicyDenied,
        ExecutorError::Internal { .. } => FailureClass::ExecutorUnavailable,
    };
    let retryable = matches!(class, FailureClass::ExecutorUnavailable);
    DispatcherError::ExecutionFailure {
        class,
        retryable,
        message: format!("{context}: {error}"),
    }
}

/// Per-node dispatch context. The executor receives the run's resolved input,
/// any prior node outputs (keyed by node id), and the current iteration index
/// for `Map` / `Loop` nodes.
#[derive(Debug, Clone)]
pub struct NodeDispatchContext {
    /// Durable, validated workflow input; never used to select authority.
    pub run_input: serde_json::Value,
    /// Only selected upstream outputs, keyed by node id.
    pub node_outputs: HashMap<String, serde_json::Value>,
    /// Definition/version-authoritative workspace selected at admission.
    pub workspace_root: String,
    /// Source trust survives trigger admission. Untrusted trigger text is
    /// never upgraded to `WorkspaceTrusted` merely because it runs here.
    pub prompt_trust: rightkit_agent_engine_protocol::ContentTrust,
    /// Immutable, hash-bound context given to this attempt.
    pub manifest: ContextManifest,
    /// Compiled version plan; control nodes use it only via pure resolver.
    pub compiled_plan: CompiledPlan,
    /// Exact immutable workflow version governing this attempt.
    pub version_id: String,
    pub iteration_index: Option<u64>,
}

impl NodeDispatchContext {
    /// Test-only empty context. Production construction happens in
    /// `NodeExecutor::build_dispatch_context` after durable authority lookup.
    pub fn for_test() -> Self {
        use rightkit_workflow::control_plane::context::{ContextBudget, ContextBuilder};
        use rightkit_workflow::control_plane::domain::{NodeId, RunId};
        use rightkit_workflow::control_plane::ids::Prefix;

        let manifest = ContextBuilder::new("v1")
            .with_provenance(
                RunId::new(Prefix::WorkflowRun, "test"),
                NodeId::new(Prefix::NodeRun, "test"),
                0,
            )
            .with_budget(ContextBudget::unbounded())
            .build()
            .expect("test context manifest");
        Self {
            run_input: Value::Object(Default::default()),
            node_outputs: HashMap::new(),
            workspace_root: String::new(),
            prompt_trust: rightkit_agent_engine_protocol::ContentTrust::User,
            manifest,
            compiled_plan: CompiledPlan {
                entry_node: "test".into(),
                nodes: Vec::new(),
                effective_capabilities:
                    rightkit_workflow::control_plane::domain::CapabilitySet::empty(),
            },
            version_id: "wfv_test".into(),
            iteration_index: None,
        }
    }
}

impl Default for NodeDispatchContext {
    fn default() -> Self {
        Self::for_test()
    }
}

/// Async executor contract for provider calls, waits, & effects.
#[async_trait]
pub trait ExecuteNode: Send + Sync {
    async fn execute_node(
        &self,
        spec: &NodeSpec,
        ctx: &NodeDispatchContext,
    ) -> Result<NodeOutput, DispatcherError>;
}

/// Fallback dispatcher. Every `NodeKind` returns
/// `DispatcherError::NodeUnsupported(kind_label)`. The match is exhaustive so
/// the type system blocks a release that adds a `NodeKind` variant without
/// updating this dispatcher.
#[derive(Debug, Default, Clone, Copy)]
pub struct NodeDispatcher;

#[async_trait]
impl ExecuteNode for NodeDispatcher {
    async fn execute_node(
        &self,
        spec: &NodeSpec,
        _ctx: &NodeDispatchContext,
    ) -> Result<NodeOutput, DispatcherError> {
        // Exhaustive match makes new node kinds a compile error here.
        let kind_label = match spec.kind {
            NodeKind::Agent => "Agent",
            NodeKind::Recipe => "Recipe",
            NodeKind::Transform => "Transform",
            NodeKind::Condition => "Condition",
            NodeKind::Join => "Join",
            NodeKind::Map => "Map",
            NodeKind::Loop => "Loop",
            NodeKind::WaitTimer => "WaitTimer",
            NodeKind::WaitEvent => "WaitEvent",
            NodeKind::Approval => "Approval",
            NodeKind::Effect => "Effect",
            NodeKind::Subworkflow => "Subworkflow",
        };
        Err(DispatcherError::NodeUnsupported(kind_label.to_string()))
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_workflow::control_plane::prelude::{
        Capability, CapabilitySet, EdgeSpec, RetryPolicy,
    };

    fn spec(kind: NodeKind) -> NodeSpec {
        NodeSpec {
            id: format!("n_test_{kind:?}"),
            kind,
            label: None,
            capabilities: CapabilitySet::from_iter([Capability::new("fs.read")]),
            edges: Vec::<EdgeSpec>::new(),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: rightkit_workflow::control_plane::prelude::BackoffStrategy::Fixed {
                    delay_ms: 100,
                },
                retryable: Vec::new(),
            },
            config: Default::default(),
        }
    }

    fn ctx() -> NodeDispatchContext {
        NodeDispatchContext::default()
    }

    #[test]
    fn iteration_scope_key_parses_only_canonical_suffix() {
        assert_eq!(iteration_index_from_id("map__iter_12"), Some(12));
        assert_eq!(iteration_index_from_id("map__iter_bad"), None);
        assert_eq!(iteration_index_from_id("map_12"), None);
    }

    #[test]
    fn workspace_containment_uses_canonical_paths() {
        let root = tempfile::tempdir().expect("workspace root");
        let child = root.path().join("child");
        std::fs::create_dir(&child).expect("child workspace");
        let outside = tempfile::tempdir().expect("outside root");

        assert!(workspace_contains(
            &root.path().display().to_string(),
            &child.display().to_string(),
        ));
        assert!(!workspace_contains(
            &root.path().display().to_string(),
            &outside.path().display().to_string(),
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn every_kind_is_rejected_in_wp03() {
        let dispatcher = NodeDispatcher;
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
            let err = dispatcher
                .execute_node(&spec(kind), &ctx())
                .await
                .expect_err("every kind must be rejected in WP-03a");
            match err {
                DispatcherError::NodeUnsupported(label) => {
                    assert_eq!(label, format!("{kind:?}"));
                }
                other => panic!("unexpected dispatcher error: {other}"),
            }
        }
    }

    #[test]
    fn node_output_serializes_into_json_value() {
        let output = NodeOutput {
            output_json: serde_json::json!({"answer": 42}),
            artifacts: vec!["artifact-1".to_string()],
            cost_usd_micros: 1_234_567,
            tokens_used: 4096,
        };
        // Round-trip via serde_json::Value to assert the public shape is
        // machine-readable (the daemon will persist this verbatim).
        let json = serde_json::to_value(&output).expect("serialize");
        assert_eq!(json["output_json"]["answer"], 42);
        assert_eq!(json["artifacts"][0], "artifact-1");
        assert_eq!(json["cost_usd_micros"], 1_234_567_u64);
        assert_eq!(json["tokens_used"], 4096_u64);
    }

    #[test]
    fn deterministic_transform_is_bounded_and_unknown_ops_fail_closed() {
        let mut node = spec(NodeKind::Transform);
        node.config = NodeConfig::Execution {
            runtime: "deterministic".into(),
            instruction: String::new(),
            runtime_target: None,
            recipe_ref: None,
            inputs_json: r#"{"answer":42}"#.into(),
            transform_operation: Some("json-to-markdown".into()),
            template: None,
            max_runtime_ms: 100,
            max_output_bytes: 128,
            output_schema: "schema://reference/opaque".into(),
        };
        assert!(execute_transform(&node)
            .expect("known transform")
            .output_json
            .as_str()
            .is_some_and(|value| value.contains("42")));
        if let NodeConfig::Execution {
            transform_operation,
            ..
        } = &mut node.config
        {
            *transform_operation = Some("shell".into());
        }
        assert!(execute_transform(&node).is_err());
        assert!(validate_output(serde_json::json!({}), 32, "schema://unknown/result-v1").is_err());
    }

    #[test]
    fn a2a_artifact_validation_rejects_workspace_escape() {
        assert!(validate_a2a_artifacts(&["reports/result.json".into()]).is_ok());
        assert!(validate_a2a_artifacts(&["../secret.txt".into()]).is_err());
        assert!(validate_a2a_artifacts(&["C:/secret.txt".into()]).is_err());
    }
}
