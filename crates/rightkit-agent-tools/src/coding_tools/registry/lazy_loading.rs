use super::native_types::{ModelToolInterface, ToolRegistry};
use crate::coding_tools::tool_trait::Tool;

// Deferred / lazy tool loading (par §#157).
//
// Claude Code's ToolSearchTool keeps the model-visible prelude small: a core
// set of always-on tools ships full schemas every turn, everything else is
// discoverable via `tool_search` and only bound into the request once the
// agent has scoped it by name. `tool_search.rs` already implements the
// discovery half; this module supplies the CORE allowlist and the registry
// state that turns deferral on/off, so `ToolRegistry` can tell a caller
// which tools belong in the always-on prelude and which are deferred by
// default when lazy mode is enabled.
//
// This is a token-tax reduction, not a capability change: a deferred tool
// remains fully registered and dispatchable by name (see
// `execute_single_tool_call` in the engine, which looks tools up by name
// regardless of prelude membership). Lazy mode only changes which schemas
// `tool_defs_for_messages` (below) serializes into the provider request by
// default.
//
// P0-C (docs/canon/daemon-master-canon-atoms.md, Lane 2) moved that serialization
// policy — monotonic promotion, canonical ordering, and the schema-byte
// ceiling — into this crate. It previously lived as private fns
// (`tool_defs_for_messages` / `scoped_deferred_tool_names`) in the binary
// crate (`engine/host-app/src/provider_client.rs`) while the core-tool
// allowlist above lived here — a split with no cross-cutting tests. This
// module is now the single owner of both halves.

/// Always-on core toolset: the minimal set an agent needs to read, edit,
/// run shell commands, search, and discover the rest. Mirrors Claude Code's
/// always-resident core (read/edit/shell/search/tool_search itself).
pub const CORE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "multi_edit",
    "apply_patch",
    "shell",
    "powershell",
    "search",
    "list_files",
    "tool_search",
    "todo_write",
    "ask_user",
    "exit_plan_mode",
];

/// True when `name` is in the always-on core set (ships its schema in the
/// prelude regardless of lazy mode).
pub fn is_core_tool(name: &str) -> bool {
    CORE_TOOL_NAMES.contains(&name)
}

pub const FIXED_V1_TOOL_NAMES: &[&str] = &[
    "read_file",
    "edit_file",
    "write_file",
    "shell",
    "search",
    "tool_search",
    "todo_write",
    "ask_user",
    "exec",
];

// ── P0-C 2a: monotonic promotion ────────────────────────────────────────────

/// Per-call cap on the number of deferred/promoted tool schemas serialized
/// into a single request's tool block. Kept unchanged from the pre-lane
/// value; distinct from [`MAX_ACCUMULATED_PROMOTED_TOOLS`], the session
/// ceiling.
pub const MAX_SCOPED_DEFERRED_TOOL_SCHEMAS: usize = 16;

/// Session-lifetime ceiling on the accumulated (monotonic, never-demoted)
/// promoted-tool set. A session may discover more tools via `tool_search`
/// than fit in one request's schema block; this bounds unbounded growth of
/// the tracked set itself. Seeded per P0-C DECISION-4 — tune later.
pub const MAX_ACCUMULATED_PROMOTED_TOOLS: usize = 48;

/// Hard ceiling on serialized tool-schema bytes for one request. Exceeding
/// it is a typed error ([`ToolSchemaBudgetExceeded`]), never a silent
/// overrun of the provider's context window by an unbounded schema block.
pub const MAX_TOOL_SCHEMA_BYTES: usize = 256 * 1024;

/// One promotion recorded at a turn boundary, with an attributable reason
/// for telemetry (P0-C 2d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPromotionEvent {
    pub tool_name: String,
    pub reason: &'static str,
}

/// Session-lifetime, monotonically-growing set of deferred tool names that
/// `tool_search` has scoped into visibility. Tools are never demoted: once
/// promoted, a tool stays a promotion candidate for the rest of the session.
/// Evaluate this only at turn boundaries — once per outbound provider
/// request, never mid-turn — via [`PromotedToolState::accumulate`].
#[derive(Debug, Default, Clone)]
pub struct PromotedToolState {
    promoted: std::collections::HashSet<String>,
    /// Names ordered by last-scoped position: the first element is the tool
    /// scoped longest ago, the last is the most recently (re-)scoped.
    /// `scoped_for_request` reads from the tail, so the per-call schema cap
    /// is a recency window — the tools the model most recently discovered
    /// ship, older promotions age out of the block but stay dispatchable by
    /// name and re-enter on re-scope. (CQ-11: the previous take-from-front
    /// priority permanently starved every promotion past the cap.)
    order: Vec<String>,
}

impl PromotedToolState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate one turn boundary: scan `messages` for `tool_search` results
    /// and fold any newly-discovered tool names into the accumulated set.
    /// Idempotent for an unchanged message list — calling it twice with the
    /// same `messages` yields the same state and an empty event list the
    /// second time. Returns every promotion (or promotion-refused-by-ceiling)
    /// event recorded this turn.
    ///
    /// `scoped_deferred_tool_names` is deterministic (transcript order, then
    /// name order within one result), so `order` never depends on HashSet
    /// iteration. Re-scoping an already-promoted name refreshes its recency
    /// position without emitting a second promotion event or touching the
    /// ceiling.
    pub fn accumulate(
        &mut self,
        messages: &[rightkit_agent_engine_protocol::TranscriptMessage],
    ) -> Vec<ToolPromotionEvent> {
        let mut events = Vec::new();
        for name in scoped_deferred_tool_names(messages) {
            if self.promoted.contains(&name) {
                // Re-scope: refresh recency so a still-needed tool is not
                // aged out of the serialization window by unrelated later
                // promotions.
                if let Some(pos) = self.order.iter().position(|n| *n == name) {
                    let name = self.order.remove(pos);
                    self.order.push(name);
                }
                continue;
            }
            if self.promoted.len() >= MAX_ACCUMULATED_PROMOTED_TOOLS {
                events.push(ToolPromotionEvent {
                    tool_name: name,
                    reason: "accumulated-ceiling-reached",
                });
                continue;
            }
            self.promoted.insert(name.clone());
            self.order.push(name.clone());
            events.push(ToolPromotionEvent {
                tool_name: name,
                reason: "tool_search-scoped",
            });
        }
        events
    }

    /// Every tool ever promoted this session (monotonic — never shrinks).
    pub fn promoted_names(&self) -> &std::collections::HashSet<String> {
        &self.promoted
    }

    pub fn promoted_count(&self) -> usize {
        self.promoted.len()
    }

    /// The subset of the accumulated set that fits the per-call serialization
    /// cap — the most recently scoped tools win slots, so a promotion beyond
    /// the cap still ships and re-scoping a starved-out tool restores it.
    pub fn scoped_for_request(&self) -> std::collections::HashSet<String> {
        self.order
            .iter()
            .rev()
            .take(MAX_SCOPED_DEFERRED_TOOL_SCHEMAS)
            .cloned()
            .collect()
    }
}

/// Names scoped by `tool_search` results in `messages`, ordered by
/// last-scoped position (earliest-scoped first). Deterministic: transcript
/// order for the results, sorted names within one result — never HashSet
/// iteration order.
fn scoped_deferred_tool_names(
    messages: &[rightkit_agent_engine_protocol::TranscriptMessage],
) -> Vec<String> {
    use rightkit_agent_engine_protocol::TranscriptMessage;

    let tool_search_call_ids = messages
        .iter()
        .filter_map(|message| match message {
            TranscriptMessage::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flat_map(|tool_calls| tool_calls.iter())
        .filter(|call| call.name == "tool_search")
        .map(|call| call.id.clone())
        .collect::<std::collections::HashSet<_>>();

    if tool_search_call_ids.is_empty() {
        return Vec::new();
    }

    // Union every tool_search result in this message list — not just the
    // most recent one — so a tool found in an earlier call is not dropped
    // once a later, unrelated tool_search result becomes the newest. This is
    // the direct fix for the pre-lane `.rev().find_map(...)` bug: a tool
    // found in call 1 used to disappear from the prelude after call 2.
    // A name moves to the end each time it is scoped, so position encodes
    // last-scoped recency.
    let mut names: Vec<String> = Vec::new();
    for message in messages {
        if let TranscriptMessage::Tool {
            tool_call_id,
            summary,
            is_error: false,
            ..
        } = message
        {
            if tool_search_call_ids.contains(tool_call_id) {
                if let Some(found) = tool_names_from_tool_search_result(summary) {
                    let mut found: Vec<String> = found.into_iter().collect();
                    found.sort();
                    for name in found {
                        if let Some(pos) = names.iter().position(|n| *n == name) {
                            names.remove(pos);
                        }
                        names.push(name);
                    }
                }
            }
        }
    }
    names
}

fn tool_names_from_tool_search_result(summary: &str) -> Option<std::collections::HashSet<String>> {
    let values = serde_json::from_str::<serde_json::Value>(summary).ok()?;
    let names = values
        .as_array()?
        .iter()
        .filter_map(|item| item.get("name").and_then(|name| name.as_str()))
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.to_string())
        .collect::<std::collections::HashSet<_>>();
    Some(names)
}

// ── P0-C 2b/2c: canonical ordering + schema-byte ceiling ────────────────────

/// Typed error for DoD (1): schema bytes exceeding the configured ceiling is
/// a typed error, never a silent overrun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSchemaBudgetExceeded {
    pub bytes: usize,
    pub ceiling: usize,
}

impl std::fmt::Display for ToolSchemaBudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tool schema block is {} bytes, exceeding the {}-byte ceiling",
            self.bytes, self.ceiling
        )
    }
}

impl std::error::Error for ToolSchemaBudgetExceeded {}

fn process_resource_available(tool: &dyn Tool) -> bool {
    let Some(kind) = tool.process_resource_kind() else {
        return true;
    };

    #[cfg(feature = "coding_tools-host")]
    let available = crate::coding_tools::process_resource_authority::process_resource_governor()
        .map(|governor| governor.capability_available(kind))
        .unwrap_or(true);

    #[cfg(not(feature = "coding_tools-host"))]
    let available = {
        let _ = kind;
        false
    };

    available
}

/// Build the provider-facing tool definition block for one request.
///
/// Canonical order (P0-C 2b): tools are sorted by name before
/// serialization, so a membership change appends rather than reshuffling
/// every downstream byte — a precondition for the cache-prefix linkage in
/// `tool_defs_for_messages_checked` / provider_client.rs (P0-C 2c).
///
/// `promoted` supplies the session's accumulated promotion state; deferred
/// tools not in `promoted.scoped_for_request()` are withheld, exactly as
/// before this lane. A deferred tool withheld here remains fully registered
/// and dispatchable by name — this only changes what ships in the request's
/// `tools` block.
pub fn tool_defs_for_messages(
    tools: &ToolRegistry,
    promoted: &PromotedToolState,
) -> Vec<serde_json::Value> {
    if tools.model_interface() == ModelToolInterface::FixedV1 {
        return fixed_v1_tool_defs(tools);
    }
    let scoped_deferred_tools = promoted.scoped_for_request();
    let mut selected: Vec<&Box<dyn Tool>> = tools
        .all()
        .iter()
        .filter(|tool| process_resource_available(tool.as_ref()))
        .filter(|tool| {
            let deferred_by_tool = tool.defer_schema_by_default();
            let deferred_by_lazy_mode = !tools.is_prelude_tool(tool.name());
            let is_deferred = deferred_by_tool || deferred_by_lazy_mode;
            !is_deferred || scoped_deferred_tools.contains(tool.name())
        })
        .collect();
    selected.sort_by(|a, b| a.name().cmp(b.name()));
    selected
        .into_iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.input_schema(),
                }
            })
        })
        .collect()
}

pub fn fixed_v1_tool_defs(tools: &ToolRegistry) -> Vec<serde_json::Value> {
    FIXED_V1_TOOL_NAMES
        .iter()
        .filter_map(|name| tools.get(name))
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.input_schema(),
                }
            })
        })
        .collect()
}

/// [`tool_defs_for_messages`], failing closed with a typed error when the
/// serialized block exceeds [`MAX_TOOL_SCHEMA_BYTES`].
pub fn tool_defs_for_messages_checked(
    tools: &ToolRegistry,
    promoted: &PromotedToolState,
) -> Result<Vec<serde_json::Value>, ToolSchemaBudgetExceeded> {
    let defs = tool_defs_for_messages(tools, promoted);
    let bytes = serde_json::to_vec(&defs)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if bytes > MAX_TOOL_SCHEMA_BYTES {
        return Err(ToolSchemaBudgetExceeded {
            bytes,
            ceiling: MAX_TOOL_SCHEMA_BYTES,
        });
    }
    Ok(defs)
}

/// Convenience, stateless wrapper: builds a fresh [`PromotedToolState`] from
/// the full `messages` history in one shot. Byte-identical to the pre-lane
/// helper's contract for a single message-history snapshot (every existing
/// caller that recomputes from full history each call keeps working), but
/// production call sites should thread a persistent `PromotedToolState`
/// through `accumulate` at each turn boundary instead, so promotion stays
/// monotonic across calls that each see only a growing message window.
pub fn tool_defs_for_full_history(
    tools: &ToolRegistry,
    messages: &[rightkit_agent_engine_protocol::TranscriptMessage],
) -> Vec<serde_json::Value> {
    let mut promoted = PromotedToolState::new();
    promoted.accumulate(messages);
    tool_defs_for_messages(tools, &promoted)
}
