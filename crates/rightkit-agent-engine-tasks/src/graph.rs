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

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, deps: &[&str], state: TaskState) -> TaskNode {
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
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn add_and_get() {
        let mut g = TaskGraph::new();
        g.add_task(task("a", &[], TaskState::Open)).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g.get("a").unwrap().id, "a");
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut g = TaskGraph::new();
        g.add_task(task("a", &[], TaskState::Open)).unwrap();
        assert_eq!(
            g.add_task(task("a", &[], TaskState::Open)),
            Err(TaskGraphError::DuplicateId("a".into()))
        );
    }

    #[test]
    fn topological_order_respects_deps() {
        let mut g = TaskGraph::new();
        g.add_task(task("c", &["b"], TaskState::Open)).unwrap();
        g.add_task(task("b", &["a"], TaskState::Open)).unwrap();
        g.add_task(task("a", &[], TaskState::Open)).unwrap();
        let order = g.topological_order().unwrap();
        let pos = |x: &str| order.iter().position(|i| i == x).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("b") < pos("c"));
    }

    #[test]
    fn cycle_detected() {
        let mut g = TaskGraph::new();
        g.add_task(task("a", &["b"], TaskState::Open)).unwrap();
        g.add_task(task("b", &["a"], TaskState::Open)).unwrap();
        assert!(matches!(
            g.topological_order(),
            Err(TaskGraphError::Cycle(_))
        ));
    }

    #[test]
    fn unknown_dependency_fails_validation() {
        let mut g = TaskGraph::new();
        g.add_task(task("a", &["ghost"], TaskState::Open)).unwrap();
        assert_eq!(
            g.validate(),
            Err(TaskGraphError::UnknownDependency("ghost".into()))
        );
    }

    #[test]
    fn ready_tasks_only_unblocked() {
        let mut g = TaskGraph::new();
        g.add_task(task("a", &[], TaskState::Done)).unwrap();
        g.add_task(task("b", &["a"], TaskState::Open)).unwrap();
        g.add_task(task("c", &["b"], TaskState::Open)).unwrap();
        let ready: Vec<_> = g.ready_tasks().iter().map(|t| t.id.clone()).collect();
        assert_eq!(ready, vec!["b"]); // a is done, c is still blocked by b
    }

    #[test]
    fn recompute_marks_blocked() {
        let mut g = TaskGraph::new();
        g.add_task(task("a", &[], TaskState::Open)).unwrap();
        g.add_task(task("b", &["a"], TaskState::Open)).unwrap();
        g.recompute_states();
        assert_eq!(g.get("b").unwrap().state, TaskState::Blocked);
    }

    #[test]
    fn mark_done_unblocks_dependents() {
        let mut g = TaskGraph::new();
        g.add_task(task("a", &[], TaskState::Open)).unwrap();
        g.add_task(task("b", &["a"], TaskState::Open)).unwrap();
        g.recompute_states();
        assert_eq!(g.get("b").unwrap().state, TaskState::Blocked);
        g.mark_done("a").unwrap();
        assert_eq!(g.get("b").unwrap().state, TaskState::Open);
    }

    #[test]
    fn mark_done_unknown_errors() {
        let mut g = TaskGraph::new();
        assert_eq!(g.mark_done("x"), Err(TaskGraphError::NotFound("x".into())));
    }

    #[test]
    fn empty_graph_topo_is_empty() {
        let g = TaskGraph::new();
        assert!(g.topological_order().unwrap().is_empty());
    }

    #[test]
    fn mark_split_transitions_state() {
        let mut g = TaskGraph::new();
        g.add_task(task("parent", &[], TaskState::InProgress))
            .unwrap();
        g.add_task(task("child1", &[], TaskState::Open)).unwrap();
        g.add_task(task("child2", &[], TaskState::Open)).unwrap();
        g.mark_split("parent", &["child1", "child2"]).unwrap();
        assert_eq!(g.get("parent").unwrap().state, TaskState::Split);
    }

    #[test]
    fn mark_split_unknown_parent_errors() {
        let mut g = TaskGraph::new();
        g.add_task(task("child", &[], TaskState::Open)).unwrap();
        assert_eq!(
            g.mark_split("ghost", &["child"]),
            Err(TaskGraphError::NotFound("ghost".into()))
        );
    }

    #[test]
    fn mark_split_unknown_child_errors() {
        let mut g = TaskGraph::new();
        g.add_task(task("parent", &[], TaskState::Open)).unwrap();
        g.add_task(task("child1", &[], TaskState::Open)).unwrap();
        assert_eq!(
            g.mark_split("parent", &["child1", "ghost"]),
            Err(TaskGraphError::UnknownDependency("ghost".into()))
        );
    }

    #[test]
    fn mark_merged_transitions_state() {
        let mut g = TaskGraph::new();
        g.add_task(task("task_a", &[], TaskState::InProgress))
            .unwrap();
        g.add_task(task("task_b", &[], TaskState::Open)).unwrap();
        g.mark_merged("task_a", "task_b").unwrap();
        assert_eq!(g.get("task_a").unwrap().state, TaskState::Merged);
    }

    #[test]
    fn mark_merged_unknown_task_errors() {
        let mut g = TaskGraph::new();
        g.add_task(task("task_b", &[], TaskState::Open)).unwrap();
        assert_eq!(
            g.mark_merged("ghost", "task_b"),
            Err(TaskGraphError::NotFound("ghost".into()))
        );
    }

    #[test]
    fn recompute_preserves_split_state() {
        let mut g = TaskGraph::new();
        g.add_task(task("parent", &[], TaskState::Split)).unwrap();
        g.add_task(task("child", &["parent"], TaskState::Open))
            .unwrap();
        g.recompute_states();
        assert_eq!(g.get("parent").unwrap().state, TaskState::Split);
        // Child should not be blocked by Split parent (Split is terminal).
        assert_eq!(g.get("child").unwrap().state, TaskState::Blocked);
    }

    #[test]
    fn recompute_preserves_merged_state() {
        let mut g = TaskGraph::new();
        g.add_task(task("task_a", &[], TaskState::Merged)).unwrap();
        g.add_task(task("task_b", &["task_a"], TaskState::Open))
            .unwrap();
        g.recompute_states();
        assert_eq!(g.get("task_a").unwrap().state, TaskState::Merged);
        // Merged is terminal like Done, so task_b remains Blocked (dependencies not Done).
        assert_eq!(g.get("task_b").unwrap().state, TaskState::Blocked);
    }
}
