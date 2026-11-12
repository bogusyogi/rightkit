//! `todo_write` — the agent's in-session task list (Claude-Code `TodoWrite` parity).
//!
//! The single most load-bearing "boring but reliable" tool: on any multi-step
//! task the agent writes down the plan and keeps it current, so it never loses
//! the thread across long tool loops. Each call **replaces** the whole list
//! (Claude-Code semantics), so the tool is stateless — the latest call is the
//! source of truth. The list is mirrored into the host app's task graph (the engine
//! emits `TaskGraphUpdated` from the parsed todos) so the UI renders it for free.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_agent_engine_protocol::{TaskNode, TaskState};
use serde_json::{json, Value};

pub struct TodoWriteTool;

#[async_trait::async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Record and update your task list for the current work. Call this at the start of any \
         multi-step task and after each step completes. Pass the COMPLETE list every time — it \
         replaces the previous list. Each item has `content` (imperative, e.g. 'Add the login \
         route'), `status` (pending | in_progress | completed), and optional `activeForm` (present \
         continuous, e.g. 'Adding the login route'). Keep exactly one item in_progress at a time. \
         Mark an item completed the moment it is done — don't batch completions."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["todos"],
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete, ordered task list (replaces any previous list)",
                    "items": {
                        "type": "object",
                        "required": ["content", "status"],
                        "properties": {
                            "content": { "type": "string", "description": "Imperative description of the task" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "activeForm": { "type": "string", "description": "Present-continuous form shown while in progress" }
                        }
                    }
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let todos = match args["todos"].as_array() {
            Some(t) => t,
            None => return ToolResult::error("Missing required argument: todos (an array)"),
        };
        if todos.is_empty() {
            return ToolResult::error("todos is empty — pass at least one task.");
        }

        let mut in_progress = 0;
        let mut rendered = Vec::with_capacity(todos.len());
        for (i, item) in todos.iter().enumerate() {
            let content = match item["content"].as_str() {
                Some(c) if !c.trim().is_empty() => c,
                _ => return ToolResult::error(format!("todos[{i}].content is missing or empty.")),
            };
            let status = item["status"].as_str().unwrap_or("pending");
            let (mark, label) = match status {
                "completed" => ("[x]", content.to_string()),
                "in_progress" => {
                    in_progress += 1;
                    let active = item["activeForm"].as_str().unwrap_or(content);
                    ("[~]", active.to_string())
                }
                "pending" => ("[ ]", content.to_string()),
                other => {
                    return ToolResult::error(format!(
                        "todos[{i}].status '{other}' is invalid (use pending | in_progress | completed)."
                    ))
                }
            };
            rendered.push(format!("{mark} {label}"));
        }
        if in_progress > 1 {
            return ToolResult::error(format!(
                "{in_progress} tasks are in_progress — keep exactly one in_progress at a time."
            ));
        }

        let total = todos.len();
        let done = todos
            .iter()
            .filter(|t| t["status"].as_str() == Some("completed"))
            .count();
        let header = format!("Todos ({done}/{total} done):");
        ToolResult::ok(format!("{header}\n{}", rendered.join("\n")))
    }
}

/// Convert a `todo_write` call's arguments into task-graph nodes so the engine
/// can emit `TaskGraphUpdated`. Invalid items are skipped; an empty result means
/// nothing should be emitted.
pub fn task_nodes_from_args(args: &Value, now: &str) -> Vec<TaskNode> {
    let Some(todos) = args["todos"].as_array() else {
        return Vec::new();
    };
    todos
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let content = item["content"].as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let state = match item["status"].as_str() {
                Some("completed") => TaskState::Done,
                Some("in_progress") => TaskState::InProgress,
                _ => TaskState::Open,
            };
            Some(TaskNode {
                id: format!("todo-{i}"),
                title: content.to_string(),
                deps: Vec::new(),
                done_when: String::new(),
                done_when_predicates: Vec::new(),
                state,
                category: "todo".into(),
                estimated_duration: String::new(),
                assigned_role: String::new(),
                escalation_path: String::new(),
                open_questions: Vec::new(),
                created_at: now.to_string(),
                updated_at: now.to_string(),
            })
        })
        .collect()
}
