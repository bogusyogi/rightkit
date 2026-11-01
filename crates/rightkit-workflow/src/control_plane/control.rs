//! WP-04 — Deterministic control nodes (Condition / Join / Map / Loop).
//!
//! ADR-0021 constraint: control nodes never spawn work. They only compute
//! "what would run next" given the current deterministic state. All control
//! decisions must be pure / deterministic and replay-stable.

use thiserror::Error;

use crate::control_plane::domain::{CompiledPlan, NodeKind, NodeSpec};
use crate::control_plane::expression::{
    evaluate, parse as parse_expr, EvalContext, ExpressionError,
};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Typed failures from the control resolver.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControlError {
    /// `node_id` not found in the compiled plan.
    #[error("node `{0}` not found in compiled plan")]
    NodeNotFound(String),
    /// `node_id` is not a control node of the expected kind.
    #[error("node `{0}` is not a control node (kind: {1:?})")]
    NotAControlNode(String, NodeKind),
    /// `Map` / `Loop` is missing the required iteration input.
    #[error("missing iteration input `{0}` for control node `{1}`")]
    MissingIterationInput(String, String),
    /// `Loop` is missing the required `max_iterations` input.
    #[error("missing `max_iterations` for loop node `{0}`")]
    MissingLoopBound(String),
    /// Predicate parse failed.
    #[error("predicate parse failed: {0}")]
    PredicateParse(String),
    /// Predicate evaluation failed.
    #[error("predicate evaluation failed: {0}")]
    PredicateFailed(ExpressionError),
    /// `max_iterations` was exceeded by a `Loop`.
    #[error("`max_iterations` exceeded for node `{0}` (max: {1})")]
    IterationBoundExceeded(String, usize),
    /// `Map` / `Loop` cardinality exceeded the hard compile-time cap.
    #[error("cardinality cap exceeded for `{0}`: {1} > {2}")]
    CardinalityExceeded(String, usize, usize),
}

// ─── ControlPlan ─────────────────────────────────────────────────────────────

/// Hard cardinality cap applied to `Map` / `Loop` iteration count.
pub const MAX_ITERATION_CARDINALITY: usize = 4096;

/// Deterministic pre-flight plan for control nodes.
///
/// `ControlPlan` is constructed from a [`NodeKind`] (e.g. [`NodeKind::Condition`])
/// and exposes two pure functions:
///
/// - [`resolve_outgoing`](Self::resolve_outgoing): given a node id and an
///   evaluation context, returns the next node ids (in declaration order)
///   that the control node would emit.
/// - [`check_entry`](Self::check_entry): pre-flight checks for the kind
///   (input bindings, iteration bound) that must hold before the node can
///   be admitted.
///
/// Both functions are pure: no I/O, no clock, no randomness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPlan {
    kind: NodeKind,
}

impl ControlPlan {
    /// Build a plan for `kind`.
    pub fn plan_for(kind: &NodeKind) -> ControlPlan {
        ControlPlan { kind: *kind }
    }

    /// Resolve the outgoing edges for `node_id` under `ctx`. Pure.
    pub fn resolve_outgoing(
        &self,
        compiled: &CompiledPlan,
        node_id: &str,
        ctx: &EvalContext,
    ) -> Result<Vec<String>, ControlError> {
        let node = compiled
            .nodes
            .iter()
            .find(|n| n.id == node_id)
            .ok_or_else(|| ControlError::NodeNotFound(node_id.to_string()))?;
        if node.kind != self.kind {
            return Err(ControlError::NotAControlNode(
                node_id.to_string(),
                node.kind,
            ));
        }
        match node.kind {
            NodeKind::Condition => self.resolve_condition(node, ctx),
            NodeKind::Join => self.resolve_join(compiled, node_id, ctx),
            NodeKind::Map => self.resolve_map(node_id, ctx),
            NodeKind::Loop => self.resolve_loop(node_id, ctx),
            other => Err(ControlError::NotAControlNode(node_id.to_string(), other)),
        }
    }

    /// Pre-flight check that the kind's required inputs are present.
    pub fn check_entry(
        &self,
        compiled: &CompiledPlan,
        ctx: &EvalContext,
    ) -> Result<(), ControlError> {
        match self.kind {
            NodeKind::Condition | NodeKind::Join => {
                // Capability ceiling must be non-empty so that downstream
                // executors can be admitted; the reducer performs the full
                // capability check.
                if compiled.effective_capabilities.as_slice().is_empty() {
                    // Empty is allowed (no capability required) — keep this
                    // branch as a hook for stricter checks in WP-05+.
                }
                Ok(())
            }
            NodeKind::Map => {
                // The Map plan reads `inputs.items` (an array). Bindings and
                // capability ceilings are validated by [`ScopeValidator`] in
                // the bindings module.
                if !ctx.inputs.contains_key("items") {
                    return Err(ControlError::MissingIterationInput(
                        "items".into(),
                        "<map>".into(),
                    ));
                }
                Ok(())
            }
            NodeKind::Loop => {
                if !ctx.inputs.contains_key("max_iterations") {
                    return Err(ControlError::MissingLoopBound("<loop>".into()));
                }
                Ok(())
            }
            other => Err(ControlError::NotAControlNode("<unnamed>".into(), other)),
        }
    }

    fn resolve_condition(
        &self,
        node: &NodeSpec,
        ctx: &EvalContext,
    ) -> Result<Vec<String>, ControlError> {
        let mut fallback: Option<String> = None;
        for edge in &node.edges {
            match &edge.when {
                Some(when) => {
                    let expr = parse_expr(when)
                        .map_err(|e| ControlError::PredicateParse(e.to_string()))?;
                    let v = evaluate(&expr, ctx).map_err(ControlError::PredicateFailed)?;
                    if v == serde_json::Value::Bool(true) {
                        return Ok(vec![edge.target.clone()]);
                    }
                }
                None => {
                    if fallback.is_none() {
                        fallback = Some(edge.target.clone());
                    }
                }
            }
        }
        Ok(fallback.map(|t| vec![t]).unwrap_or_default())
    }

    fn resolve_join(
        &self,
        compiled: &CompiledPlan,
        node_id: &str,
        ctx: &EvalContext,
    ) -> Result<Vec<String>, ControlError> {
        let inbound: Vec<String> = compiled
            .nodes
            .iter()
            .filter(|n| n.edges.iter().any(|e| e.target == node_id))
            .map(|n| n.id.clone())
            .collect();
        let node = compiled
            .nodes
            .iter()
            .find(|n| n.id == node_id)
            .expect("node existence already checked");
        let ready = match &node.config {
            crate::control_plane::domain::NodeConfig::Join { mode } if mode == "any" => {
                inbound.iter().any(|id| ctx.node_outputs.contains_key(id))
            }
            _ => inbound.iter().all(|id| ctx.node_outputs.contains_key(id)),
        };
        if !ready {
            return Ok(vec![]);
        }
        Ok(node.edges.iter().map(|e| e.target.clone()).collect())
    }

    fn resolve_map(&self, node_id: &str, ctx: &EvalContext) -> Result<Vec<String>, ControlError> {
        let items = ctx
            .inputs
            .get("items")
            .ok_or_else(|| ControlError::MissingIterationInput("items".into(), node_id.into()))?;
        let arr = items.as_array().ok_or_else(|| {
            ControlError::MissingIterationInput("items (not array)".into(), node_id.into())
        })?;
        if arr.len() > MAX_ITERATION_CARDINALITY {
            return Err(ControlError::CardinalityExceeded(
                node_id.to_string(),
                arr.len(),
                MAX_ITERATION_CARDINALITY,
            ));
        }
        let max: usize = ctx
            .inputs
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(arr.len());
        let bound = arr.len().min(max);
        Ok((0..bound).map(|i| format!("{node_id}__iter_{i}")).collect())
    }

    fn resolve_loop(&self, node_id: &str, ctx: &EvalContext) -> Result<Vec<String>, ControlError> {
        let max: usize = ctx
            .inputs
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .ok_or_else(|| ControlError::MissingLoopBound(node_id.to_string()))?;
        if max > MAX_ITERATION_CARDINALITY {
            return Err(ControlError::CardinalityExceeded(
                node_id.to_string(),
                max,
                MAX_ITERATION_CARDINALITY,
            ));
        }
        let next_index = match ctx.iteration_index {
            None => 0,
            Some(n) => n + 1,
        };
        if next_index >= max {
            return Ok(vec![]);
        }
        Ok(vec![format!("{node_id}__iter_{next_index}")])
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::domain::{
        BackoffStrategy, Capability, CapabilitySet, EdgeSpec, FailureClass, RetryPolicy,
    };
    use serde_json::json;

    fn empty_caps() -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn node(id: &str, kind: NodeKind, edges: Vec<EdgeSpec>) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind,
            label: None,
            capabilities: empty_caps(),
            edges,
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffStrategy::Fixed { delay_ms: 100 },
                retryable: vec![FailureClass::TransientNetwork],
            },
            config: Default::default(),
        }
    }

    fn edge(target: &str, when: Option<&str>) -> EdgeSpec {
        EdgeSpec {
            target: target.into(),
            when: when.map(|s| s.to_string()),
        }
    }

    fn plan_with(nodes: Vec<NodeSpec>) -> CompiledPlan {
        CompiledPlan {
            entry_node: nodes.first().map(|n| n.id.clone()).unwrap_or_default(),
            nodes,
            effective_capabilities: CapabilitySet::from_iter([Capability::new("noop")]),
        }
    }

    fn ctx_empty() -> EvalContext {
        EvalContext::empty()
    }

    // ── Condition: picks then / else ────────────────────────────────────────

    #[test]
    fn condition_picks_then_branch_when_predicate_is_true() {
        let compiled = plan_with(vec![node(
            "c1",
            NodeKind::Condition,
            vec![
                edge("then_branch", Some("inputs.flag == true")),
                edge("else_branch", None),
            ],
        )]);
        let mut ctx = ctx_empty();
        ctx.inputs.insert("flag".into(), json!(true));
        let plan = ControlPlan::plan_for(&NodeKind::Condition);
        let out = plan
            .resolve_outgoing(&compiled, "c1", &ctx)
            .expect("resolves");
        assert_eq!(out, vec!["then_branch".to_string()]);
    }

    #[test]
    fn condition_picks_else_branch_when_predicate_is_false() {
        let compiled = plan_with(vec![node(
            "c1",
            NodeKind::Condition,
            vec![
                edge("then_branch", Some("inputs.flag == true")),
                edge("else_branch", None),
            ],
        )]);
        let mut ctx = ctx_empty();
        ctx.inputs.insert("flag".into(), json!(false));
        let plan = ControlPlan::plan_for(&NodeKind::Condition);
        let out = plan
            .resolve_outgoing(&compiled, "c1", &ctx)
            .expect("resolves");
        assert_eq!(out, vec!["else_branch".to_string()]);
    }

    #[test]
    fn condition_picks_first_matching_conditional_in_declaration_order() {
        // Two conditional edges; the second matches. The first edge's
        // predicate is false, so the second wins.
        let compiled = plan_with(vec![node(
            "c1",
            NodeKind::Condition,
            vec![
                edge("a", Some("inputs.x == 1")),
                edge("b", Some("inputs.x == 2")),
                edge("c", None),
            ],
        )]);
        let mut ctx = ctx_empty();
        ctx.inputs.insert("x".into(), json!(2));
        let plan = ControlPlan::plan_for(&NodeKind::Condition);
        let out = plan
            .resolve_outgoing(&compiled, "c1", &ctx)
            .expect("resolves");
        assert_eq!(out, vec!["b".to_string()]);
    }

    // ── Join: emits next only when all inbound are completed ────────────────

    #[test]
    fn join_does_not_emit_when_any_inbound_is_missing() {
        let compiled = plan_with(vec![
            node("a", NodeKind::Transform, vec![edge("j", None)]),
            node("b", NodeKind::Transform, vec![edge("j", None)]),
            node("j", NodeKind::Join, vec![edge("down", None)]),
        ]);
        let mut ctx = ctx_empty();
        ctx.node_outputs.insert("a".into(), json!(1));
        // `b` is missing.
        let plan = ControlPlan::plan_for(&NodeKind::Join);
        let out = plan
            .resolve_outgoing(&compiled, "j", &ctx)
            .expect("resolves");
        assert!(out.is_empty());
    }

    #[test]
    fn join_emits_outgoing_only_when_all_inbound_are_present() {
        let compiled = plan_with(vec![
            node("a", NodeKind::Transform, vec![edge("j", None)]),
            node("b", NodeKind::Transform, vec![edge("j", None)]),
            node("j", NodeKind::Join, vec![edge("down", None)]),
        ]);
        let mut ctx = ctx_empty();
        ctx.node_outputs.insert("a".into(), json!(1));
        ctx.node_outputs.insert("b".into(), json!(2));
        let plan = ControlPlan::plan_for(&NodeKind::Join);
        let out = plan
            .resolve_outgoing(&compiled, "j", &ctx)
            .expect("resolves");
        assert_eq!(out, vec!["down".to_string()]);
    }

    // ── Map: emits iteration-indexed child per bound ────────────────────────

    #[test]
    fn map_emits_one_iteration_child_per_input_item() {
        let compiled = plan_with(vec![node("m", NodeKind::Map, vec![])]);
        let mut ctx = ctx_empty();
        ctx.inputs.insert("items".into(), json!(["a", "b", "c"]));
        let plan = ControlPlan::plan_for(&NodeKind::Map);
        let out = plan
            .resolve_outgoing(&compiled, "m", &ctx)
            .expect("resolves");
        assert_eq!(
            out,
            vec![
                "m__iter_0".to_string(),
                "m__iter_1".to_string(),
                "m__iter_2".to_string(),
            ]
        );
    }

    #[test]
    fn map_respects_max_iterations_input() {
        let compiled = plan_with(vec![node("m", NodeKind::Map, vec![])]);
        let mut ctx = ctx_empty();
        ctx.inputs
            .insert("items".into(), json!(["a", "b", "c", "d", "e"]));
        ctx.inputs.insert("max_iterations".into(), json!(2));
        let plan = ControlPlan::plan_for(&NodeKind::Map);
        let out = plan
            .resolve_outgoing(&compiled, "m", &ctx)
            .expect("resolves");
        assert_eq!(out, vec!["m__iter_0".to_string(), "m__iter_1".to_string()]);
    }

    #[test]
    fn map_errors_when_items_is_missing() {
        let compiled = plan_with(vec![node("m", NodeKind::Map, vec![])]);
        let ctx = ctx_empty();
        let plan = ControlPlan::plan_for(&NodeKind::Map);
        let err = plan
            .resolve_outgoing(&compiled, "m", &ctx)
            .expect_err("must error when `items` is missing");
        assert!(matches!(err, ControlError::MissingIterationInput(_, _)));
    }

    // ── Loop: respects max_iterations ───────────────────────────────────────

    #[test]
    fn loop_emits_next_iteration_when_under_max() {
        let compiled = plan_with(vec![node("l", NodeKind::Loop, vec![])]);
        let mut ctx = ctx_empty();
        ctx.inputs.insert("max_iterations".into(), json!(3));
        ctx.iteration_index = Some(1);
        let plan = ControlPlan::plan_for(&NodeKind::Loop);
        let out = plan
            .resolve_outgoing(&compiled, "l", &ctx)
            .expect("resolves");
        assert_eq!(out, vec!["l__iter_2".to_string()]);
    }

    #[test]
    fn loop_emits_iter_zero_when_no_prior_iteration() {
        let compiled = plan_with(vec![node("l", NodeKind::Loop, vec![])]);
        let mut ctx = ctx_empty();
        ctx.inputs.insert("max_iterations".into(), json!(3));
        let plan = ControlPlan::plan_for(&NodeKind::Loop);
        let out = plan
            .resolve_outgoing(&compiled, "l", &ctx)
            .expect("resolves");
        assert_eq!(out, vec!["l__iter_0".to_string()]);
    }

    #[test]
    fn loop_emits_nothing_when_max_reached() {
        let compiled = plan_with(vec![node("l", NodeKind::Loop, vec![])]);
        let mut ctx = ctx_empty();
        ctx.inputs.insert("max_iterations".into(), json!(3));
        ctx.iteration_index = Some(2);
        let plan = ControlPlan::plan_for(&NodeKind::Loop);
        let out = plan
            .resolve_outgoing(&compiled, "l", &ctx)
            .expect("resolves");
        assert!(out.is_empty());
    }

    #[test]
    fn loop_errors_when_max_iterations_is_missing() {
        let compiled = plan_with(vec![node("l", NodeKind::Loop, vec![])]);
        let ctx = ctx_empty();
        let plan = ControlPlan::plan_for(&NodeKind::Loop);
        let err = plan
            .resolve_outgoing(&compiled, "l", &ctx)
            .expect_err("must error when `max_iterations` is missing");
        assert!(matches!(err, ControlError::MissingLoopBound(_)));
    }

    // ── check_entry / kind-mismatch / not-found guards ─────────────────────

    #[test]
    fn check_entry_for_map_requires_items() {
        let compiled = plan_with(vec![node("m", NodeKind::Map, vec![])]);
        let ctx = ctx_empty();
        let plan = ControlPlan::plan_for(&NodeKind::Map);
        let err = plan.check_entry(&compiled, &ctx).expect_err("must error");
        assert!(matches!(err, ControlError::MissingIterationInput(_, _)));
    }

    #[test]
    fn check_entry_for_loop_requires_max_iterations() {
        let compiled = plan_with(vec![node("l", NodeKind::Loop, vec![])]);
        let ctx = ctx_empty();
        let plan = ControlPlan::plan_for(&NodeKind::Loop);
        let err = plan.check_entry(&compiled, &ctx).expect_err("must error");
        assert!(matches!(err, ControlError::MissingLoopBound(_)));
    }

    #[test]
    fn check_entry_for_condition_succeeds() {
        let compiled = plan_with(vec![node("c", NodeKind::Condition, vec![])]);
        let ctx = ctx_empty();
        let plan = ControlPlan::plan_for(&NodeKind::Condition);
        plan.check_entry(&compiled, &ctx)
            .expect("condition always passes");
    }

    #[test]
    fn resolve_outgoing_rejects_wrong_kind() {
        let compiled = plan_with(vec![node("m", NodeKind::Map, vec![])]);
        let ctx = ctx_empty();
        let plan = ControlPlan::plan_for(&NodeKind::Condition);
        let err = plan
            .resolve_outgoing(&compiled, "m", &ctx)
            .expect_err("must reject");
        assert!(matches!(
            err,
            ControlError::NotAControlNode(_, NodeKind::Map)
        ));
    }

    #[test]
    fn resolve_outgoing_rejects_unknown_node() {
        let compiled = plan_with(vec![node("m", NodeKind::Map, vec![])]);
        let ctx = ctx_empty();
        let plan = ControlPlan::plan_for(&NodeKind::Map);
        let err = plan
            .resolve_outgoing(&compiled, "missing", &ctx)
            .expect_err("must reject");
        assert!(matches!(err, ControlError::NodeNotFound(_)));
    }

    #[test]
    fn plan_for_is_pure_and_deterministic() {
        let a = ControlPlan::plan_for(&NodeKind::Condition);
        let b = ControlPlan::plan_for(&NodeKind::Condition);
        assert_eq!(a, b);
    }
}
