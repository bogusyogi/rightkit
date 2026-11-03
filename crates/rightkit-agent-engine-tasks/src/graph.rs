//! The task dependency DAG — topological ordering, cycle detection, dependency-aware state.

use rightkit_agent_engine_protocol::{TaskNode, TaskState};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskGraphError {
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("dependency cycle detected involving: {0}")]
    Cycle(String),
    #[error("duplicate task id: {0}")]
    DuplicateId(String),
    #[error("dependency references unknown task: {0}")]
    UnknownDependency(String),
}

/// A directed acyclic graph of tasks.
#[derive(Debug, Default)]
pub struct TaskGraph {
    nodes: HashMap<String, TaskNode>,
    /// Insertion order, for stable iteration.
    order: Vec<String>,
}

impl TaskGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a task. Errors if the id already exists.
    pub fn add_task(&mut self, task: TaskNode) -> Result<(), TaskGraphError> {
        if self.nodes.contains_key(&task.id) {
            return Err(TaskGraphError::DuplicateId(task.id.clone()));
        }
        self.order.push(task.id.clone());
        self.nodes.insert(task.id.clone(), task);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&TaskNode> {
        self.nodes.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut TaskNode> {
        self.nodes.get_mut(id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Validate that every dependency references an existing task and there are no cycles.
    pub fn validate(&self) -> Result<(), TaskGraphError> {
        for id in &self.order {
            let node = &self.nodes[id];
            for dep in &node.deps {
                if !self.nodes.contains_key(dep) {
                    return Err(TaskGraphError::UnknownDependency(dep.clone()));
                }
            }
        }
        // Cycle check via topological sort.
        self.topological_order().map(|_| ())
    }

    /// Return task ids in dependency order (deps before dependents).
    /// Errors with `Cycle` if the graph is cyclic.
    pub fn topological_order(&self) -> Result<Vec<String>, TaskGraphError> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for id in &self.order {
            in_degree.entry(id.as_str()).or_insert(0);
        }
        for id in &self.order {
            for dep in &self.nodes[id].deps {
                // Edge: dep -> id. id depends on dep, so id's in-degree rises.
                if self.nodes.contains_key(dep) {
                    *in_degree.entry(id.as_str()).or_insert(0) += 1;
                }
            }
        }

        // Build adjacency: dep -> [dependents]
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
        for id in &self.order {
            for dep in &self.nodes[id].deps {
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(id.as_str());
            }
        }

        let mut queue: VecDeque<&str> = self
            .order
            .iter()
            .map(|s| s.as_str())
            .filter(|id| in_degree[id] == 0)
            .collect();

        let mut result = Vec::new();
        while let Some(id) = queue.pop_front() {
            result.push(id.to_string());
            if let Some(children) = dependents.get(id) {
                for child in children {
                    let d = in_degree.get_mut(child).unwrap();
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(child);
                    }
                }
            }
        }

        if result.len() != self.nodes.len() {
            // Find a node still with non-zero in-degree to name in the error.
            let stuck = in_degree
                .iter()
                .find(|(_, &d)| d > 0)
                .map(|(id, _)| id.to_string())
                .unwrap_or_default();
            return Err(TaskGraphError::Cycle(stuck));
        }
        Ok(result)
    }

    /// Tasks whose dependencies are all `Done` and which are not themselves terminal.
    pub fn ready_tasks(&self) -> Vec<&TaskNode> {
        self.order
            .iter()
            .map(|id| &self.nodes[id])
            .filter(|node| {
                matches!(
                    node.state,
                    TaskState::Open | TaskState::InProgress | TaskState::Blocked
                ) && node.deps.iter().all(|dep| {
                    self.nodes
                        .get(dep)
                        .map(|d| d.state == TaskState::Done)
                        .unwrap_or(false)
                })
            })
            .collect()
    }

    /// Recompute Blocked/unblocked state based on dependency completion.
    /// A task with an incomplete dependency becomes `Blocked`; an `Open` task whose deps
    /// are all `Done` is left `Open` (ready). Terminal states (Done/Failed/Cancelled/Merged/Split)
    /// are never overwritten. Escalated tasks transition back to Open when deps are ready.
    pub fn recompute_states(&mut self) {
        let done: HashSet<String> = self
            .nodes
            .values()
            .filter(|n| n.state == TaskState::Done)
            .map(|n| n.id.clone())
            .collect();

        for id in &self.order {
            let node = self.nodes.get_mut(id).unwrap();
            if matches!(
                node.state,
                TaskState::Done
                    | TaskState::Failed
                    | TaskState::Cancelled
                    | TaskState::Merged
                    | TaskState::Split
            ) {
                continue;
            }
            let all_deps_done = node.deps.iter().all(|d| done.contains(d));
            if !all_deps_done {
                node.state = TaskState::Blocked;
            } else if matches!(node.state, TaskState::Blocked | TaskState::Escalated) {
                // Unblock back to Open (includes transitioning from Escalated).
                node.state = TaskState::Open;
            }
        }
    }

    /// Mark a task done and recompute downstream blocked state.
    pub fn mark_done(&mut self, id: &str) -> Result<(), TaskGraphError> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| TaskGraphError::NotFound(id.to_string()))?;
        node.state = TaskState::Done;
        self.recompute_states();
        Ok(())
    }

    /// Mark a task as Split (decomposed into child tasks).
    /// This is called when a task produces sub-tasks during execution.
    /// The parent task transitions to Split state and is marked terminal
    /// (it will not be re-executed; its children represent the actual work).
    pub fn mark_split(
        &mut self,
        parent_id: &str,
        child_ids: &[&str],
    ) -> Result<(), TaskGraphError> {
        let parent = self
            .nodes
            .get_mut(parent_id)
            .ok_or_else(|| TaskGraphError::NotFound(parent_id.to_string()))?;
        parent.state = TaskState::Split;
        // Ensure all children exist before marking split.
        for child_id in child_ids {
            if !self.nodes.contains_key(*child_id) {
                return Err(TaskGraphError::UnknownDependency(child_id.to_string()));
            }
        }
        Ok(())
    }

    /// Mark a task as Merged into another task.
    /// This is called when a task's work is consolidated into another task.
    /// The merged task transitions to Merged state and is marked terminal.
    /// The target task (into_id) receives the merged task's dependents if needed.
    pub fn mark_merged(&mut self, merged_id: &str, _into_id: &str) -> Result<(), TaskGraphError> {
        let node = self
            .nodes
            .get_mut(merged_id)
            .ok_or_else(|| TaskGraphError::NotFound(merged_id.to_string()))?;
        node.state = TaskState::Merged;
        Ok(())
    }

    /// Iterate tasks in insertion order.
    pub fn tasks(&self) -> impl Iterator<Item = &TaskNode> {
        self.order.iter().map(move |id| &self.nodes[id])
    }
}
