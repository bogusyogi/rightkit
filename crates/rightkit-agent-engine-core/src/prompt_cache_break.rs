//! Prompt-cache break detection (atom `VG-001`, `ADAPT_TO_RUST`).
//!
//! Donor: `D:\Downloads\code\src\services\api\promptCacheBreakDetection.ts`
//! (claude-code). The donor's two-phase mechanism is ported as-is:
//!
//! * **Phase 1, pre-call** ([`PromptCacheBreakDetector::record_request`]):
//!   snapshot everything the client controls that can change the provider's
//!   cache key (system hash, tool-schema hash and per-tool hashes, model,
//!   request parameters, cache-control placement, per-message hashes) and
//!   diff it against the previous request's snapshot. The diff is stored as
//!   *pending changes*; no event fires yet.
//! * **Phase 2, post-call** ([`PromptCacheBreakDetector::check_response`]):
//!   compare the response's cache-read tokens with the baseline the previous
//!   response set up. A drop of more than 5% AND at least
//!   [`MIN_CACHE_MISS_TOKENS`] is a break; the pending changes (or, when none
//!   exist, the elapsed time since the previous response) explain it.
//!
//! Adaptations at the host app seams (all deliberate):
//!
//! * The baseline is the previous response's `cache_read + cache_creation`
//!   (what the provider has just been told to hold), not `cache_read` alone as
//!   in the donor. The donor misses the worst silent burn: a request that
//!   writes the cache every turn and never reads it back (`read == 0` forever
//!   so the drop is always 0).
//! * Per-message hashes are part of the snapshot (the donor never sees message
//!   edits and falls back to "likely server-side"); an edit before the
//!   previous request's tail is attributed to [`BreakReason::MessagesEdited`].
//! * A compaction epoch proves an expected history rewrite. Simultaneous
//!   system/tool/parameter changes remain independently unexpected.
//! * Idle gaps use response time for the prior response & request time for the
//!   next call; elapsed time alone never proves provider TTL expiry.
//! * The donor's temp-file unified diff is replaced by a bounded textual
//!   `detail` (changed tool names, first edited message index, char delta) on
//!   the durable event; no file is written and nothing is printed.
//! * Per-source tracking (`repl_main_thread`, `agent:*`) is not needed: every
//!   `SessionEngine` (parent or sub-agent) owns exactly one detector.
//! * The donor's Anthropic-only fields (`fastMode`, `autoModeActive`,
//!   `isUsingOverage`, `cachedMCEnabled`, `globalCacheStrategy`) collapse into
//!   `effort`, `betas`, `extra_body_hash` and `cache_control_hash`.
//!
//! The detector is dependency-free apart from `serde_json`/`serde` so the
//! `rightkit-cache` dependency does not touch it: it lives in the engine layer and only needs the numbers the
//! engine already has.

use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

/// Minimum absolute token shortfall before a drop is a break (donor value).
pub const MIN_CACHE_MISS_TOKENS: u64 = 2_000;
/// Cache read must fall below this fraction of the baseline (donor: 95%).
pub const CACHE_READ_RETAIN_RATIO: f64 = 0.95;
/// Anthropic-style ephemeral cache TTL (donor `CACHE_TTL_5MIN_MS`).
pub const CACHE_TTL_5MIN_MS: u64 = 5 * 60 * 1000;
/// Extended cache TTL (donor `CACHE_TTL_1HOUR_MS`).
pub const CACHE_TTL_1HOUR_MS: u64 = 60 * 60 * 1000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// FNV-1a 64-bit streaming hasher (stable across processes and platforms).
#[derive(Debug, Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub fn write(&mut self, bytes: &[u8]) -> &mut Self {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    /// Length-prefixed write so adjacent fields cannot alias each other.
    pub fn write_field(&mut self, bytes: &[u8]) -> &mut Self {
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes)
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

/// Hash raw bytes.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = Fnv::default();
    hasher.write(bytes);
    hasher.finish()
}

/// Hash the canonical JSON serialization of a value (donor `computeHash`).
pub fn hash_json<T: Serialize + ?Sized>(value: &T) -> u64 {
    match serde_json::to_vec(value) {
        Ok(bytes) => hash_bytes(&bytes),
        // Serialization of our own wire types cannot fail; a failure must
        // still produce a value that differs from every successful hash of
        // the same shape rather than a silent constant, so fold the error in.
        Err(error) => hash_bytes(error.to_string().as_bytes()),
    }
}

/// Cause of a cache break, in attribution-precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BreakReason {
    /// The session compaction epoch changed (expected).
    Compaction,
    /// The model changed (expected: a switch is a user action).
    Model,
    /// A system block (stable tier or boundary overlay) changed.
    System,
    /// Tool schemas or the tool set changed.
    Tools,
    /// Effort / betas / extra request body / routing key changed.
    Params,
    /// Cache-control placement changed (scope or TTL).
    CacheControl,
    /// A message at or before the previous request's tail was edited.
    MessagesEdited,
    /// Nothing changed and the idle gap exceeded the 1h TTL.
    TtlExpiry1h,
    /// Nothing changed and the idle gap exceeded the 5min TTL.
    TtlExpiry5m,
    /// Nothing changed and the gap was under TTL: provider-side eviction or
    /// routing (donor: "likely server-side").
    ServerSide,
    /// Nothing changed and no timing information exists.
    Unknown,
}

impl BreakReason {
    /// Stable wire name carried by the `promptCacheBreak` event.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compaction => "compaction",
            Self::Model => "model",
            Self::System => "system",
            Self::Tools => "tools",
            Self::Params => "params",
            Self::CacheControl => "cacheControl",
            Self::MessagesEdited => "messagesEdited",
            Self::TtlExpiry1h => "ttlExpiry1h",
            Self::TtlExpiry5m => "ttlExpiry5m",
            Self::ServerSide => "serverSide",
            Self::Unknown => "unknown",
        }
    }
}

/// One tool in a snapshot: name plus the hash of its full schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSnapshot {
    pub name: String,
    pub hash: u64,
}

/// Name of a provider tool definition (`{"function":{"name":..}}` or
/// `{"name":..}`), falling back to its position like the donor's `__idx_N`.
fn tool_name(def: &serde_json::Value, index: usize) -> String {
    def.get("function")
        .and_then(|f| f.get("name"))
        .or_else(|| def.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("__idx_{index}"))
}

/// Everything the client controls that can change the provider cache key for
/// one request (donor `PromptStateSnapshot`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptStateInput {
    pub system_hash: u64,
    pub system_chars: usize,
    pub tools: Vec<ToolSnapshot>,
    /// One hash per message AFTER the system tier, in wire order. The volatile
    /// per-turn tail (goal reminder) must not be included.
    pub message_hashes: Vec<u64>,
    pub model: String,
    pub effort: String,
    pub betas: Vec<String>,
    pub extra_body_hash: u64,
    pub cache_control_hash: u64,
    /// Session compaction epoch; a change is an expected break.
    pub compaction_epoch: u64,
    /// Hash of the model-promoted (`tool_search`) tool-name set. A change that
    /// only adds or ages out tools is the deferred-tool design working, so the
    /// resulting tool-block break is expected.
    pub tool_promotion_hash: u64,
    pub cache_eligible: bool,
    pub expected_reusable_prefix_tokens: Option<u64>,
}

impl PromptStateInput {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    /// System blocks in wire order (hash is order- and boundary-sensitive).
    pub fn with_system<S: AsRef<str>>(mut self, blocks: &[S]) -> Self {
        let mut hasher = Fnv::default();
        let mut chars = 0usize;
        for block in blocks {
            let text = block.as_ref();
            hasher.write_field(text.as_bytes());
            chars += text.chars().count();
        }
        self.system_hash = hasher.finish();
        self.system_chars = chars;
        self
    }

    /// Tool definitions exactly as serialized onto the wire.
    pub fn with_tool_defs(mut self, defs: &[serde_json::Value]) -> Self {
        self.tools = defs
            .iter()
            .enumerate()
            .map(|(index, def)| ToolSnapshot {
                name: tool_name(def, index),
                hash: hash_json(def),
            })
            .collect();
        self
    }

    pub fn with_message_hashes(mut self, hashes: Vec<u64>) -> Self {
        self.message_hashes = hashes;
        self
    }

    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        self.effort = effort.into();
        self
    }

    pub fn with_betas<S: AsRef<str>>(mut self, betas: &[S]) -> Self {
        let mut sorted: Vec<String> = betas.iter().map(|b| b.as_ref().to_owned()).collect();
        sorted.sort();
        self.betas = sorted;
        self
    }

    pub fn with_extra_body_hash(mut self, hash: u64) -> Self {
        self.extra_body_hash = hash;
        self
    }

    pub fn with_cache_control_hash(mut self, hash: u64) -> Self {
        self.cache_control_hash = hash;
        self
    }

    pub fn with_compaction_epoch(mut self, epoch: u64) -> Self {
        self.compaction_epoch = epoch;
        self
    }

    pub fn with_tool_promotion_hash(mut self, hash: u64) -> Self {
        self.tool_promotion_hash = hash;
        self
    }

    pub fn with_cache_coverage(mut self, eligible: bool, tokens: Option<u64>) -> Self {
        self.cache_eligible = eligible;
        self.expected_reusable_prefix_tokens = tokens;
        self
    }

    fn tools_hash(&self) -> u64 {
        let mut hasher = Fnv::default();
        for tool in &self.tools {
            hasher.write_field(tool.name.as_bytes());
            hasher.write(&tool.hash.to_le_bytes());
        }
        hasher.finish()
    }
}

/// Token usage of one provider response, provider-normalised: `input_tokens`
/// EXCLUDES cache reads and cache writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResponseCacheUsage {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// A detected cache break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBreak {
    /// Primary cause (first unexpected cause in precedence order, else first
    /// expected one).
    pub reason: BreakReason,
    /// True when every observed cause was deliberate / unavoidable.
    pub expected: bool,
    /// Baseline minus cache read.
    pub tokens_lost: u64,
    /// Every observed cause, donor-style (`"system prompt changed (+3 chars), ..."`).
    pub detail: String,
    pub previous_cache_read_tokens: u64,
    pub previous_cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// 1-based provider request number within the session.
    pub turn: u64,
    pub cumulative_tokens_lost: u64,
}

/// Why a break was deliberate even though a naive diff would flag it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedCause {
    /// Cache-editing deletions were sent; the next read legitimately drops
    /// (donor `notifyCacheDeletion`).
    CacheDeletion,
}

#[derive(Debug, Clone, Default)]
struct MessageEdit {
    /// First wire index whose hash changed, or the new length on truncation.
    first_index: usize,
    previous_len: usize,
    new_len: usize,
}

#[derive(Debug, Clone, Default)]
struct PendingChanges {
    system: bool,
    system_char_delta: i64,
    tools: bool,
    /// The promoted-tool set changed between the two snapshots.
    promotion: bool,
    added_tools: Vec<String>,
    removed_tools: Vec<String>,
    changed_tools: Vec<String>,
    model: bool,
    previous_model: String,
    new_model: String,
    effort: bool,
    previous_effort: String,
    new_effort: String,
    betas: bool,
    added_betas: Vec<String>,
    removed_betas: Vec<String>,
    extra_body: bool,
    cache_control: bool,
    compaction: bool,
    messages: Option<MessageEdit>,
}

impl PendingChanges {
    fn any(&self) -> bool {
        self.system
            || self.tools
            || self.model
            || self.effort
            || self.betas
            || self.extra_body
            || self.cache_control
            || self.compaction
            || self.messages.is_some()
    }

    /// Fold a later diff into an earlier one that no response has consumed
    /// yet (a request that failed before returning usage still changed the
    /// provider-visible prefix).
    fn merge(&mut self, later: PendingChanges) {
        self.system |= later.system;
        self.system_char_delta += later.system_char_delta;
        self.tools |= later.tools;
        self.promotion |= later.promotion;
        extend_unique(&mut self.added_tools, later.added_tools);
        extend_unique(&mut self.removed_tools, later.removed_tools);
        extend_unique(&mut self.changed_tools, later.changed_tools);
        if later.model {
            if !self.model {
                self.previous_model = later.previous_model;
            }
            self.model = true;
            self.new_model = later.new_model;
        }
        if later.effort {
            if !self.effort {
                self.previous_effort = later.previous_effort;
            }
            self.effort = true;
            self.new_effort = later.new_effort;
        }
        self.betas |= later.betas;
        extend_unique(&mut self.added_betas, later.added_betas);
        extend_unique(&mut self.removed_betas, later.removed_betas);
        self.extra_body |= later.extra_body;
        self.cache_control |= later.cache_control;
        self.compaction |= later.compaction;
        self.messages = match (self.messages.take(), later.messages) {
            (Some(a), Some(b)) => Some(if a.first_index <= b.first_index { a } else { b }),
            (a, b) => a.or(b),
        };
    }
}

fn extend_unique(into: &mut Vec<String>, from: Vec<String>) {
    for item in from {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Baseline {
    cache_read: u64,
    cache_creation: u64,
}

impl Baseline {
    /// What the provider is expected to serve from cache on the next request.
    fn expected_read(self) -> u64 {
        self.cache_read.saturating_add(self.cache_creation)
    }
}

/// Per-session prompt-cache break detector (one per `SessionEngine`).
#[derive(Debug, Clone, Default)]
pub struct PromptCacheBreakDetector {
    previous: Option<PromptStateInput>,
    pending: Option<PendingChanges>,
    baseline: Option<Baseline>,
    call_count: u64,
    request_ms: Option<u64>,
    last_response_ms: Option<u64>,
    cache_deletion_pending: bool,
    expected_reusable_prefix_tokens: u64,
    cumulative_tokens_lost: u64,
    cache_eligible: bool,
}

impl PromptCacheBreakDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of provider requests recorded so far.
    pub fn call_count(&self) -> u64 {
        self.call_count
    }

    pub fn cumulative_tokens_lost(&self) -> u64 {
        self.cumulative_tokens_lost
    }

    /// Mark the next observed break cause as deliberate.
    pub fn note_expected(&mut self, cause: ExpectedCause) {
        match cause {
            ExpectedCause::CacheDeletion => self.cache_deletion_pending = true,
        }
    }

    /// Phase 1: snapshot the request about to be sent and diff it against the
    /// previous request. `now_ms` is a monotonic-enough wall clock in
    /// milliseconds (any epoch; only differences are used).
    pub fn record_request(&mut self, input: PromptStateInput, now_ms: u64) {
        self.call_count += 1;
        self.request_ms = Some(now_ms);
        let Some(previous) = self.previous.as_ref() else {
            self.previous = Some(input);
            return;
        };
        // Only the previous request can have been cached. Fresh user/tool
        // suffixes on this request cannot count as lost reusable tokens.
        self.expected_reusable_prefix_tokens =
            previous.expected_reusable_prefix_tokens.unwrap_or(0);
        let changes = diff_snapshots(previous, &input);
        if changes.any() {
            match self.pending.as_mut() {
                Some(existing) => existing.merge(changes),
                None => self.pending = Some(changes),
            }
        }
        self.previous = Some(input);
    }

    /// Phase 2: evaluate the response for a cache break. Returns `Some` only
    /// when cache reads fell materially below the baseline.
    pub fn check_response(&mut self, usage: ResponseCacheUsage) -> Option<CacheBreak> {
        let snapshot = self.previous.as_ref()?;
        // A response with no usage at all tells us nothing; keep the pending
        // diff and the baseline for the next response.
        if usage.input_tokens == 0
            && usage.cache_read_tokens == 0
            && usage.cache_creation_tokens == 0
        {
            return None;
        }
        let snapshot = snapshot.clone();
        self.cache_eligible |= snapshot.cache_eligible
            || usage.cache_read_tokens > 0
            || usage.cache_creation_tokens > 0;
        let baseline = self.baseline.replace(Baseline {
            cache_read: usage.cache_read_tokens,
            cache_creation: usage.cache_creation_tokens,
        });
        let gap_ms = match (self.last_response_ms, self.request_ms) {
            (Some(last), Some(request)) => Some(request.saturating_sub(last)),
            _ => None,
        };
        self.last_response_ms = Some(now_ms());
        let pending = self.pending.take();
        let cache_deletion = std::mem::take(&mut self.cache_deletion_pending);

        if !self.cache_eligible {
            return None;
        }
        // First call: no previous value to compare against.
        let baseline = baseline?;
        if cache_deletion {
            // Cache deletions intentionally shrink the cached prefix; the
            // baseline was already reset above, so do not flag.
            return None;
        }
        // Provider-reported cache coverage is authoritative when available;
        // a byte estimator is not evidence of a miss beyond that coverage.
        let observed = baseline.expected_read();
        let expected_read = if observed > 0 {
            observed
        } else {
            self.expected_reusable_prefix_tokens
        };
        let shortfall = expected_read.saturating_sub(usage.cache_read_tokens);
        if shortfall == 0 {
            return None;
        }

        let (reason, expected, mut detail) = attribute(pending.as_ref(), gap_ms);
        self.cumulative_tokens_lost = self.cumulative_tokens_lost.saturating_add(shortfall);
        detail.push_str(&format!(
            "; cumulative estimated reusable-prefix loss: {} tokens",
            self.cumulative_tokens_lost
        ));
        Some(CacheBreak {
            reason,
            expected,
            tokens_lost: shortfall,
            detail,
            previous_cache_read_tokens: baseline.cache_read,
            previous_cache_creation_tokens: baseline.cache_creation,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            turn: self.call_count,
            cumulative_tokens_lost: self.cumulative_tokens_lost,
        })
    }
}

fn diff_snapshots(previous: &PromptStateInput, current: &PromptStateInput) -> PendingChanges {
    let mut changes = PendingChanges {
        system: previous.system_hash != current.system_hash,
        system_char_delta: current.system_chars as i64 - previous.system_chars as i64,
        ..PendingChanges::default()
    };

    if previous.tools_hash() != current.tools_hash() {
        changes.tools = true;
        let previous_hashes: HashMap<&str, u64> = previous
            .tools
            .iter()
            .map(|tool| (tool.name.as_str(), tool.hash))
            .collect();
        let current_names: BTreeSet<&str> = current
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        for tool in &current.tools {
            match previous_hashes.get(tool.name.as_str()) {
                None => changes.added_tools.push(tool.name.clone()),
                Some(hash) if *hash != tool.hash => changes.changed_tools.push(tool.name.clone()),
                Some(_) => {}
            }
        }
        for tool in &previous.tools {
            if !current_names.contains(tool.name.as_str()) {
                changes.removed_tools.push(tool.name.clone());
            }
        }
    }

    if previous.model != current.model {
        changes.model = true;
        changes.previous_model = previous.model.clone();
        changes.new_model = current.model.clone();
    }
    if previous.effort != current.effort {
        changes.effort = true;
        changes.previous_effort = previous.effort.clone();
        changes.new_effort = current.effort.clone();
    }
    if previous.betas != current.betas {
        changes.betas = true;
        changes.added_betas = current
            .betas
            .iter()
            .filter(|beta| !previous.betas.contains(beta))
            .cloned()
            .collect();
        changes.removed_betas = previous
            .betas
            .iter()
            .filter(|beta| !current.betas.contains(beta))
            .cloned()
            .collect();
    }
    changes.extra_body = previous.extra_body_hash != current.extra_body_hash;
    changes.cache_control = previous.cache_control_hash != current.cache_control_hash;
    changes.compaction = previous.compaction_epoch != current.compaction_epoch;
    changes.promotion = previous.tool_promotion_hash != current.tool_promotion_hash;

    // Compaction rewrites history by design; do not double-report it as an
    // edit. Otherwise the previous request's whole message list must still be
    // a byte-identical prefix of this one's.
    if !changes.compaction {
        let common = previous
            .message_hashes
            .len()
            .min(current.message_hashes.len());
        let first_diff = (0..common)
            .find(|&index| previous.message_hashes[index] != current.message_hashes[index]);
        let first_index = match first_diff {
            Some(index) => Some(index),
            None if current.message_hashes.len() < previous.message_hashes.len() => Some(common),
            None => None,
        };
        changes.messages = first_index.map(|first_index| MessageEdit {
            first_index,
            previous_len: previous.message_hashes.len(),
            new_len: current.message_hashes.len(),
        });
    }
    changes
}

/// Turn pending changes (or the idle gap) into `(primary, expected, detail)`.
fn attribute(pending: Option<&PendingChanges>, gap_ms: Option<u64>) -> (BreakReason, bool, String) {
    // (reason, expected) in precedence order.
    let mut causes: Vec<(BreakReason, bool)> = Vec::new();
    let mut parts: Vec<String> = Vec::new();

    if let Some(changes) = pending {
        if changes.model {
            causes.push((BreakReason::Model, false));
            parts.push(format!(
                "model changed ({} -> {})",
                changes.previous_model, changes.new_model
            ));
        }
        if changes.system {
            causes.push((BreakReason::System, false));
            let delta = match changes.system_char_delta {
                0 => String::new(),
                delta if delta > 0 => format!(" (+{delta} chars)"),
                delta => format!(" ({delta} chars)"),
            };
            parts.push(format!("system prompt changed{delta}"));
        }
        if changes.compaction {
            // Preserve simultaneous prompt changes as the primary diagnosis;
            // compaction is still reported as a secondary cause.
            causes.push((BreakReason::Compaction, true));
            parts.push("session compacted".to_string());
        }
        if changes.tools {
            // Promotion only adds tools or ages them out of the capped window;
            // a changed schema on an existing tool is never the promotion.
            let promotion_only = changes.promotion && changes.changed_tools.is_empty();
            causes.push((BreakReason::Tools, false));
            let diff = if changes.added_tools.is_empty() && changes.removed_tools.is_empty() {
                " (tool prompt/schema changed, same tool set)".to_string()
            } else {
                format!(
                    " (+{}/-{} tools)",
                    changes.added_tools.len(),
                    changes.removed_tools.len()
                )
            };
            let mut text = format!("tools changed{diff}");
            if !changes.changed_tools.is_empty() {
                text.push_str(&format!(
                    " [schema changed: {}]",
                    bounded_names(&changes.changed_tools)
                ));
            }
            if !changes.added_tools.is_empty() {
                text.push_str(&format!(
                    " [added: {}]",
                    bounded_names(&changes.added_tools)
                ));
            }
            if !changes.removed_tools.is_empty() {
                text.push_str(&format!(
                    " [removed: {}]",
                    bounded_names(&changes.removed_tools)
                ));
            }
            if promotion_only {
                text.push_str(" [tool_search promotion]");
            }
            parts.push(text);
        }
        if changes.effort || changes.betas || changes.extra_body {
            causes.push((BreakReason::Params, false));
            if changes.effort {
                parts.push(format!(
                    "effort changed ({} -> {})",
                    or_default(&changes.previous_effort),
                    or_default(&changes.new_effort)
                ));
            }
            if changes.betas {
                let added = (!changes.added_betas.is_empty())
                    .then(|| format!("+{}", changes.added_betas.join(",")));
                let removed = (!changes.removed_betas.is_empty())
                    .then(|| format!("-{}", changes.removed_betas.join(",")));
                let diff = [added, removed]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                parts.push(if diff.is_empty() {
                    "betas changed".to_string()
                } else {
                    format!("betas changed ({diff})")
                });
            }
            if changes.extra_body {
                parts.push("extra body params changed".to_string());
            }
        }
        // Donor: only a standalone cause when nothing else explains it.
        if changes.cache_control && !changes.system {
            causes.push((BreakReason::CacheControl, false));
            parts.push("cache_control changed (scope or TTL)".to_string());
        }
        if let Some(edit) = &changes.messages {
            causes.push((BreakReason::MessagesEdited, false));
            parts.push(if edit.first_index >= edit.new_len {
                format!(
                    "history truncated ({} -> {} messages)",
                    edit.previous_len, edit.new_len
                )
            } else {
                format!(
                    "message {} of {} edited before the cached tail ({} messages previously)",
                    edit.first_index, edit.new_len, edit.previous_len
                )
            });
        }
    }

    if causes.is_empty() {
        // TTL policy varies by provider and marker. Time alone is not evidence
        // that expiry was expected, so every unexplained miss remains visible.
        return match gap_ms {
            Some(gap) => (
                BreakReason::ServerSide,
                false,
                format!("provider cache miss with unchanged request after {gap}ms; TTL policy unavailable"),
            ),
            None => (BreakReason::Unknown, false, "unknown cause".to_string()),
        };
    }

    let expected = causes.iter().all(|(_, expected)| *expected);
    let reason = causes
        .iter()
        .find(|(_, expected)| !*expected)
        .or_else(|| causes.first())
        .map(|(reason, _)| *reason)
        .unwrap_or(BreakReason::Unknown);
    (reason, expected, parts.join(", "))
}

fn or_default(value: &str) -> &str {
    if value.is_empty() {
        "default"
    } else {
        value
    }
}

/// At most eight names, so a 200-tool MCP swap cannot bloat a durable event.
fn bounded_names(names: &[String]) -> String {
    const MAX: usize = 8;
    if names.len() <= MAX {
        return names.join(",");
    }
    format!("{},+{} more", names[..MAX].join(","), names.len() - MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, description: &str) -> serde_json::Value {
        json!({"type": "function", "function": {"name": name, "description": description}})
    }

    fn base() -> PromptStateInput {
        PromptStateInput::new("claude-sonnet-4-6")
            .with_system(&["you are an agent", "workspace rules"])
            .with_tool_defs(&[tool("read_file", "reads"), tool("write_file", "writes")])
            .with_message_hashes(vec![1, 2, 3])
            .with_effort("high")
            .with_cache_coverage(true, Some(50_000))
    }

    /// Warm cache: request 1 writes 50k, request 2 reads it back.
    fn warm(detector: &mut PromptCacheBreakDetector, input: PromptStateInput) -> u64 {
        detector.record_request(input.clone(), 1_000);
        assert!(detector
            .check_response(ResponseCacheUsage {
                input_tokens: 100,
                cache_read_tokens: 0,
                cache_creation_tokens: 50_000,
            })
            .is_none());
        let mut next = input;
        next.message_hashes.push(4);
        detector.record_request(next, 2_000);
        assert!(detector
            .check_response(ResponseCacheUsage {
                input_tokens: 100,
                cache_read_tokens: 50_000,
                cache_creation_tokens: 400,
            })
            .is_none());
        2_000
    }

    fn miss() -> ResponseCacheUsage {
        ResponseCacheUsage {
            input_tokens: 100,
            cache_read_tokens: 3_000,
            cache_creation_tokens: 48_000,
        }
    }

    fn with_next(
        detector: &mut PromptCacheBreakDetector,
        mutate: impl FnOnce(&mut PromptStateInput),
        now: u64,
    ) {
        let mut next = base();
        next.message_hashes = vec![1, 2, 3, 4, 5];
        mutate(&mut next);
        detector.record_request(next, now);
    }

    fn warmed() -> PromptCacheBreakDetector {
        let mut detector = PromptCacheBreakDetector::new();
        warm(&mut detector, base());
        detector
    }

    #[test]
    fn healthy_growth_is_never_a_break() {
        let mut detector = warmed();
        with_next(&mut detector, |_| {}, 3_000);
        let result = detector.check_response(ResponseCacheUsage {
            input_tokens: 80,
            cache_read_tokens: 50_400,
            cache_creation_tokens: 900,
        });
        assert_eq!(result, None);
    }

    #[test]
    fn first_call_never_breaks() {
        let mut detector = PromptCacheBreakDetector::new();
        detector.record_request(base(), 0);
        assert_eq!(detector.check_response(miss()), None);
    }

    #[test]
    fn small_cache_drop_is_reported() {
        let mut detector = PromptCacheBreakDetector::new();
        detector.record_request(base(), 0);
        detector.check_response(ResponseCacheUsage {
            input_tokens: 10,
            cache_read_tokens: 3_000,
            cache_creation_tokens: 0,
        });
        with_next(&mut detector, |_| {}, 10);
        let found = detector
            .check_response(ResponseCacheUsage {
                input_tokens: 10,
                cache_read_tokens: 1_500,
                cache_creation_tokens: 0,
            })
            .expect("small losses remain visible");
        assert_eq!(found.tokens_lost, 1_500);
    }

    #[test]
    fn every_observed_cache_shortfall_is_reported() {
        let mut detector = warmed();
        with_next(&mut detector, |_| {}, 3_000);
        let found = detector
            .check_response(ResponseCacheUsage {
                input_tokens: 10,
                cache_read_tokens: 48_000,
                cache_creation_tokens: 100,
            })
            .expect("small shortfall remains visible");
        assert_eq!(found.tokens_lost, 2_400);
    }

    #[test]
    fn write_without_read_is_caught_the_donor_would_miss() {
        let mut detector = PromptCacheBreakDetector::new();
        detector.record_request(base(), 0);
        detector.check_response(ResponseCacheUsage {
            input_tokens: 10,
            cache_read_tokens: 0,
            cache_creation_tokens: 40_000,
        });
        with_next(&mut detector, |_| {}, 10);
        let broke = detector
            .check_response(ResponseCacheUsage {
                input_tokens: 10,
                cache_read_tokens: 0,
                cache_creation_tokens: 40_500,
            })
            .expect("a cache that is written every turn and never read is a break");
        assert_eq!(broke.tokens_lost, 40_000);
        assert_eq!(broke.reason, BreakReason::ServerSide);
        assert!(!broke.expected);
    }

    #[test]
    fn repeated_zero_reads_keep_reporting_cumulative_loss() {
        let mut detector = warmed();
        with_next(&mut detector, |_| {}, 3_000);
        let first = detector.check_response(miss()).expect("first miss");
        with_next(&mut detector, |_| {}, 4_000);
        let second = detector.check_response(miss()).expect("repeated miss");
        assert_eq!(first.tokens_lost, 47_400);
        assert_eq!(second.tokens_lost, 48_000);
        assert_eq!(second.cumulative_tokens_lost, 95_400);
    }

    #[test]
    fn system_change_is_unexpected_and_attributed() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                *next = next
                    .clone()
                    .with_system(&["you are an agent", "workspace rules v2!"])
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::System);
        assert!(!broke.expected);
        assert!(
            broke.detail.contains("system prompt changed (+4 chars)"),
            "{}",
            broke.detail
        );
        assert_eq!(broke.turn, 3);
        assert_eq!(broke.tokens_lost, 47_400);
    }

    #[test]
    fn tool_schema_change_names_the_tool() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                *next = next.clone().with_tool_defs(&[
                    tool("read_file", "reads, differently"),
                    tool("write_file", "writes"),
                ])
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Tools);
        assert!(!broke.expected);
        assert!(broke.detail.contains("same tool set"), "{}", broke.detail);
        assert!(
            broke.detail.contains("schema changed: read_file"),
            "{}",
            broke.detail
        );
    }

    #[test]
    fn tool_added_and_removed_are_counted() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                *next = next
                    .clone()
                    .with_tool_defs(&[tool("read_file", "reads"), tool("shell", "runs")])
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Tools);
        assert!(broke.detail.contains("+1/-1 tools"), "{}", broke.detail);
        assert!(broke.detail.contains("added: shell"), "{}", broke.detail);
        assert!(
            broke.detail.contains("removed: write_file"),
            "{}",
            broke.detail
        );
    }

    #[test]
    fn tool_set_change_without_intent_evidence_is_unexpected() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| *next = next.clone().with_tool_defs(&[tool("read_file", "reads")]),
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Tools);
        assert!(!broke.expected, "tool change origin was not proven");
    }

    #[test]
    fn tool_search_promotion_without_user_intent_remains_unexpected() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                *next = next
                    .clone()
                    .with_tool_defs(&[
                        tool("read_file", "reads"),
                        tool("write_file", "writes"),
                        tool("lsp", "promoted"),
                    ])
                    .with_tool_promotion_hash(11)
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Tools);
        assert!(!broke.expected, "{}", broke.detail);
        assert!(
            broke.detail.contains("tool_search promotion"),
            "{}",
            broke.detail
        );

        // A promotion that coincides with a changed existing schema is not excused.
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                *next = next
                    .clone()
                    .with_tool_defs(&[
                        tool("read_file", "reads, changed"),
                        tool("write_file", "writes"),
                    ])
                    .with_tool_promotion_hash(11)
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert!(!broke.expected, "{}", broke.detail);

        // Tool changes with an unchanged promoted set are unexpected.
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| *next = next.clone().with_tool_defs(&[tool("read_file", "reads")]),
            3_000,
        );
        assert!(!detector.check_response(miss()).expect("break").expected);
    }

    #[test]
    fn tool_change_does_not_excuse_a_system_change() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                *next = next
                    .clone()
                    .with_tool_defs(&[tool("read_file", "reads")])
                    .with_system(&["totally different"])
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::System);
        assert!(!broke.expected);
    }

    #[test]
    fn unexplained_tool_change_remains_visible() {
        let mut detector = warmed();
        with_next(&mut detector, |_| {}, 3_000);
        assert_eq!(
            detector.check_response(ResponseCacheUsage {
                input_tokens: 10,
                cache_read_tokens: 50_400,
                cache_creation_tokens: 10,
            }),
            None
        );
        with_next(
            &mut detector,
            |next| *next = next.clone().with_tool_defs(&[tool("read_file", "reads")]),
            4_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert!(!broke.expected, "the earlier note must not leak forward");
    }

    #[test]
    fn model_switch_without_explicit_user_intent_is_unexpected() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| next.model = "claude-opus-4-6".into(),
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Model);
        assert!(!broke.expected);
        assert!(
            broke
                .detail
                .contains("claude-sonnet-4-6 -> claude-opus-4-6"),
            "{}",
            broke.detail
        );
    }

    #[test]
    fn effort_change_without_intent_evidence_is_unexpected() {
        let mut detector = warmed();
        with_next(&mut detector, |next| next.effort = "low".into(), 3_000);
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Params);
        assert!(!broke.expected);
        assert!(
            broke.detail.contains("effort changed (high -> low)"),
            "{}",
            broke.detail
        );

        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| *next = next.clone().with_betas(&["interleaved-thinking"]),
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Params);
        assert!(!broke.expected);
        assert!(
            broke.detail.contains("+interleaved-thinking"),
            "{}",
            broke.detail
        );

        let mut detector = warmed();
        with_next(&mut detector, |next| next.extra_body_hash = 99, 3_000);
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Params);
        assert!(!broke.expected);
    }

    #[test]
    fn effort_change_cannot_hide_an_extra_body_change() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                next.effort = "low".into();
                next.extra_body_hash = 7;
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert!(!broke.expected);
    }

    #[test]
    fn cache_control_change_is_a_standalone_cause_only_when_nothing_else_explains_it() {
        let mut detector = warmed();
        with_next(&mut detector, |next| next.cache_control_hash = 5, 3_000);
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::CacheControl);
        assert!(!broke.expected);

        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                next.cache_control_hash = 5;
                *next = next.clone().with_system(&["different"]);
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::System);
        assert!(!broke.detail.contains("cache_control"), "{}", broke.detail);
    }

    #[test]
    fn edited_history_is_attributed_with_the_first_edited_index() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| next.message_hashes = vec![1, 99, 3, 4, 5],
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::MessagesEdited);
        assert!(!broke.expected);
        assert!(
            broke.detail.contains("message 1 of 5 edited"),
            "{}",
            broke.detail
        );
    }

    #[test]
    fn truncated_history_without_compaction_is_unexpected() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| next.message_hashes = vec![1, 2],
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::MessagesEdited);
        assert!(
            broke.detail.contains("history truncated"),
            "{}",
            broke.detail
        );
    }

    #[test]
    fn appending_messages_is_not_an_edit() {
        let previous = base();
        let mut current = base();
        current.message_hashes.extend([4, 5, 6]);
        assert!(diff_snapshots(&previous, &current).messages.is_none());
    }

    #[test]
    fn compaction_is_expected_and_does_not_double_report_message_edits() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                next.compaction_epoch = 1;
                next.message_hashes = vec![42];
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::Compaction);
        assert!(
            broke.expected,
            "a compaction epoch proves an expected history rewrite"
        );
        assert!(!broke.detail.contains("edited"), "{}", broke.detail);
    }

    #[test]
    fn compaction_does_not_excuse_a_simultaneous_system_change() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| {
                next.compaction_epoch = 1;
                *next = next.clone().with_system(&["changed"]);
            },
            3_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::System);
        assert!(!broke.expected);
    }

    #[test]
    fn idle_gap_is_unexpected_without_provider_ttl_evidence() {
        let mut detector = warmed();
        with_next(&mut detector, |_| {}, 2_000 + CACHE_TTL_5MIN_MS + 1);
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::ServerSide);
        assert!(!broke.expected);
    }

    #[test]
    fn long_idle_gap_is_unexpected_without_provider_ttl_evidence() {
        let mut detector = warmed();
        with_next(&mut detector, |_| {}, 2_000 + CACHE_TTL_1HOUR_MS + 1);
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::ServerSide);
        assert!(!broke.expected);
    }

    #[test]
    fn unchanged_prompt_and_short_gap_is_server_side_and_unexpected() {
        let mut detector = warmed();
        with_next(&mut detector, |_| {}, 2_000 + 10_000);
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::ServerSide);
        assert!(
            !broke.expected,
            "an unexplained miss is exactly what we must surface"
        );
        assert!(
            broke.detail.contains("TTL policy unavailable"),
            "{}",
            broke.detail
        );
    }

    #[test]
    fn missing_timing_information_is_unknown() {
        let (reason, expected, _) = attribute(None, None);
        assert_eq!(reason, BreakReason::Unknown);
        assert!(!expected);
    }

    #[test]
    fn cache_deletion_drop_is_not_a_break_and_resets_the_baseline() {
        let mut detector = warmed();
        detector.note_expected(ExpectedCause::CacheDeletion);
        with_next(&mut detector, |_| {}, 3_000);
        assert_eq!(detector.check_response(miss()), None);
        // The baseline is now the reduced read; a healthy next call is clean.
        with_next(&mut detector, |_| {}, 4_000);
        assert_eq!(
            detector.check_response(ResponseCacheUsage {
                input_tokens: 10,
                cache_read_tokens: 51_000,
                cache_creation_tokens: 0,
            }),
            None
        );
    }

    #[test]
    fn haiku_route_is_not_blanket_excluded() {
        let mut detector = PromptCacheBreakDetector::new();
        let mut input = base();
        input.model = "claude-haiku-4-5".into();
        warm(&mut detector, input.clone());
        let mut next = input;
        next.message_hashes = vec![1, 2, 3, 4, 5];
        detector.record_request(next, 3_000);
        assert!(detector.check_response(miss()).is_some());
    }

    #[test]
    fn usage_less_response_keeps_pending_changes_and_baseline() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| *next = next.clone().with_system(&["changed"]),
            3_000,
        );
        assert_eq!(detector.check_response(ResponseCacheUsage::default()), None);
        with_next(
            &mut detector,
            |next| *next = next.clone().with_system(&["changed"]),
            4_000,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(
            broke.reason,
            BreakReason::System,
            "pending diff must survive"
        );
        assert_eq!(broke.tokens_lost, 47_400, "baseline must survive");
    }

    #[test]
    fn changes_from_a_failed_request_survive_the_retry() {
        let mut detector = warmed();
        // Request 3 changes the system prompt but fails before usage arrives.
        with_next(
            &mut detector,
            |next| *next = next.clone().with_system(&["changed"]),
            3_000,
        );
        // Request 4 (the retry) is identical to request 3.
        with_next(
            &mut detector,
            |next| *next = next.clone().with_system(&["changed"]),
            3_100,
        );
        let broke = detector.check_response(miss()).expect("break");
        assert_eq!(broke.reason, BreakReason::System);
        assert!(!broke.expected);
    }

    #[test]
    fn a_break_is_reported_once_and_the_next_healthy_call_is_clean() {
        let mut detector = warmed();
        with_next(
            &mut detector,
            |next| *next = next.clone().with_system(&["changed"]),
            3_000,
        );
        assert!(detector.check_response(miss()).is_some());
        with_next(
            &mut detector,
            |next| *next = next.clone().with_system(&["changed"]),
            4_000,
        );
        assert_eq!(
            detector.check_response(ResponseCacheUsage {
                input_tokens: 10,
                cache_read_tokens: 51_000,
                cache_creation_tokens: 100,
            }),
            None
        );
    }

    #[test]
    fn hashes_are_stable_and_boundary_sensitive() {
        let a = PromptStateInput::new("m").with_system(&["ab", "c"]);
        let b = PromptStateInput::new("m").with_system(&["a", "bc"]);
        let a2 = PromptStateInput::new("m").with_system(&["ab", "c"]);
        assert_eq!(a.system_hash, a2.system_hash);
        assert_ne!(a.system_hash, b.system_hash, "block boundaries must matter");
        assert_eq!(hash_bytes(b""), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn tool_hash_is_order_sensitive_and_name_falls_back_to_index() {
        let one = PromptStateInput::new("m").with_tool_defs(&[tool("a", "x"), tool("b", "y")]);
        let two = PromptStateInput::new("m").with_tool_defs(&[tool("b", "y"), tool("a", "x")]);
        assert_ne!(one.tools_hash(), two.tools_hash());
        let anonymous = PromptStateInput::new("m").with_tool_defs(&[json!({"x": 1})]);
        assert_eq!(anonymous.tools[0].name, "__idx_0");
        let flat = PromptStateInput::new("m").with_tool_defs(&[json!({"name": "flat"})]);
        assert_eq!(flat.tools[0].name, "flat");
    }

    #[test]
    fn bounded_names_caps_the_event_detail() {
        let names: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
        let text = bounded_names(&names);
        assert!(text.ends_with("+12 more"), "{text}");
        assert!(!text.contains("t8"));
    }
}
