//! Prompt-overhead telemetry (master-change row 5.8): base system prompt +
//! tool prelude token overhead as a first-class, local-first metric.
//!
//! Every provider request pays a fixed overhead before any user content: the
//! base system prompt (charter, instructions, posture) and the tool prelude
//! (serialized tool definitions). This module records byte sizes and an
//! explicit, conservative token estimate (bytes / 4) per provider + model so
//! the overhead is measurable, comparable across models, and available to
//! compaction decisions without guessing.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Conservative chars-per-token ratio used for the estimate. Real tokenizers
/// vary by language and model; the estimate is labelled an estimate
/// everywhere it is surfaced and is never used for billing.
const ESTIMATED_CHARS_PER_TOKEN: usize = 4;

/// Cap on distinct (provider, model) aggregation rows.
const MAX_DISTINCT_ROWS: usize = 512;

/// One observation of the fixed per-request prompt overhead.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PromptOverheadEvent {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub system_prompt_bytes: usize,
    pub tool_prelude_bytes: usize,
}

/// Running aggregate for one (provider, model) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PromptOverheadAggregate {
    pub observations: u64,
    pub total_system_prompt_est_tokens: u64,
    pub total_tool_prelude_est_tokens: u64,
    pub max_system_prompt_est_tokens: u64,
    pub max_tool_prelude_est_tokens: u64,
}

pub type PromptOverheadRow = ((Option<String>, Option<String>), PromptOverheadAggregate);

#[derive(Default)]
struct PromptOverheadSink {
    rows: BTreeMap<(Option<String>, Option<String>), PromptOverheadAggregate>,
    dropped_rows: u64,
}

static SINK: Mutex<Option<PromptOverheadSink>> = Mutex::new(None);

fn with_sink<R>(f: impl FnOnce(&mut PromptOverheadSink) -> R) -> R {
    let mut guard = SINK
        .lock()
        .expect("prompt overhead telemetry sink lock poisoned");
    let sink = guard.get_or_insert_with(PromptOverheadSink::default);
    f(sink)
}

/// Explicit conservative token estimate from a byte length.
pub fn estimate_tokens(byte_len: usize) -> u64 {
    // Ceiling division keeps this explicitly conservative. A floor would
    // report zero for a non-empty one-byte prompt, understating fixed cost.
    byte_len.div_ceil(ESTIMATED_CHARS_PER_TOKEN) as u64
}

/// Record one provider-request overhead observation.
pub fn record_prompt_overhead(event: PromptOverheadEvent) {
    let system_tokens = estimate_tokens(event.system_prompt_bytes);
    let prelude_tokens = estimate_tokens(event.tool_prelude_bytes);
    tracing::info!(
        operation = "prompt_overhead",
        provider = event.provider.as_deref().unwrap_or("unknown"),
        model = event.model.as_deref().unwrap_or("unknown"),
        system_prompt_est_tokens = system_tokens,
        tool_prelude_est_tokens = prelude_tokens,
        "base prompt + tool prelude overhead observed"
    );
    with_sink(|sink| {
        let key = (event.provider, event.model);
        if !sink.rows.contains_key(&key) && sink.rows.len() >= MAX_DISTINCT_ROWS {
            sink.dropped_rows += 1;
            return;
        }
        let row = sink.rows.entry(key).or_default();
        row.observations += 1;
        row.total_system_prompt_est_tokens += system_tokens;
        row.total_tool_prelude_est_tokens += prelude_tokens;
        row.max_system_prompt_est_tokens = row.max_system_prompt_est_tokens.max(system_tokens);
        row.max_tool_prelude_est_tokens = row.max_tool_prelude_est_tokens.max(prelude_tokens);
    });
}

/// Snapshot the per-(provider, model) aggregates.
pub fn prompt_overhead_aggregates() -> Vec<PromptOverheadRow> {
    with_sink(|sink| sink.rows.clone().into_iter().collect())
}

/// Rows dropped because the distinct-row cap was reached.
pub fn prompt_overhead_dropped_rows() -> u64 {
    with_sink(|sink| sink.dropped_rows)
}

/// Reset the sink. Intended for tests and diagnostics endpoints.
pub fn reset_prompt_overhead_telemetry() {
    with_sink(|sink| {
        sink.rows.clear();
        sink.dropped_rows = 0;
    });
}

// ── P0-C 2d: cache-prefix / promotion telemetry ─────────────────────────────
//
// Deliberately a separate, additive sink rather than new fields on
// `PromptOverheadEvent` above. `PromptOverheadEvent` is constructed by
// struct literal from an external integration-test crate this lane does not
// own (`engine/tests/master_change/master_change_regression.rs`, a
// `[[test]]` target in the workspace `engine/Cargo.toml`); that literal
// lists every field with no `..Default::default()`, so adding required
// fields to the struct would break that crate's compilation — the "existing
// test would break" case this lane's dispatch explicitly says to avoid, not
// paper over by editing the test. `CachePrefixEvent` correlates 1:1 with a
// `PromptOverheadEvent` for the same request (same provider/model) via its
// own sink instead.

/// Cap on retained `CachePrefixEvent` rows (bounded like the aggregate sink
/// above, but this sink keeps individual observations rather than an
/// aggregate, since prefix-generation and change-reason are per-event).
const MAX_CACHE_PREFIX_EVENTS: usize = 4096;

/// One request's cache-prefix / tool-promotion observation (P0-C 2c/2d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePrefixEvent {
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Serialized bytes of always-on (core / non-promoted) tool schemas.
    pub core_bytes: usize,
    /// Serialized bytes of promoted (deferred, `tool_search`-scoped) tool
    /// schemas shipped in this request.
    pub promoted_bytes: usize,
    /// Count of distinct tools promoted this session (monotonic — never
    /// shrinks turn to turn).
    pub promoted_count: usize,
    /// Cumulative count of promotion events recorded this session.
    pub promotion_events_this_session: u64,
    /// Monotonically-increasing generation number of the combined
    /// (system-prefix + tool-schema) cache prefix hash.
    pub prefix_generation: u64,
    /// Whether the combined prefix hash changed from the prior observation.
    pub prefix_changed: bool,
    /// Attributable reason for the change, `None` when `prefix_changed` is
    /// false. Never silently unattributed when a change did occur.
    pub prefix_change_reason: Option<String>,
}

static CACHE_PREFIX_SINK: Mutex<Option<Vec<CachePrefixEvent>>> = Mutex::new(None);

/// Record one cache-prefix / promotion observation.
pub fn record_cache_prefix_event(event: CachePrefixEvent) {
    tracing::info!(
        operation = "cache_prefix",
        provider = event.provider.as_deref().unwrap_or("unknown"),
        model = event.model.as_deref().unwrap_or("unknown"),
        core_bytes = event.core_bytes,
        promoted_bytes = event.promoted_bytes,
        promoted_count = event.promoted_count,
        promotion_events_this_session = event.promotion_events_this_session,
        prefix_generation = event.prefix_generation,
        prefix_changed = event.prefix_changed,
        prefix_change_reason = event.prefix_change_reason.as_deref().unwrap_or(""),
        "cache prefix / tool-promotion observation"
    );
    let mut guard = CACHE_PREFIX_SINK
        .lock()
        .expect("cache prefix telemetry sink lock poisoned");
    let rows = guard.get_or_insert_with(Vec::new);
    if rows.len() >= MAX_CACHE_PREFIX_EVENTS {
        rows.remove(0);
    }
    rows.push(event);
}

/// Snapshot every retained cache-prefix event, oldest first.
pub fn cache_prefix_events() -> Vec<CachePrefixEvent> {
    CACHE_PREFIX_SINK
        .lock()
        .expect("cache prefix telemetry sink lock poisoned")
        .clone()
        .unwrap_or_default()
}

/// Reset the sink. Intended for tests and diagnostics endpoints.
pub fn reset_cache_prefix_telemetry() {
    let mut guard = CACHE_PREFIX_SINK
        .lock()
        .expect("cache prefix telemetry sink lock poisoned");
    *guard = None;
}
