//! Prompt-cache discipline. Ported from CodeRight `providers::prompt_cache`
//! (breakpoint planner + cache-token ledger).
//!
//! Rules enforced for Anthropic `cache_control`:
//! 1. At most [`MAX_BREAKPOINTS`] markers per request.
//! 2. The stable prefix order is tools, then system, then messages; markers go on
//!    the LAST block of each stable section so prefix lookups hit.
//! 3. Remaining slots go to the last large stable message blocks; volatile
//!    blocks and blocks under [`MIN_BREAKPOINT_TOKENS`] are never marked.
//! 4. Request JSON is built deterministically so identical prefixes serialize
//!    byte-identically across turns.

use crate::types::Usage;

pub const MAX_BREAKPOINTS: usize = 4;
pub const MIN_BREAKPOINT_TOKENS: u32 = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheableBlock {
    pub id: String,
    pub approx_tokens: u32,
    pub volatile: bool,
}

/// Rough token estimate (~4 bytes/token); only used to rank breakpoint candidates.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32).div_ceil(4)
}

/// Indices of blocks that get a breakpoint, ascending: the most recent stable,
/// sufficiently large blocks, up to `max`.
pub fn plan_breakpoints(blocks: &[CacheableBlock], max: usize) -> Vec<usize> {
    let cap = max.min(MAX_BREAKPOINTS);
    let mut chosen: Vec<usize> = Vec::new();
    for (i, b) in blocks.iter().enumerate().rev() {
        if chosen.len() >= cap {
            break;
        }
        if b.volatile || b.approx_tokens < MIN_BREAKPOINT_TOKENS {
            continue;
        }
        chosen.push(i);
    }
    chosen.sort_unstable();
    chosen
}

/// Session-scoped planner: keeps anchors stable while history only appends and
/// replans when topology changes, so warm cache entries are not invalidated by
/// moving a marker needlessly.
#[derive(Debug, Clone, Default)]
pub struct CacheBreakpointPlanner {
    max: usize,
    ids: Vec<String>,
    plan: Vec<usize>,
}

impl CacheBreakpointPlanner {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            ..Self::default()
        }
    }

    pub fn plan_for(&mut self, blocks: &[CacheableBlock]) -> &[usize] {
        let topology_changed = self.ids.len() > blocks.len()
            || self.ids.iter().zip(blocks).any(|(k, c)| k != &c.id)
            || (self.ids.is_empty() && !blocks.is_empty());
        let stale = self.plan.iter().any(|&i| match blocks.get(i) {
            Some(b) => b.volatile || b.approx_tokens < MIN_BREAKPOINT_TOKENS,
            None => true,
        });
        if topology_changed || stale {
            self.plan = plan_breakpoints(blocks, self.max);
        }
        self.ids = blocks.iter().map(|b| b.id.clone()).collect();
        &self.plan
    }
}

/// Session cache accounting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheLedger {
    pub write_tokens: u64,
    pub read_tokens: u64,
    pub input_tokens: u64,
}

impl CacheLedger {
    pub fn record(&mut self, usage: &Usage) {
        self.write_tokens += usage.cache_write_tokens;
        self.read_tokens += usage.cache_read_tokens;
        self.input_tokens += usage.input_tokens;
    }
    /// Fraction of prompt tokens served from cache (0 when nothing was sent).
    pub fn hit_ratio(&self) -> f64 {
        let total = self.read_tokens + self.write_tokens + self.input_tokens;
        if total == 0 {
            0.0
        } else {
            self.read_tokens as f64 / total as f64
        }
    }
}
