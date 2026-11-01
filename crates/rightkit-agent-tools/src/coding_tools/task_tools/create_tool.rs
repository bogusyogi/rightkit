// Background task tools for durable agent-managed work.
//
// `todo_write` is the lightweight in-turn checklist. These `task_*` tools are
// for longer-lived records and shell-backed background work that an agent can
// create, list, poll, update, and stop.

#[cfg(feature = "coding_tools-host")]
use crate::coding_tools::background_shell::{GetShellOutputTool, KillShellTool};
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_engine_protocol::{TaskNode, TaskState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static TASK_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub node: TaskNode,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub shell_id: Option<String>,
    #[serde(default)]
    pub output: String,
}

pub type TaskStore = Arc<Mutex<HashMap<String, TaskRecord>>>;

pub struct TaskCreateTool;
pub struct TaskUpdateTool;
pub struct TaskGetTool;

#[cfg(feature = "coding_tools-host")]
fn map_background_admission(
    result: Result<String, crate::coding_tools::process_resource_authority::ProcessAdmissionError>,
) -> Result<String, String> {
    result.map_err(|error| error.to_string())
}
pub struct TaskListTool;
pub struct TaskOutputTool;
pub struct TaskStopTool;

#[async_trait::async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "task_create"
    }

    fn description(&self) -> &str {
        "Create a durable background task record. If `command` is provided, starts it with the background shell runner and returns both task_id and shell_id. If `kind` is `local_agent`, delegates the task to a configured sub-agent role and stores the result."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["title"],
            "properties": {
                "title": { "type": "string" },
                "kind": { "type": "string", "enum": ["record", "shell", "local_agent"], "description": "Task type. Defaults to shell when command is present, otherwise record." },
                "command": { "type": "string", "description": "Optional shell command to run in the background" },
                "task": { "type": "string", "description": "Optional task prompt for local_agent tasks. Defaults to title." },
                "done_when": { "type": "string" },
                "deps": { "type": "array", "items": { "type": "string" } },
                "assigned_role": { "type": "string" },
                "category": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let title = match args["title"].as_str().map(str::trim) {
            Some(title) if !title.is_empty() => title.to_string(),
            _ => return ToolResult::error("Missing required argument: title"),
        };
        let command = args["command"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let requested_kind = args["kind"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if command.is_some() && matches!(requested_kind, Some("local_agent")) {
            return ToolResult::error("task_create: local_agent tasks cannot also set command");
        }
        let shell_id = if let Some(command) = command {
            // A command-backed task needs a background shell. On a host that
            // cannot spawn processes, say so in the tool result rather than
            // silently creating a task that will never run — an unexplained
            // no-op is a tool the model retries.
            #[cfg(not(feature = "coding_tools-host"))]
            {
                let _ = command;
                return ToolResult::error(
                    "host_capability_unavailable: task_create with a command needs a background shell, which this host cannot spawn. Create the task without a command, or hand the work to a paired host or CI.",
                );
            }
            #[cfg(feature = "coding_tools-host")]
            {
                if let Some(backend) = ctx
                    .execution_backend
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .filter(|value| !value.eq_ignore_ascii_case("local"))
                {
                    return ToolResult::error(format!(
                        "task_create command executionBackend '{backend}' is not routed through the selected non-local backend; use shell(run_in_background=true) for backend-routed long-running commands or select local."
                    ));
                }
                if let Some(message) = ctx.unenforced_network_profile_error("task_create") {
                    return ToolResult::error(message);
                }
                let store =
                    match &ctx.background_shells {
                        Some(store) => store,
                        None => return ToolResult::error(
                            "task_create with command requires background shells in this context.",
                        ),
                    };
                match map_background_admission(
                    crate::coding_tools::background_shell::spawn_background_with_target(
                        command.to_string(),
                        &ctx.workspace_root,
                        store,
                        ctx.cancel_rx.clone(),
                        crate::coding_tools::background_shell::BgExecutionTarget {
                            backend_id: ctx.execution_backend.clone(),
                            workspace_id: ctx.execution_workspace_id.clone(),
                            network_profile: ctx.execution_network_profile.clone(),
                        },
                    ),
                ) {
                    Ok(shell_id) => Some(shell_id),
                    Err(error) => return ToolResult::error(error.to_string()),
                }
            }
        } else {
            None
        };
        let now = now_string();
        let id = format!("task-{}", TASK_SEQ.fetch_add(1, Ordering::Relaxed));
        let mut record = TaskRecord {
            node: TaskNode {
                id: id.clone(),
                title: title.clone(),
                deps: string_array(&args["deps"]),
                done_when: args["done_when"].as_str().unwrap_or_default().to_string(),
                done_when_predicates: Vec::new(),
                state: if shell_id.is_some() {
                    TaskState::InProgress
                } else {
                    TaskState::Open
                },
                category: args["category"].as_str().unwrap_or("task").to_string(),
                estimated_duration: String::new(),
                assigned_role: args["assigned_role"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                escalation_path: String::new(),
                open_questions: Vec::new(),
                created_at: now.clone(),
                updated_at: now,
            },
            kind: if shell_id.is_some() {
                "shell".into()
            } else if matches!(requested_kind, Some("local_agent")) {
                "local_agent".into()
            } else {
                "record".into()
            },
            command: command.map(str::to_string),
            shell_id,
            output: String::new(),
        };
        if record.kind == "local_agent" {
            let role = record.node.assigned_role.trim().to_string();
            if role.is_empty() {
                return ToolResult::error("task_create: local_agent tasks require assigned_role");
            }
            let Some(spawner) = &ctx.spawner else {
                return ToolResult::error(
                    "task_create: local_agent task requires sub-agent orchestration in this context",
                );
            };
            let task = args["task"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&title);
            record.node.state = TaskState::InProgress;
            let result = spawner
                .spawn(
                    &role,
                    task,
                    &ctx.session_id,
                    crate::coding_tools::context::SubagentSpawnOptions::default(),
                )
                .await;
            record.node.updated_at = now_string();
            match result {
                Ok(output) => {
                    record.node.state = TaskState::Done;
                    record.output = output;
                }
                Err(e) => {
                    record.node.state = TaskState::Failed;
                    record.output = format!("local_agent failed: {e}");
                }
            }
        }
        let mut records = match load_effective_records(ctx) {
            Ok(records) => records,
            Err(e) => return ToolResult::error(e),
        };
        records.insert(id.clone(), record.clone());
        if let Err(e) = save_records(ctx, &records) {
            return ToolResult::error(e);
        }
        set_memory_records(ctx, records);
        ToolResult::ok(render_record(&record))
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::coding_tools::context::ToolContext;
    use rightkit_agent_engine_runtime_resources::ServiceScope;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    #[test]
    fn command_admission_denial_maps_before_task_publication() {
        let denial =
            crate::coding_tools::process_resource_authority::ProcessAdmissionError::Unconfigured;
        let result = super::map_background_admission(Err(denial));
        assert!(result
            .expect_err("denial must stop task publication")
            .contains("resource_governor_unconfigured"));
    }

    #[tokio::test]
    async fn command_admission_denial_leaves_task_store_without_in_progress_record() {
        let dir = tempdir().unwrap();
        let shell_store: crate::coding_tools::background_shell::BgStore =
            Arc::new(Mutex::new(HashMap::new()));
        let task_store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let governor =
            match crate::coding_tools::process_resource_authority::process_resource_governor() {
                Some(governor) => governor,
                None => {
                    let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
                    crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
                );
                    crate::coding_tools::process_resource_authority::process_resource_governor()
                        .expect("test must resolve installed resource governor")
                }
            };
        let scope = ServiceScope::Workspace(dir.path().to_string_lossy().into_owned());
        let _ = &governor;
        crate::coding_tools::process_resource_authority::test_denied_scopes()
            .lock()
            .unwrap()
            .insert(scope.clone());
        struct Undeny(ServiceScope);
        impl Drop for Undeny {
            fn drop(&mut self) {
                crate::coding_tools::process_resource_authority::test_denied_scopes()
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&self.0);
            }
        }
        let _undeny = Undeny(scope.clone());

        let ctx = ToolContext::new(
            concat!("task", "-admission-denial"),
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_background_shells(shell_store.clone())
        .with_task_store(task_store.clone());
        let result = TaskCreateTool
            .execute(
                json!({
                    "title": "Denied background task",
                    "command": "echo should-not-run"
                }),
                &ctx,
            )
            .await;

        assert!(
            result.is_error,
            "admission denial must reach caller: {}",
            result.content
        );
        assert!(
            result.content.contains("resource_admission_denied")
                || result.content.contains("resource_governor_unconfigured"),
            "unexpected admission result: {}",
            result.content
        );
        assert!(shell_store.lock().unwrap().is_empty());
        let records = task_store.lock().unwrap();
        assert!(
            records.is_empty(),
            "denied task must not be published: {records:?}"
        );
        assert!(records
            .values()
            .all(|record| record.node.state != TaskState::InProgress));
        drop(records);
        assert!(!dir.path().join(".rightkit").join("tasks.json").exists());
    }
}
