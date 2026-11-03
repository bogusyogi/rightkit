//! Per-session task tracker — wraps a `TaskGraph` with session-scoped bookkeeping.

use crate::graph::{TaskGraph, TaskGraphError};
use rightkit_agent_engine_protocol::{TaskNode, TaskState};

/// Tracks the task graph for a single session and provides progress summaries.
pub struct SessionTaskTracker {
    pub session_id: String,
    graph: TaskGraph,
}

impl SessionTaskTracker {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            graph: TaskGraph::new(),
        }
    }

    pub fn graph(&self) -> &TaskGraph {
        &self.graph
    }

    pub fn graph_mut(&mut self) -> &mut TaskGraph {
        &mut self.graph
    }

    /// Replace the whole task set (e.g. after a re-plan). Recomputes blocked state.
    pub fn set_tasks(&mut self, tasks: Vec<TaskNode>) -> Result<(), TaskGraphError> {
        let mut graph = TaskGraph::new();
        for t in tasks {
            graph.add_task(t)?;
        }
        graph.validate()?;
        graph.recompute_states();
        self.graph = graph;
        Ok(())
    }

    pub fn add_task(&mut self, task: TaskNode) -> Result<(), TaskGraphError> {
        self.graph.add_task(task)?;
        self.graph.recompute_states();
        Ok(())
    }

    pub fn mark_done(&mut self, id: &str) -> Result<(), TaskGraphError> {
        self.graph.mark_done(id)
    }

    pub fn set_state(&mut self, id: &str, state: TaskState) -> Result<(), TaskGraphError> {
        let node = self
            .graph
            .get_mut(id)
            .ok_or_else(|| TaskGraphError::NotFound(id.to_string()))?;
        node.state = state;
        self.graph.recompute_states();
        Ok(())
    }

    /// Count of tasks in `Done` state.
    pub fn done_count(&self) -> usize {
        self.graph
            .tasks()
            .filter(|t| t.state == TaskState::Done)
            .count()
    }

    /// Total task count.
    pub fn total_count(&self) -> usize {
        self.graph.len()
    }

    /// Fraction complete in [0.0, 1.0]. Empty graph is 0.0.
    pub fn progress(&self) -> f64 {
        if self.graph.is_empty() {
            return 0.0;
        }
        self.done_count() as f64 / self.total_count() as f64
    }

    /// True when every task is in a terminal-success state.
    pub fn all_done(&self) -> bool {
        !self.graph.is_empty() && self.graph.tasks().all(|t| t.state == TaskState::Done)
    }

    /// Ids of currently actionable tasks.
    pub fn ready_task_ids(&self) -> Vec<String> {
        self.graph
            .ready_tasks()
            .iter()
            .map(|t| t.id.clone())
            .collect()
    }
}
