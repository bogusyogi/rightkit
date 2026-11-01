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
//! (the shape `DaemonState::tasks` already stores —
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

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> String {
        "2026-08-29T00:00:00Z".to_string()
    }

    fn seed(id: &str, deps: &[&str], state: TaskState) -> TaskNode {
        TaskNode {
            id: id.into(),
            title: id.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            done_when: String::new(),
            done_when_predicates: vec![],
            state,
            category: String::new(),
            estimated_duration: String::new(),
            assigned_role: String::new(),
            escalation_path: String::new(),
            open_questions: vec![],
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn canonical_task_id_is_the_node_id_no_shadow_scheme() {
        let node = seed("a", &[], TaskState::Open);
        assert_eq!(canonical_task_id(&node), "a");
    }

    #[test]
    fn create_task_appends_and_computes_blocked_state() {
        let mut tasks = vec![seed("a", &[], TaskState::Open)];
        let created = create_task(
            &mut tasks,
            CreateTaskInput {
                id: "b".into(),
                title: "B".into(),
                deps: vec!["a".into()],
                done_when: "a done".into(),
                category: "impl".into(),
                estimated_duration: "1h".into(),
                assigned_role: String::new(),
                escalation_path: String::new(),
                now: now(),
            },
        )
        .unwrap();
        assert_eq!(created.state, TaskState::Blocked);
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn create_task_duplicate_id_errors() {
        let mut tasks = vec![seed("a", &[], TaskState::Open)];
        let err = create_task(
            &mut tasks,
            CreateTaskInput {
                id: "a".into(),
                title: "dup".into(),
                now: now(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::Graph(TaskGraphError::DuplicateId(_))
        ));
    }

    #[test]
    fn update_task_patches_fields_without_touching_deps_or_state() {
        let mut tasks = vec![seed("a", &["ghost-not-checked-here"], TaskState::Open)];
        // update_task must not validate deps (that's create/validate's job);
        // it only edits the requested fields.
        let updated = update_task(
            &mut tasks,
            "a",
            TaskPatch {
                title: Some("New title".into()),
                category: Some("bugfix".into()),
                now: now(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title, "New title");
        assert_eq!(updated.category, "bugfix");
        assert_eq!(updated.state, TaskState::Open);
        assert_eq!(updated.deps, vec!["ghost-not-checked-here".to_string()]);
    }

    #[test]
    fn update_task_missing_id_errors() {
        let mut tasks = vec![seed("a", &[], TaskState::Open)];
        let err = update_task(&mut tasks, "ghost", TaskPatch::default()).unwrap_err();
        assert_eq!(err, ProjectionError::NotFound("ghost".to_string()));
    }

    #[test]
    fn mark_active_transitions_open_to_in_progress() {
        let mut tasks = vec![seed("a", &[], TaskState::Open)];
        let updated = mark_active(&mut tasks, "a", now()).unwrap();
        assert_eq!(updated.state, TaskState::InProgress);
    }

    #[test]
    fn mark_active_refuses_terminal_reactivation() {
        let mut tasks = vec![seed("a", &[], TaskState::Done)];
        let err = mark_active(&mut tasks, "a", now()).unwrap_err();
        assert!(matches!(err, ProjectionError::NotFound(_)));
        // State must be untouched on refusal.
        assert_eq!(tasks[0].state, TaskState::Done);
    }

    #[test]
    fn mark_done_unblocks_dependents_through_task_graph() {
        let mut tasks = vec![
            seed("a", &[], TaskState::Open),
            seed("b", &["a"], TaskState::Blocked),
        ];
        mark_done(&mut tasks, "a", now()).unwrap();
        let index = index_by_id(&tasks);
        assert_eq!(index["a"].state, TaskState::Done);
        assert_eq!(index["b"].state, TaskState::Open);
    }

    #[test]
    fn mark_done_unknown_id_errors() {
        let mut tasks = vec![seed("a", &[], TaskState::Open)];
        let err = mark_done(&mut tasks, "ghost", now()).unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::Graph(TaskGraphError::NotFound(_))
        ));
    }

    #[test]
    fn mark_failed_keeps_dependents_blocked() {
        let mut tasks = vec![
            seed("a", &[], TaskState::Open),
            seed("b", &["a"], TaskState::Blocked),
        ];
        mark_failed(&mut tasks, "a", now()).unwrap();
        let index = index_by_id(&tasks);
        assert_eq!(index["a"].state, TaskState::Failed);
        assert_eq!(index["b"].state, TaskState::Blocked);
    }

    #[test]
    fn assign_owner_writes_assigned_role_no_second_field() {
        let mut tasks = vec![seed("a", &[], TaskState::Open)];
        let updated = assign_owner(&mut tasks, "a", "alice".into(), now()).unwrap();
        assert_eq!(updated.assigned_role, "alice");
    }

    #[test]
    fn ready_listing_matches_task_graph_ready_tasks() {
        let tasks = vec![
            seed("a", &[], TaskState::Done),
            seed("b", &["a"], TaskState::Open),
            seed("c", &["b"], TaskState::Open),
        ];
        let ready = ready_listing(&tasks).unwrap();
        let ids: Vec<_> = ready.iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids, vec!["b".to_string()]);
    }

    #[test]
    fn blocked_listing_recomputes_before_reading() {
        // Stored state says Open even though the dep is not Done — a
        // stale snapshot. blocked_listing must recompute, not trust it.
        let tasks = vec![
            seed("a", &[], TaskState::Open),
            seed("b", &["a"], TaskState::Open),
        ];
        let blocked = blocked_listing(&tasks).unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, "b");
    }

    #[test]
    fn reorder_moves_task_before_target() {
        let mut tasks = vec![
            seed("a", &[], TaskState::Open),
            seed("b", &[], TaskState::Open),
            seed("c", &[], TaskState::Open),
        ];
        reorder(&mut tasks, "c", Some("a")).unwrap();
        let ids: Vec<_> = tasks.iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    #[test]
    fn reorder_to_end_when_before_id_is_none() {
        let mut tasks = vec![
            seed("a", &[], TaskState::Open),
            seed("b", &[], TaskState::Open),
        ];
        reorder(&mut tasks, "a", None).unwrap();
        let ids: Vec<_> = tasks.iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn reorder_never_changes_state_or_deps() {
        let mut tasks = vec![
            seed("a", &[], TaskState::Done),
            seed("b", &["a"], TaskState::Open),
        ];
        reorder(&mut tasks, "b", Some("a")).unwrap();
        let index = index_by_id(&tasks);
        assert_eq!(index["a"].state, TaskState::Done);
        assert_eq!(index["b"].deps, vec!["a".to_string()]);
    }

    #[test]
    fn reorder_unknown_target_is_rejected_and_list_is_unchanged() {
        let mut tasks = vec![
            seed("a", &[], TaskState::Open),
            seed("b", &[], TaskState::Open),
        ];
        let before: Vec<_> = tasks.iter().map(|t| t.id.clone()).collect();
        let err = reorder(&mut tasks, "a", Some("ghost")).unwrap_err();
        assert!(matches!(err, ProjectionError::ReorderTargetNotFound(_)));
        let after: Vec<_> = tasks.iter().map(|t| t.id.clone()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn reorder_unknown_source_errors() {
        let mut tasks = vec![seed("a", &[], TaskState::Open)];
        let err = reorder(&mut tasks, "ghost", None).unwrap_err();
        assert_eq!(err, ProjectionError::NotFound("ghost".to_string()));
    }

    #[test]
    fn index_by_id_covers_every_task() {
        let tasks = vec![
            seed("a", &[], TaskState::Open),
            seed("b", &[], TaskState::Open),
        ];
        let index = index_by_id(&tasks);
        assert_eq!(index.len(), 2);
        assert!(index.contains_key("a"));
        assert!(index.contains_key("b"));
    }
}
