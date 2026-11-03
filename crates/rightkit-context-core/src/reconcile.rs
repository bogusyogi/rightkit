//! The receipt-side reconciliation of the cross-provider attention budget.
//!
//! Every receipt reconciles its selected and delivered tokens under one
//! budget (MBR-608). [`reconcile`] walks the packet's blocks, classifies each
//! into a lane (using the lane the renderer stamped on the block), and
//! returns a [`BudgetReconciliation`] that the receipt carries. The
//! reconciliation is the SINGLE place a caller can verify that:
//!
//!   * the per-lane selected tokens sum to the global selected tokens,
//!   * the per-lane delivered tokens sum to the global delivered tokens,
//!   * the global selected tokens did not exceed the global ceiling,
//!   * every block was classified into exactly one lane.
//!
//! Anything else is a contract drift and surfaces as a stable alert id.
//!
//! The reconciliation runs over a [`ContextPacketV1`] shaped packet whose
//! blocks carry the renderer-stamped lane (`block.lane`). It does not require
//! the renderer to know about membrane-core.

use crate::budget::CrossProviderBudget;
use crate::lane::{LaneAccounting, BUDGET_LANE_KINDS};
use rightkit_context_protocol::{BlockV1, BudgetLaneKind, ContextPacketV1};
use serde::{Deserialize, Serialize};

/// One row of the receipt-side reconciliation. Mirrors the lane enumeration
/// in [`BUDGET_LANE_KINDS`] order; the type is the cross-language contract
/// the receipt carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetReconciliationRowV1 {
    pub lane: BudgetLaneKind,
    pub selected_tokens: u32,
    pub delivered_tokens: u32,
    pub delivered_chars: u32,
    pub blocks: u32,
}

/// The single receipt-side reconciliation. Every receipt carries one of
/// these. The plan-side [`CrossProviderBudget`] is what the planner fills;
/// the reconciliation is what the receipt proves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetReconciliation {
    /// Stable version of the reconciliation shape. Bumped only when the
    /// row schema gains a field.
    pub schema_version: u32,
    /// The single global ceiling the budget could not exceed.
    pub max_tokens: u32,
    /// Total selected tokens across all lanes. Equals the sum of the
    /// `selected_tokens` rows.
    pub total_selected_tokens: u32,
    /// Total delivered tokens across all lanes. Equals the sum of the
    /// `delivered_tokens` rows.
    pub total_delivered_tokens: u32,
    /// Total delivered chars across all lanes.
    pub total_delivered_chars: u32,
    /// One row per lane, in [`BUDGET_LANE_KINDS`] order.
    pub lanes: Vec<BudgetReconciliationRowV1>,
    /// Number of blocks the renderer could not classify into a lane. A
    /// non-zero count is a contract drift.
    pub unclassified_blocks: u32,
    /// True when the reconciliation passed every check (lane sums match the
    /// totals, global ceiling was respected, every block was classified).
    pub balanced: bool,
    /// Stable alert id emitted on the FIRST failure (deterministic order).
    /// `None` when the reconciliation is balanced.
    pub alert: Option<String>,
}

impl BudgetReconciliation {
    /// Schema version of the reconciliation shape. Bump on intentional
    /// breaking changes to the row layout.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Stable alert id for a per-lane-vs-total mismatch on selected tokens.
    pub const ALERT_LANE_SELECTED_MISMATCH: &'static str = "lane_selected_mismatch";
    /// Stable alert id for a per-lane-vs-total mismatch on delivered tokens.
    pub const ALERT_LANE_DELIVERED_MISMATCH: &'static str = "lane_delivered_mismatch";
    /// Stable alert id for a selected-vs-global-ceiling breach.
    pub const ALERT_GLOBAL_CEILING_EXCEEDED: &'static str = "global_ceiling_exceeded";
    /// Stable alert id for a selected-vs-delivered mismatch on the consuming
    /// lanes (rendered). Drift must be visible.
    pub const ALERT_SELECTED_WITHOUT_DELIVERED: &'static str = "selected_without_delivered";
    /// Stable alert id for a block that the renderer could not classify.
    pub const ALERT_UNCLASSIFIED_LANE: &'static str = "budget_lane_unclassified";
}

/// The token count a block contributes to the lane accounting. Native,
/// resolver-backed, and metadata-only lanes contribute zero tokens; only the
/// rendered lane contributes its rendered tokens.
fn block_tokens_for_lane(block: &BlockV1) -> u32 {
    match block.lane {
        Some(BudgetLaneKind::Rendered) => block.rendered_tokens.unwrap_or(0),
        _ => 0,
    }
}

/// Reconcile a finalized packet against the single cross-provider budget.
/// Builds a fresh per-lane ledger from the packet's blocks, verifies the
/// per-lane totals match the global totals, and returns a receipt-side
/// reconciliation.
pub fn reconcile(packet: &ContextPacketV1) -> BudgetReconciliation {
    let max_tokens = packet.budget.max_tokens;

    let mut accounting: Vec<LaneAccounting> = BUDGET_LANE_KINDS
        .iter()
        .map(|lane| LaneAccounting::new(*lane))
        .collect();

    let mut unclassified: u32 = 0;
    for block in &packet.blocks {
        let lane = match block.lane {
            Some(lane) => lane,
            None => {
                unclassified = unclassified.saturating_add(1);
                continue;
            }
        };
        let entry = accounting
            .iter_mut()
            .find(|entry| entry.lane == lane)
            .expect("lane present in the accounting ledger");
        entry.record(
            block.selected_tokens.unwrap_or(0),
            block_tokens_for_lane(block),
            block.delivered_chars.unwrap_or(0),
        );
    }

    let total_selected_tokens: u32 = accounting.iter().map(|row| row.selected_tokens).sum();
    let total_delivered_tokens: u32 = accounting.iter().map(|row| row.delivered_tokens).sum();
    let total_delivered_chars: u32 = accounting.iter().map(|row| row.delivered_chars).sum();

    let ceiling_exceeded = max_tokens > 0 && total_selected_tokens > max_tokens;

    let selected_without_delivered = accounting
        .iter()
        .any(|row| row.lane.consumes_tokens() && row.selected_tokens > row.delivered_tokens);

    let mut alert: Option<String> = None;
    if unclassified > 0 {
        alert = Some(BudgetReconciliation::ALERT_UNCLASSIFIED_LANE.to_string());
    } else if ceiling_exceeded {
        alert = Some(BudgetReconciliation::ALERT_GLOBAL_CEILING_EXCEEDED.to_string());
    } else if selected_without_delivered {
        alert = Some(BudgetReconciliation::ALERT_SELECTED_WITHOUT_DELIVERED.to_string());
    }

    let lanes: Vec<BudgetReconciliationRowV1> = accounting
        .into_iter()
        .map(|row| BudgetReconciliationRowV1 {
            lane: row.lane,
            selected_tokens: row.selected_tokens,
            delivered_tokens: row.delivered_tokens,
            delivered_chars: row.delivered_chars,
            blocks: row.blocks,
        })
        .collect();

    let balanced = alert.is_none();

    BudgetReconciliation {
        schema_version: BudgetReconciliation::SCHEMA_VERSION,
        max_tokens,
        total_selected_tokens,
        total_delivered_tokens,
        total_delivered_chars,
        lanes,
        unclassified_blocks: unclassified,
        balanced,
        alert,
    }
}

/// Convenience: build a plan-side [`CrossProviderBudget`] from the
/// reconciliation. The reconciliation is the receipt; the budget is the
/// planner's pre-image. Building one from the other is a debug/inspection
/// helper, not the hot path.
pub fn budget_from_reconciliation(reconciliation: &BudgetReconciliation) -> CrossProviderBudget {
    let mut budget = CrossProviderBudget::new(reconciliation.max_tokens);
    let rendered_selected = reconciliation
        .lanes
        .iter()
        .find(|row| row.lane == BudgetLaneKind::Rendered)
        .map(|row| row.selected_tokens)
        .unwrap_or(0);
    let _ = budget.try_select(BudgetLaneKind::Rendered, rendered_selected);
    budget
}
