//! Normalized task and assignment state, so a host can tell an idle lane from an unknown one.
//!
//! [`TaskBoard`] folds the vendor-neutral [`AgentEvent`] stream into one task list and one set
//! of subagents. Sources:
//! - Claude: `TaskCreate` and `TaskUpdate` tool calls, `TodoWrite` and `update_plan` tool calls
//!   (each replaces the whole list), and `Task` / `Agent` subagents through
//!   [`AgentEvent::SubagentStarted`] and [`AgentEvent::SubagentFinished`].
//! - Codex: `turn/plan/updated` through [`AgentEvent::PlanUpdated`] (replaces the list), and
//!   `collabAgentToolCall` subagents through the same subagent events.
//!
//! Nothing here reads a missing event as idle. [`TaskBoard::idle_lanes`] answers
//! [`Observation::Unknown`] until the session has started, a task list has been reported, and
//! the host has set the lane capacity with [`TaskBoard::set_capacity`]. Assignment is explicit:
//! the host links a ready task to a running agent with [`TaskBoard::assign`].
//!
//! Unverified: the `TaskCreate` task number. The board keys a created task by its tool-use id
//! and renames it to the `#<n>` number when the tool result names one. When the CLI's result
//! text carries no such number, a later `TaskUpdate` by number does not match the task.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{AgentEvent, PlanStep};

/// Claude tool that creates one task.
pub const TASK_CREATE_TOOL: &str = "TaskCreate";
/// Claude tool that updates one task.
pub const TASK_UPDATE_TOOL: &str = "TaskUpdate";
/// Claude tool that replaces the whole todo list.
pub const TODO_WRITE_TOOL: &str = "TodoWrite";
/// Plan tool that replaces the whole plan.
pub const UPDATE_PLAN_TOOL: &str = "update_plan";

/// Where a task stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

/// One task as the agent reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    /// Ids of tasks that must complete first. An id that names no known task blocks.
    pub blocked_by: Vec<String>,
    /// The running agent this task is assigned to, set by [`TaskBoard::assign`].
    pub assigned_agent: Option<String>,
}

/// A count the host can act on, or the admission that it is not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Observation {
    Known { value: u32 },
    Unknown,
}

/// The task list and the running subagents of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskBoard {
    tasks: BTreeMap<String, Task>,
    /// Task ids in the order the list reported them.
    order: Vec<String>,
    /// Subagent id to whether it is still running.
    agents: BTreeMap<String, bool>,
    /// `TaskCreate` tool-use ids whose result may name the task number.
    creating: BTreeSet<String>,
    session_started: bool,
    task_source_seen: bool,
    capacity: Option<u32>,
}

impl TaskBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of agent lanes the host can run at once. `None` makes the capacity unknown.
    pub fn set_capacity(&mut self, lanes: Option<u32>) {
        self.capacity = lanes;
    }

    /// Fold one event into the board. Events without task or agent state are ignored.
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init { .. } | AgentEvent::SessionInit(_) => self.session_started = true,
            AgentEvent::ToolUse {
                id, name, input, ..
            } => self.tool_use(id, name, input),
            AgentEvent::ToolResultContent { id, content, .. } => self.tool_result(id, content),
            AgentEvent::PlanUpdated { plan } => self.replace_list(plan),
            AgentEvent::SubagentStarted { id, .. } => {
                self.agents.insert(id.clone(), true);
            }
            AgentEvent::SubagentFinished { id, .. } => self.finish_agent(id),
            _ => {}
        }
    }

    /// Fold a sequence of events in order. A host adapter calls this on each batch it reads.
    pub fn apply_all<'a>(&mut self, events: impl IntoIterator<Item = &'a AgentEvent>) {
        for event in events {
            self.apply(event);
        }
    }

    /// Link a task to a running agent. `false` when the task does not exist, is not pending,
    /// or the agent is not running.
    pub fn assign(&mut self, task_id: &str, agent_id: &str) -> bool {
        let running = self.agents.get(agent_id).copied().unwrap_or(false);
        if !running {
            return false;
        }
        match self.tasks.get_mut(task_id) {
            Some(task) if task.status == TaskStatus::Pending => {
                task.assigned_agent = Some(agent_id.to_string());
                true
            }
            _ => false,
        }
    }

    /// Every task, in the order the list reported them.
    pub fn tasks(&self) -> Vec<&Task> {
        self.order
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .collect()
    }

    /// Pending, unassigned tasks whose blockers have all completed, in list order.
    pub fn ready_tasks(&self) -> Vec<&Task> {
        self.tasks()
            .into_iter()
            .filter(|task| {
                task.status == TaskStatus::Pending
                    && task.assigned_agent.is_none()
                    && task.blocked_by.iter().all(|blocker| {
                        self.tasks
                            .get(blocker)
                            .is_some_and(|b| b.status == TaskStatus::Completed)
                    })
            })
            .collect()
    }

    /// Subagents observed running now.
    pub fn running_agents(&self) -> usize {
        self.agents.values().filter(|running| **running).count()
    }

    /// How many more ready tasks a lane could take now: the smaller of the ready tasks and the
    /// spare lanes (capacity minus running agents). [`Observation::Unknown`] until the session
    /// has started, a task list has been reported, and the capacity is set. A host raises
    /// IDLE_LANE only on a `Known` value above zero.
    pub fn idle_lanes(&self) -> Observation {
        match (self.session_started, self.task_source_seen, self.capacity) {
            (true, true, Some(capacity)) => {
                let spare = capacity.saturating_sub(self.running_agents() as u32);
                let ready = self.ready_tasks().len() as u32;
                Observation::Known {
                    value: spare.min(ready),
                }
            }
            _ => Observation::Unknown,
        }
    }

    fn tool_use(&mut self, id: &str, name: &str, input: &Value) {
        match name {
            TASK_CREATE_TOOL => {
                self.task_source_seen = true;
                let title = text(input, &["subject", "title", "content", "description"])
                    .unwrap_or_default();
                let blocked_by = blocked_by(input).unwrap_or_default();
                self.insert_task(id, title, TaskStatus::Pending, blocked_by);
                self.creating.insert(id.to_string());
            }
            TASK_UPDATE_TOOL => {
                self.task_source_seen = true;
                let Some(target) = text(input, &["taskId", "id"]) else {
                    return;
                };
                let status = input
                    .get("status")
                    .and_then(Value::as_str)
                    .and_then(parse_status);
                let title = text(input, &["subject", "title"]);
                let blockers = blocked_by(input);
                if let Some(task) = self.tasks.get_mut(&target) {
                    if let Some(status) = status {
                        task.status = status;
                    }
                    if let Some(title) = title {
                        task.title = title;
                    }
                    if let Some(blockers) = blockers {
                        task.blocked_by = blockers;
                    }
                }
            }
            TODO_WRITE_TOOL => {
                let steps = steps_from(input.get("todos"), "content");
                self.replace_list(&steps);
            }
            UPDATE_PLAN_TOOL => {
                let steps = steps_from(input.get("plan"), "step");
                self.replace_list(&steps);
            }
            _ => {}
        }
    }

    fn tool_result(&mut self, id: &str, content: &str) {
        if !self.creating.remove(id) {
            return;
        }
        let Some(number) = task_number(content) else {
            return;
        };
        if number == id || self.tasks.contains_key(&number) {
            return;
        }
        let Some(mut task) = self.tasks.remove(id) else {
            return;
        };
        task.id = number.clone();
        self.tasks.insert(number.clone(), task);
        for slot in self.order.iter_mut() {
            if slot.as_str() == id {
                *slot = number.clone();
            }
        }
        for other in self.tasks.values_mut() {
            for blocker in other.blocked_by.iter_mut() {
                if blocker.as_str() == id {
                    *blocker = number.clone();
                }
            }
        }
    }

    fn insert_task(
        &mut self,
        id: &str,
        title: String,
        status: TaskStatus,
        blocked_by: Vec<String>,
    ) {
        if !self.tasks.contains_key(id) {
            self.order.push(id.to_string());
        }
        self.tasks.insert(
            id.to_string(),
            Task {
                id: id.to_string(),
                title,
                status,
                blocked_by,
                assigned_agent: None,
            },
        );
    }

    /// A list report replaces every task. Assignments carry over to a step with the same id.
    fn replace_list(&mut self, steps: &[PlanStep]) {
        self.task_source_seen = true;
        let previous: BTreeMap<String, String> = self
            .tasks
            .iter()
            .filter_map(|(key, task)| {
                task.assigned_agent
                    .clone()
                    .map(|agent| (key.clone(), agent))
            })
            .collect();
        self.tasks.clear();
        self.order.clear();
        self.creating.clear();
        for (index, step) in steps.iter().enumerate() {
            let id = format!("step-{index}");
            let assigned_agent = previous.get(&id).cloned();
            self.tasks.insert(
                id.clone(),
                Task {
                    id: id.clone(),
                    title: step.step.clone(),
                    status: parse_status(&step.status).unwrap_or(TaskStatus::Pending),
                    blocked_by: Vec::new(),
                    assigned_agent,
                },
            );
            self.order.push(id);
        }
    }

    fn finish_agent(&mut self, id: &str) {
        if let Some(running) = self.agents.get_mut(id) {
            *running = false;
        }
        for task in self.tasks.values_mut() {
            if task.assigned_agent.as_deref() == Some(id) {
                task.assigned_agent = None;
            }
        }
    }
}

/// The first non-empty string among `keys`.
fn text(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

/// `blockedBy` as task ids; numbers are read as their decimal text. `None` when absent.
fn blocked_by(input: &Value) -> Option<Vec<String>> {
    input
        .get("blockedBy")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(id) => Some(id.clone()),
                    Value::Number(id) => Some(id.to_string()),
                    _ => None,
                })
                .collect()
        })
}

/// A status word the vendors use. `None` for a word this crate does not know.
fn parse_status(word: &str) -> Option<TaskStatus> {
    match word {
        "pending" | "todo" | "not_started" | "notStarted" => Some(TaskStatus::Pending),
        "in_progress" | "inProgress" | "running" => Some(TaskStatus::InProgress),
        "completed" | "complete" | "done" => Some(TaskStatus::Completed),
        _ => None,
    }
}

/// Items of a list tool, read as steps with the title under `title_key`.
fn steps_from(items: Option<&Value>, title_key: &str) -> Vec<PlanStep> {
    items
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(PlanStep {
                step: item.get(title_key).and_then(Value::as_str)?.to_string(),
                status: item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending")
                    .to_string(),
            })
        })
        .collect()
}

/// The digits after the first `#` in a tool result, such as `Task #7 created`.
fn task_number(content: &str) -> Option<String> {
    let start = content.find('#')? + 1;
    let digits: String = content[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then_some(digits)
}
