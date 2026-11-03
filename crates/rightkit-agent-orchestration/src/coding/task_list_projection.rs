//! PL-006 — Lightweight task-list projection over the canonical TaskGraph.
//!
//! Donor lineage (`REPAIR/WIRE`, canon PL-006): the canonical mechanism is
//! the host app's own [`rightkit_agent_engine_tasks::TaskGraph`]
//! (`engine/crates/tasks/src/graph.rs`) — this module wires a lightweight
//! ergonomic surface over it, it does not reimplement graph state. Pi's
//! task-list extensions (`packages/coding-agent/src/core/extensions/`,
//! the todo/task-list tool surface referenced by canon as "ergonomics
//! reference only") informed the operation shape (create/update/
//! mark-active/done/failed, ready & blocked listings, non-semantic
//! reordering, owner assignment) but contribute no code: `TaskGraph`
//! already owns dependency-aware state, ready/blocked computation and
//! terminal-state discipline, so this module composes those existing
//! methods instead of re-deriving them.
//!
//! **Projection only.** Every operation here builds a
//! [`rightkit_agent_engine_tasks::TaskGraph`] from the caller's `Vec<TaskNode>`
//! (the shape [`DaemonState::tasks`] already stores —
//! `engine/host-app/src/api_state_types.rs`), performs the mutation
//! through `TaskGraph`'s own methods (`add_task`, `mark_done`,
//! `recompute_states`, `ready_tasks`), and returns the updated flat list.
//! No second store, no shadow ID scheme: a task's canonical id
//! ([`TaskNode::id`]) *is* the projection id (see [`canonical_task_id`]).

use std::collections::HashMap;

use rightkit_agent_engine_protocol::{TaskNode, TaskState};
use rightkit_agent_engine_tasks::{TaskGraph, TaskGraphError};

/// Errors returned by projection operations. Wraps
/// [`rightkit_agent_engine_tasks::TaskGraphError`] (the canonical graph's own error
/// type) plus the few projection-local failure modes (duplicate create
/// input, reorder target not found) that have no graph equivalent.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectionError {
    #[error(transparent)]
    Graph(#[from] TaskGraphError),
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("reorder target not found: {0}")]
    ReorderTargetNotFound(String),
}

/// Fields accepted when creating a new task through the projection.
/// Mirrors [`TaskNode`]'s writable fields; `state` always starts `Open`
/// (or `Blocked` once `recompute_states` runs, if `deps` are incomplete)
/// and timestamps are caller-supplied ISO-8601 strings, matching
/// `TaskNode`'s existing `String` timestamp convention
/// (`engine/crates/protocol/src/task.rs`).
#[derive(Debug, Clone, Default)]
pub struct CreateTaskInput {
    pub id: String,
    pub title: String,
    pub deps: Vec<String>,
    pub done_when: String,
    pub category: String,
    pub estimated_duration: String,
    pub assigned_role: String,
    pub escalation_path: String,
    pub now: String,
}

/// Non-semantic field updates: title/category/estimated_duration/
/// escalation_path/open_questions text edits that do not change
/// dependency or state semantics. `None` leaves a field unchanged.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub category: Option<String>,
    pub estimated_duration: Option<String>,
    pub escalation_path: Option<String>,
    pub open_questions: Option<Vec<String>>,
    pub now: String,
}

/// The projection id for a task is its canonical `TaskGraph` id — no
/// shadow identifier system. Exists so call sites can name the invariant
/// explicitly rather than reaching into `TaskNode::id` directly.
pub fn canonical_task_id(node: &TaskNode) -> &str {
    &node.id
}

/// Build a [`TaskGraph`] from a flat task list (the shape `DaemonState`
/// stores). Errors on duplicate ids — the same discipline `TaskGraph`
/// enforces at insertion.
pub fn graph_from_tasks(tasks: &[TaskNode]) -> Result<TaskGraph, TaskGraphError> {
    let mut graph = TaskGraph::new();
    for node in tasks {
        graph.add_task(node.clone())?;
    }
    Ok(graph)
}

/// Flatten a [`TaskGraph`] back to insertion-ordered `Vec<TaskNode>`.
pub fn tasks_from_graph(graph: &TaskGraph) -> Vec<TaskNode> {
    graph.tasks().cloned().collect()
}

/// Create a task and append it to the list. The new task's dependency
/// state is computed immediately (`recompute_states`) so a task created
/// with incomplete deps starts life correctly `Blocked`, never a stale
/// `Open`.
pub fn create_task(
    tasks: &mut Vec<TaskNode>,
    input: CreateTaskInput,
) -> Result<TaskNode, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    let node = TaskNode {
        id: input.id.clone(),
        title: input.title,
        deps: input.deps,
        done_when: input.done_when,
        done_when_predicates: vec![],
        state: TaskState::Open,
        category: input.category,
        estimated_duration: input.estimated_duration,
        assigned_role: input.assigned_role,
        escalation_path: input.escalation_path,
        open_questions: vec![],
        created_at: input.now.clone(),
        updated_at: input.now,
    };
    graph.add_task(node)?;
    graph.recompute_states();
    *tasks = tasks_from_graph(&graph);
    graph
        .get(&input.id)
        .cloned()
        .ok_or(ProjectionError::NotFound(input.id))
}

/// Apply a non-semantic field patch (title/category/estimated_duration/
/// escalation_path/open_questions). Never touches `deps` or `state`.
pub fn update_task(
    tasks: &mut Vec<TaskNode>,
    id: &str,
    patch: TaskPatch,
) -> Result<TaskNode, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    {
        let node = graph
            .get_mut(id)
            .ok_or_else(|| ProjectionError::NotFound(id.to_string()))?;
        if let Some(title) = patch.title {
            node.title = title;
        }
        if let Some(category) = patch.category {
            node.category = category;
        }
        if let Some(duration) = patch.estimated_duration {
            node.estimated_duration = duration;
        }
        if let Some(path) = patch.escalation_path {
            node.escalation_path = path;
        }
        if let Some(questions) = patch.open_questions {
            node.open_questions = questions;
        }
        node.updated_at = patch.now;
    }
    *tasks = tasks_from_graph(&graph);
    graph
        .get(id)
        .cloned()
        .ok_or_else(|| ProjectionError::NotFound(id.to_string()))
}

/// Mark a task `InProgress`. Errors if the task does not exist; does not
/// override a terminal state (`Done`/`Failed`/`Merged`/`Split`) — callers
/// checking `TaskState` first get an honest no-op-with-error rather than
/// silent reanimation of finished work.
pub fn mark_active(
    tasks: &mut Vec<TaskNode>,
    id: &str,
    now: String,
) -> Result<TaskNode, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    {
        let node = graph
            .get_mut(id)
            .ok_or_else(|| ProjectionError::NotFound(id.to_string()))?;
        if matches!(
            node.state,
            TaskState::Done
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::Merged
                | TaskState::Split
        ) {
            return Err(ProjectionError::NotFound(format!(
                "task {id} is terminal and cannot change state"
            )));
        }
        node.state = TaskState::InProgress;
        node.updated_at = now;
    }
    graph.recompute_states();
    *tasks = tasks_from_graph(&graph);
    graph
        .get(id)
        .cloned()
        .ok_or_else(|| ProjectionError::NotFound(id.to_string()))
}

/// Mark a task `Done` via `TaskGraph::mark_done`, which also recomputes
/// downstream dependents (a dependent blocked only on this task unblocks
/// to `Open`).
pub fn mark_done(
    tasks: &mut Vec<TaskNode>,
    id: &str,
    now: String,
) -> Result<TaskNode, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    if matches!(
        graph.get(id).map(|node| &node.state),
        Some(
            TaskState::Done
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::Merged
                | TaskState::Split
        )
    ) {
        return Err(ProjectionError::NotFound(format!(
            "task {id} is terminal and cannot change state"
        )));
    }
    graph.mark_done(id)?;
    if let Some(node) = graph.get_mut(id) {
        node.updated_at = now;
    }
    *tasks = tasks_from_graph(&graph);
    graph
        .get(id)
        .cloned()
        .ok_or_else(|| ProjectionError::NotFound(id.to_string()))
}

/// Mark a task `Failed`. Terminal like `Done`/`Cancelled`/`Merged`/`Split`; downstream
/// dependents stay `Blocked` (a `Failed` dependency never satisfies
/// `recompute_states`'s "all deps `Done`" unblock condition).
pub fn mark_failed(
    tasks: &mut Vec<TaskNode>,
    id: &str,
    now: String,
) -> Result<TaskNode, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    {
        let node = graph
            .get_mut(id)
            .ok_or_else(|| ProjectionError::NotFound(id.to_string()))?;
        if matches!(
            node.state,
            TaskState::Done
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::Merged
                | TaskState::Split
        ) {
            return Err(ProjectionError::NotFound(format!(
                "task {id} is terminal and cannot change state"
            )));
        }
        node.state = TaskState::Failed;
        node.updated_at = now;
    }
    graph.recompute_states();
    *tasks = tasks_from_graph(&graph);
    graph
        .get(id)
        .cloned()
        .ok_or_else(|| ProjectionError::NotFound(id.to_string()))
}

/// Mark a task cancelled by its owner. Cancellation is terminal and keeps
/// dependent tasks blocked, just like failure, while preserving its cause.
pub fn mark_cancelled(
    tasks: &mut Vec<TaskNode>,
    id: &str,
    now: String,
) -> Result<TaskNode, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    {
        let node = graph
            .get_mut(id)
            .ok_or_else(|| ProjectionError::NotFound(id.to_string()))?;
        if matches!(
            node.state,
            TaskState::Done
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::Merged
                | TaskState::Split
        ) {
            return Err(ProjectionError::NotFound(format!(
                "task {id} is terminal and cannot change state"
            )));
        }
        node.state = TaskState::Cancelled;
        node.updated_at = now;
    }
    graph.recompute_states();
    *tasks = tasks_from_graph(&graph);
    graph
        .get(id)
        .cloned()
        .ok_or_else(|| ProjectionError::NotFound(id.to_string()))
}

/// Assign (or clear, with `""`) an owner. Reuses `TaskNode::assigned_role`
/// — the field the canonical graph already carries for this purpose —
/// rather than introducing a second owner field/store.
pub fn assign_owner(
    tasks: &mut Vec<TaskNode>,
    id: &str,
    owner: String,
    now: String,
) -> Result<TaskNode, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    {
        let node = graph
            .get_mut(id)
            .ok_or_else(|| ProjectionError::NotFound(id.to_string()))?;
        node.assigned_role = owner;
        node.updated_at = now;
    }
    *tasks = tasks_from_graph(&graph);
    graph
        .get(id)
        .cloned()
        .ok_or_else(|| ProjectionError::NotFound(id.to_string()))
}

/// Ready listing: tasks whose dependencies are all `Done` and which are
/// not themselves terminal, via `TaskGraph::ready_tasks`.
pub fn ready_listing(tasks: &[TaskNode]) -> Result<Vec<TaskNode>, ProjectionError> {
    let graph = graph_from_tasks(tasks)?;
    Ok(graph.ready_tasks().into_iter().cloned().collect())
}

/// Blocked listing: tasks currently in `TaskState::Blocked`, computed
/// fresh (`recompute_states`) rather than trusting a possibly-stale stored
/// state, so a caller never sees a blocked task whose dependency actually
/// finished without a recompute in between.
pub fn blocked_listing(tasks: &[TaskNode]) -> Result<Vec<TaskNode>, ProjectionError> {
    let mut graph = graph_from_tasks(tasks)?;
    graph.recompute_states();
    Ok(graph
        .tasks()
        .filter(|n| n.state == TaskState::Blocked)
        .cloned()
        .collect())
}

/// Non-semantic reordering: move the task with `id` to immediately before
/// `before_id` (or to the end when `before_id` is `None`). This changes
/// only the flat list's display order — the order `TaskGraph` uses for
/// stable iteration and tie-breaking, never dependency or state
/// semantics. `topological_order`/`ready_tasks` results are unaffected in
/// content, only in the order equally-ready tasks may be listed.
pub fn reorder(
    tasks: &mut Vec<TaskNode>,
    id: &str,
    before_id: Option<&str>,
) -> Result<(), ProjectionError> {
    let current_pos = tasks
        .iter()
        .position(|n| n.id == id)
        .ok_or_else(|| ProjectionError::NotFound(id.to_string()))?;

    // Resolve the target position *before* mutating the list, so a missing
    // `before_id` fails without ever removing the source task.
    let insert_pos = match before_id {
        None => tasks.len(),
        Some(before) => tasks
            .iter()
            .position(|n| n.id == before)
            .ok_or_else(|| ProjectionError::ReorderTargetNotFound(before.to_string()))?,
    };

    let node = tasks.remove(current_pos);
    // Removing an earlier element shifts every later index left by one;
    // correct the insertion point when the source sat before the target.
    let insert_pos = if current_pos < insert_pos {
        insert_pos - 1
    } else {
        insert_pos
    };
    tasks.insert(insert_pos, node);
    Ok(())
}

/// Canonical-task-ID linkage lookup: a stable `id -> TaskNode` index for
/// callers that need repeated lookups without re-walking the flat list
/// (e.g. resolving multiple projection ids per request). Read-only — does
/// not itself mutate the graph.
pub fn index_by_id(tasks: &[TaskNode]) -> HashMap<&str, &TaskNode> {
    tasks.iter().map(|n| (n.id.as_str(), n)).collect()
}
