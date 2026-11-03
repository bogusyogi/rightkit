//! Cross-provider budget lanes.
//!
//! Every Membrane block is admitted under exactly one of four explicit lanes
//! ([`rightkit_context_protocol::BudgetLaneKind`]). The lane is the contract surface
//! that reconciles selected tokens, delivered tokens, and delivered chars
//! across providers. A provider may emit blocks in any subset of lanes; the
//! lane is about how the block is delivered, not where it came from.
//!
//! Lane → delivery surface mapping (the contract):
//!
//!   * [`BudgetLaneKind::Native`]         — host loaded the content with a
//!     matching host receipt (MBR-010).
//!     Zero bytes; zero tokens.
//!   * [`BudgetLaneKind::Rendered`]       — the renderer serialized inline
//!     text. `renderedTokens` is the
//!     token count consumed.
//!   * [`BudgetLaneKind::ResolverBacked`] — only a resolver reference, the
//!     agent retrieves on demand. Zero
//!     tokens consumed; `deliveredChars`
//!     tracks the reference header.
//!   * [`BudgetLaneKind::MetadataOnly`]   — metadata without content. Zero
//!     tokens; zero bytes.
//!
//! The lanes are mutually exclusive per block. The cross-provider budget
//! totals are the sum of the per-lane totals — never a separate counter.

use rightkit_context_protocol::BudgetLaneKind;
use serde::{Deserialize, Serialize};

// Re-export the protocol lane enum so downstream call sites can keep
// importing `rightkit_context_core::BudgetLaneKind` without chasing the protocol.
pub use rightkit_context_protocol::BudgetLaneKind as LaneKind;

/// The fixed, ordered set of lanes. The order is the contract order; both
/// the Rust and TypeScript reconciliations iterate this exact sequence so the
/// receipts never reorder.
pub const BUDGET_LANE_KINDS: &[BudgetLaneKind] = &[
    BudgetLaneKind::Native,
    BudgetLaneKind::Rendered,
    BudgetLaneKind::ResolverBacked,
    BudgetLaneKind::MetadataOnly,
];

/// Per-lane allocation — the planner's intended cap for a lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaneAllocation {
    pub lane: BudgetLaneKind,
    /// Maximum tokens this lane may consume. Zero for lanes that never
    /// consume tokens (native, resolver_backed, metadata_only).
    pub max_tokens: u32,
    /// Tokens the planner actually selected into this lane.
    pub selected_tokens: u32,
}

impl LaneAllocation {
    pub fn new(lane: BudgetLaneKind, max_tokens: u32) -> Self {
        Self {
            lane,
            max_tokens: if lane.consumes_tokens() {
                max_tokens
            } else {
                0
            },
            selected_tokens: 0,
        }
    }

    /// Try to reserve `tokens` against this lane. Returns `true` and adds to
    /// `selected_tokens` when the lane has capacity, `false` otherwise.
    pub fn try_select(&mut self, tokens: u32) -> bool {
        if !self.lane.consumes_tokens() {
            // Non-consuming lanes always accept; selection is recorded on the
            // accounting side, not here.
            return true;
        }
        let proposed = self.selected_tokens.saturating_add(tokens);
        if proposed > self.max_tokens {
            return false;
        }
        self.selected_tokens = proposed;
        true
    }
}

/// Per-lane accounting — the receipt-side observation of the lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaneAccounting {
    pub lane: BudgetLaneKind,
    /// Tokens the planner selected for this lane.
    pub selected_tokens: u32,
    /// Tokens the renderer/delivery actually emitted for this lane.
    pub delivered_tokens: u32,
    /// Unicode code points (chars) carried in this lane across all blocks.
    pub delivered_chars: u32,
    /// Number of blocks that occupied this lane.
    pub blocks: u32,
}

impl LaneAccounting {
    pub fn new(lane: BudgetLaneKind) -> Self {
        Self {
            lane,
            selected_tokens: 0,
            delivered_tokens: 0,
            delivered_chars: 0,
            blocks: 0,
        }
    }

    /// Record one block against this lane.
    pub fn record(&mut self, selected_tokens: u32, delivered_tokens: u32, delivered_chars: u32) {
        self.selected_tokens = self.selected_tokens.saturating_add(selected_tokens);
        self.delivered_tokens = self.delivered_tokens.saturating_add(delivered_tokens);
        self.delivered_chars = self.delivered_chars.saturating_add(delivered_chars);
        self.blocks = self.blocks.saturating_add(1);
    }

    /// True when the lane's selected tokens were all delivered. Native,
    /// resolver-backed, and metadata-only lanes always satisfy this (their
    /// selected tokens are zero by construction).
    pub fn reconciled(&self) -> bool {
        if self.lane.consumes_tokens() {
            self.selected_tokens == self.delivered_tokens
        } else {
            // Zero-token lanes: any non-zero selected figure is a contract
            // violation; otherwise the lane is balanced by construction.
            self.selected_tokens == 0
        }
    }
}

/// Derive the lane from a block's `deliveryClass` and `deliveryMode`. The
/// mapping is the MBR-608 contract; both the Rust and TypeScript
/// implementations must produce identical results.
pub fn lane_from_block(
    delivery_class: Option<&str>,
    delivery_mode: Option<&str>,
) -> Result<BudgetLaneKind, String> {
    // Native delivery is identified by the MBR-010 receipt verification, not
    // by deliveryClass. A block whose deliveryMode is "native" is always in
    // the native lane, regardless of its deliveryClass.
    if delivery_mode == Some("native") {
        return Ok(BudgetLaneKind::Native);
    }
    match delivery_class {
        Some("rendered") => Ok(BudgetLaneKind::Rendered),
        Some("metadata_only") => Ok(BudgetLaneKind::MetadataOnly),
        Some("resolver_backed") => Ok(BudgetLaneKind::ResolverBacked),
        Some(other) => Err(other.to_string()),
        None => Err("missing_delivery_class".to_string()),
    }
}
