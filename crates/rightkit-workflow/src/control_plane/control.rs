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
