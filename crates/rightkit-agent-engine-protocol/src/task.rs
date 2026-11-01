use serde::{Deserialize, Serialize};

/// The state of a task in the DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    /// Not yet done — work hasn't started, or evidence is stale.
    Open,
    /// Some weighted DoneWhen predicates are satisfied, but not all.
    InProgress,
    /// Blocked because at least one dependency task is not Done.
    Blocked,
    /// DoneWhen predicate holds AND all deps are Done.
    Done,
    /// The task failed and requires repair or replanning.
    Failed,
    /// The task was explicitly stopped by its owner.
    Cancelled,
    /// The task has been escalated to another role or human.
    Escalated,
    /// The task has been decomposed into child tasks.
    Split,
    /// The task has been merged into another task.
    Merged,
}

/// A predicate that must hold for the task to be considered done.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneWhenPredicate {
    pub predicate: String,
    pub weight: f64,
}

/// The result of evaluating a completion predicate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionPredicateResult {
    pub predicate: String,
    pub weight: f64,
    pub satisfied: bool,
}

/// A single node in the task DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNode {
    pub id: String,
    pub title: String,
    /// Ids of tasks that must be Done before this task can transition to Done.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Opaque predicate string interpreted by the evaluator.
    pub done_when: String,
    #[serde(default)]
    pub done_when_predicates: Vec<DoneWhenPredicate>,
    /// Current computed state.
    pub state: TaskState,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub estimated_duration: String,
    #[serde(default)]
    pub assigned_role: String,
    #[serde(default)]
    pub escalation_path: String,
    #[serde(default)]
    pub open_questions: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}
