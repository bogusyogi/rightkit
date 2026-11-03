//! P3 scaffolds for watchlist primitives — routing, tree search, and skill-based routing.
//! These are minimal working implementations that can be evolved as the info-flow
//! engine develops.

/// Routing mode decision based on plan uncertainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingMode {
    /// High uncertainty: use info-flow routing (dynamic, flexible).
    InfoFlow,
    /// Low uncertainty: use DAG routing (structured, deterministic).
    Dag,
}

/// Routes tasks based on plan uncertainty.
pub struct InfoFlowRouter;

impl InfoFlowRouter {
    /// Decide routing mode based on plan uncertainty (0.0 = certain, 1.0 = completely uncertain).
    /// Threshold: uncertainty >= 0.5 -> InfoFlow, else DAG.
    pub fn route(plan_uncertainty: f64) -> RoutingMode {
        if plan_uncertainty >= 0.5 {
            RoutingMode::InfoFlow
        } else {
            RoutingMode::Dag
        }
    }
}

/// A scored node in a tree search space.
#[derive(Debug, Clone)]
pub struct ScoredNode<T> {
    /// The payload of this node (generic to support various node types).
    pub payload: T,
    /// Score: higher is better (e.g., 0.0-1.0).
    pub score: f64,
    /// Parent node index in the tree, if any.
    pub parent: Option<usize>,
    /// Child indices.
    pub children: Vec<usize>,
}

/// Bounded tree search over scored nodes.
pub struct E2RTreeSearch<T> {
    /// All nodes in the tree, indexed by position.
    nodes: Vec<ScoredNode<T>>,
    /// Maximum number of nodes to expand before stopping.
    max_nodes: usize,
}

impl<T: Clone> E2RTreeSearch<T> {
    /// Create a new search tree with a maximum node budget.
    pub fn new(max_nodes: usize) -> Self {
        Self {
            nodes: Vec::new(),
            max_nodes: max_nodes.max(1),
        }
    }

    /// Expand the tree by adding a root node.
    pub fn add_root(&mut self, payload: T, score: f64) -> usize {
        if self.nodes.len() >= self.max_nodes {
            return 0; // Budget exhausted
        }
        let idx = self.nodes.len();
        self.nodes.push(ScoredNode {
            payload,
            score,
            parent: None,
            children: Vec::new(),
        });
        idx
    }

    /// Expand the tree by adding a child node to an existing parent.
    pub fn add_child(&mut self, parent_idx: usize, payload: T, score: f64) -> Option<usize> {
        if self.nodes.len() >= self.max_nodes || parent_idx >= self.nodes.len() {
            return None;
        }
        let idx = self.nodes.len();
        self.nodes.push(ScoredNode {
            payload,
            score,
            parent: Some(parent_idx),
            children: Vec::new(),
        });
        self.nodes[parent_idx].children.push(idx);
        Some(idx)
    }

    /// Find the highest-scoring leaf node.
    pub fn best_leaf(&self) -> Option<&ScoredNode<T>> {
        self.nodes
            .iter()
            .filter(|n| n.children.is_empty())
            .max_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Reconstruct the path from root to a given node.
    pub fn path_to(&self, mut node_idx: usize) -> Vec<&ScoredNode<T>> {
        let mut path = Vec::new();
        loop {
            if node_idx >= self.nodes.len() {
                break;
            }
            path.push(&self.nodes[node_idx]);
            match self.nodes[node_idx].parent {
                Some(parent_idx) => node_idx = parent_idx,
                None => break,
            }
        }
        path.reverse();
        path
    }

    /// Find the best path from root to leaf.
    pub fn best_path(&self) -> Option<Vec<&ScoredNode<T>>> {
        self.best_leaf().and_then(|leaf| {
            let leaf_idx = self.nodes.iter().position(|n| std::ptr::eq(n, leaf));
            leaf_idx.map(|idx| self.path_to(idx))
        })
    }

    /// Get all nodes in the tree.
    pub fn all_nodes(&self) -> &[ScoredNode<T>] {
        &self.nodes
    }

    /// Get the number of nodes currently in the tree.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// Routes tasks to agents by keyword affinity matching.
pub struct SkillBankRouter;

impl SkillBankRouter {
    /// Match a task to a skill by keyword affinity.
    /// Returns the best-matching skill, or None if no skills match.
    pub fn route_by_skill(task: &str, skills: &[String]) -> Option<String> {
        if skills.is_empty() {
            return None;
        }

        let task_lower = task.to_lowercase();
        let mut best_skill = None;
        let mut best_score = 0.0;

        for skill in skills {
            let skill_lower = skill.to_lowercase();
            // Simple keyword matching: count overlapping words
            let task_words: Vec<&str> = task_lower.split_whitespace().collect();
            let skill_words: Vec<&str> = skill_lower.split_whitespace().collect();

            let matches = task_words
                .iter()
                .filter(|tw| {
                    skill_words
                        .iter()
                        .any(|sw| sw.contains(*tw) || tw.contains(sw))
                })
                .count();

            let score = matches as f64 / skill_words.len().max(1) as f64;

            if score > best_score {
                best_score = score;
                best_skill = Some(skill.clone());
            }
        }

        if best_score > 0.0 {
            best_skill
        } else {
            // Fallback: return first skill
            skills.first().cloned()
        }
    }
}
