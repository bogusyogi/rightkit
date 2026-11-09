//! Aho-Corasick keyword graph, ported from Sherpa-ONNX 1.13.4 `context-graph.{h,cc}`.
//!
//! Nodes live in one arena; index 0 is the root. Scores, thresholds and fail/output
//! links are computed exactly as the C++ builds them, including the merge rules for
//! keywords that share a prefix or end on the same node.

use std::collections::{BTreeMap, VecDeque};

pub(crate) const ROOT: usize = 0;

#[derive(Debug, Clone)]
pub(crate) struct ContextState {
    /// Token on the arc into this node; -1 for the root.
    pub token: i32,
    pub token_score: f32,
    pub node_score: f32,
    pub output_score: f32,
    /// Depth: the number of tokens from the root.
    pub level: usize,
    pub ac_threshold: f32,
    pub is_end: bool,
    pub phrase: String,
    next: BTreeMap<i32, usize>,
    fail: usize,
    output: Option<usize>,
}

impl ContextState {
    #[allow(clippy::too_many_arguments)] // mirrors the C++ constructor
    fn new(
        token: i32,
        token_score: f32,
        node_score: f32,
        output_score: f32,
        level: usize,
        ac_threshold: f32,
        is_end: bool,
        phrase: String,
    ) -> Self {
        Self {
            token,
            token_score,
            node_score,
            output_score,
            level,
            ac_threshold,
            is_end,
            phrase,
            next: BTreeMap::new(),
            fail: ROOT,
            output: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ContextGraph {
    nodes: Vec<ContextState>,
}

impl ContextGraph {
    /// `scores`, `phrases` and `ac_thresholds` are either empty or one per keyword; a
    /// zero score or threshold falls back to `context_score` / `ac_threshold`.
    pub(crate) fn new(
        token_ids: &[Vec<i32>],
        context_score: f32,
        ac_threshold: f32,
        scores: &[f32],
        phrases: &[String],
        ac_thresholds: &[f32],
    ) -> Self {
        let mut graph = Self {
            nodes: vec![ContextState::new(-1, 0.0, 0.0, 0.0, 0, 0.0, false, String::new())],
        };
        graph.build(
            token_ids,
            context_score,
            ac_threshold,
            scores,
            phrases,
            ac_thresholds,
        );
        graph.fill_fail_output();
        graph
    }

    pub(crate) fn state(&self, index: usize) -> &ContextState {
        &self.nodes[index]
    }

    /// Tokens that advance `state` along a keyword (its direct arcs; fail transitions are
    /// not followed). Trace only.
    pub(crate) fn extension_tokens(&self, state: usize) -> impl Iterator<Item = i32> + '_ {
        self.nodes[state].next.keys().copied()
    }

    fn build(
        &mut self,
        token_ids: &[Vec<i32>],
        context_score: f32,
        default_threshold: f32,
        scores: &[f32],
        phrases: &[String],
        ac_thresholds: &[f32],
    ) {
        for (i, ids) in token_ids.iter().enumerate() {
            let mut node = ROOT;
            let score = scores.get(i).copied().unwrap_or(0.0);
            let score = if score == 0.0 { context_score } else { score };
            let threshold = ac_thresholds.get(i).copied().unwrap_or(0.0);
            let threshold = if threshold == 0.0 {
                default_threshold
            } else {
                threshold
            };
            let phrase = phrases.get(i).cloned().unwrap_or_default();
            for (j, &token) in ids.iter().enumerate() {
                let last = j + 1 == ids.len();
                let parent_score = self.nodes[node].node_score;
                match self.nodes[node].next.get(&token).copied() {
                    None => {
                        let child = self.nodes.len();
                        self.nodes.push(ContextState::new(
                            token,
                            score,
                            parent_score + score,
                            if last { parent_score + score } else { 0.0 },
                            j + 1,
                            if last { threshold } else { 0.0 },
                            last,
                            if last { phrase.clone() } else { String::new() },
                        ));
                        self.nodes[node].next.insert(token, child);
                        node = child;
                    }
                    Some(child) => {
                        let c = &mut self.nodes[child];
                        let token_score = score.max(c.token_score);
                        c.token_score = token_score;
                        let node_score = parent_score + token_score;
                        c.node_score = node_score;
                        let is_end = last || c.is_end;
                        c.output_score = if is_end { node_score } else { 0.0 };
                        c.is_end = is_end;
                        if last {
                            c.phrase = phrase.clone();
                            c.ac_threshold = threshold;
                        }
                        node = child;
                    }
                }
            }
        }
    }

    /// Follow fail links from `start` until a node with an arc on `token`, stopping at
    /// the root. Returns the node reached by that arc, or the node where the walk stopped.
    fn fail_walk(&self, start: usize, token: i32) -> usize {
        let mut node = start;
        while !self.nodes[node].next.contains_key(&token) {
            node = self.nodes[node].fail;
            if self.nodes[node].token == -1 {
                break;
            }
        }
        self.nodes[node].next.get(&token).copied().unwrap_or(node)
    }

    fn fill_fail_output(&mut self) {
        let mut queue = VecDeque::new();
        let first: Vec<usize> = self.nodes[ROOT].next.values().copied().collect();
        for child in first {
            self.nodes[child].fail = ROOT;
            queue.push_back(child);
        }
        while let Some(current) = queue.pop_front() {
            let children: Vec<(i32, usize)> = self.nodes[current]
                .next
                .iter()
                .map(|(t, c)| (*t, *c))
                .collect();
            for (token, child) in children {
                let parent_fail = self.nodes[current].fail;
                let fail = match self.nodes[parent_fail].next.get(&token) {
                    Some(&f) => f,
                    None => self.fail_walk(self.nodes[parent_fail].fail, token),
                };
                self.nodes[child].fail = fail;
                let mut output = Some(fail);
                while let Some(o) = output {
                    if self.nodes[o].is_end {
                        break;
                    }
                    let next = self.nodes[o].fail;
                    output = (self.nodes[next].token != -1).then_some(next);
                }
                self.nodes[child].output = output;
                if let Some(o) = output {
                    let add = self.nodes[o].output_score;
                    self.nodes[child].output_score += add;
                }
                queue.push_back(child);
            }
        }
    }

    /// `ForwardOneStep` in strict mode: the score delta and the next state.
    pub(crate) fn forward_one_step(&self, state: usize, token: i32) -> (f32, usize) {
        let current = &self.nodes[state];
        let (node, score) = match current.next.get(&token) {
            Some(&n) => (n, self.nodes[n].token_score),
            None => {
                let n = self.fail_walk(current.fail, token);
                (n, self.nodes[n].node_score - current.node_score)
            }
        };
        (score + self.nodes[node].output_score, node)
    }

    /// `IsMatched`: the keyword end reached at `state`, directly or through its output link.
    pub(crate) fn matched(&self, state: usize) -> Option<usize> {
        let s = &self.nodes[state];
        if s.is_end {
            Some(state)
        } else {
            s.output
        }
    }
}
