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

    /// Last-scoped order, oldest first. Test-only evidence hook for
    /// deterministic-ordering proofs.
    #[cfg(test)]
    fn order_snapshot(&self) -> &[String] {
        &self.order
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
        .filter_map(|name| tools.get(*name))
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

#[cfg(test)]
mod lazy_loading_tests {
    use super::*;

    #[test]
    fn core_set_includes_read_edit_shell_search_and_tool_search() {
        for name in [
            "read_file",
            "write_file",
            "edit_file",
            "shell",
            "search",
            "tool_search",
        ] {
            assert!(is_core_tool(name), "{name} should be core");
        }
    }

    #[test]
    fn non_core_tool_is_not_core() {
        assert!(!is_core_tool("github"));
        assert!(!is_core_tool("web_search"));
        assert!(!is_core_tool("browser_fetch"));
    }

    #[test]
    fn lazy_registry_requires_an_explicit_opt_in() {
        let default_registry = ToolRegistry::new();
        assert!(!default_registry.lazy_loading_enabled());
        assert!(default_registry.is_prelude_tool("web_search"));

        let enabled_registry = ToolRegistry::new().with_lazy_loading(true);
        assert!(enabled_registry.lazy_loading_enabled());
        assert!(!enabled_registry.is_prelude_tool("web_search"));
        assert!(enabled_registry.is_prelude_tool("read_file"));

        // An explicit false restores the full-prelude behavior; a previous
        // opt-in must never leak into a newly constructed registry.
        let disabled_registry = ToolRegistry::new().with_lazy_loading(false);
        assert!(!disabled_registry.lazy_loading_enabled());
        assert!(disabled_registry.is_prelude_tool("web_search"));
    }

    #[test]
    fn promotion_is_monotonic_across_turn_boundaries() {
        use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};

        let mut state = PromotedToolState::new();

        // Turn 1: tool_search finds `alpha`.
        let turn1 = vec![
            TranscriptMessage::Assistant {
                id: "a1".into(),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "s1".into(),
                    name: "tool_search".into(),
                    arguments_json: "{}".into(),
                }],
                pinned: false,
            },
            TranscriptMessage::Tool {
                id: "r1".into(),
                created_at: "t".into(),
                tool_call_id: "s1".into(),
                summary: serde_json::json!([{"name": "alpha"}]).to_string(),
                output: None,
                trust: None,
                is_error: false,
                process_id: None,
                pinned: false,
            },
        ];
        let events1 = state.accumulate(&turn1);
        assert_eq!(events1.len(), 1);
        assert!(state.promoted_names().contains("alpha"));

        // Turn 2: a second, unrelated tool_search call/result is the newest
        // in the window, but `alpha` must remain promoted (never demoted) —
        // this is the exact bug the `.rev().find_map(...)` predecessor had.
        let mut turn2 = turn1.clone();
        turn2.push(TranscriptMessage::Assistant {
            id: "a2".into(),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "s2".into(),
                name: "tool_search".into(),
                arguments_json: "{}".into(),
            }],
            pinned: false,
        });
        turn2.push(TranscriptMessage::Tool {
            id: "r2".into(),
            created_at: "t".into(),
            tool_call_id: "s2".into(),
            summary: serde_json::json!([{"name": "beta"}]).to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
        let events2 = state.accumulate(&turn2);
        assert_eq!(events2.len(), 1);
        assert!(state.promoted_names().contains("alpha"));
        assert!(state.promoted_names().contains("beta"));
        assert_eq!(state.promoted_count(), 2);

        // Re-accumulating the same window is idempotent — no new events.
        assert!(state.accumulate(&turn2).is_empty());
    }

    #[test]
    fn accumulated_ceiling_stops_growth_but_never_demotes() {
        use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};

        let mut state = PromotedToolState::new();
        // Fill to the ceiling with distinct tool names in one turn.
        let names: Vec<String> = (0..MAX_ACCUMULATED_PROMOTED_TOOLS)
            .map(|i| format!("tool{i}"))
            .collect();
        let summary = serde_json::to_string(
            &names
                .iter()
                .map(|n| serde_json::json!({"name": n}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let full = vec![
            TranscriptMessage::Assistant {
                id: "a1".into(),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "s1".into(),
                    name: "tool_search".into(),
                    arguments_json: "{}".into(),
                }],
                pinned: false,
            },
            TranscriptMessage::Tool {
                id: "r1".into(),
                created_at: "t".into(),
                tool_call_id: "s1".into(),
                summary,
                output: None,
                trust: None,
                is_error: false,
                process_id: None,
                pinned: false,
            },
        ];
        state.accumulate(&full);
        assert_eq!(state.promoted_count(), MAX_ACCUMULATED_PROMOTED_TOOLS);

        // One more tool, in a later turn, is refused by the ceiling — but
        // every prior promotion is still present (never demoted).
        let mut later = full.clone();
        later.push(TranscriptMessage::Assistant {
            id: "a2".into(),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "s2".into(),
                name: "tool_search".into(),
                arguments_json: "{}".into(),
            }],
            pinned: false,
        });
        later.push(TranscriptMessage::Tool {
            id: "r2".into(),
            created_at: "t".into(),
            tool_call_id: "s2".into(),
            summary: serde_json::json!([{"name": "one-too-many"}]).to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
        let events = state.accumulate(&later);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, "accumulated-ceiling-reached");
        assert_eq!(state.promoted_count(), MAX_ACCUMULATED_PROMOTED_TOOLS);
        assert!(!state.promoted_names().contains("one-too-many"));
        for name in &names {
            assert!(state.promoted_names().contains(name));
        }
    }

    #[test]
    fn ordering_is_canonical_and_stable_for_identical_tool_sets() {
        let registry = ToolRegistry::build_default();
        let promoted = PromotedToolState::new();
        let first = tool_defs_for_messages(&registry, &promoted);
        let second = tool_defs_for_messages(&registry, &promoted);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap(),
            "identical tool sets must produce byte-identical serialization"
        );
        // Sorted by name: every adjacent pair is non-decreasing.
        let names: Vec<&str> = first
            .iter()
            .filter_map(|def| def["function"]["name"].as_str())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn schema_byte_ceiling_is_a_typed_error_not_a_silent_overrun() {
        struct HugeTool;
        #[async_trait::async_trait]
        impl Tool for HugeTool {
            fn name(&self) -> &str {
                "huge_tool"
            }
            fn description(&self) -> &str {
                "huge"
            }
            fn input_schema(&self) -> serde_json::Value {
                // A single property whose description alone blows the ceiling.
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "p": {"type": "string", "description": "x".repeat(MAX_TOOL_SCHEMA_BYTES + 1024)}
                    }
                })
            }
            async fn execute(
                &self,
                _args: serde_json::Value,
                _ctx: &crate::coding_tools::ToolContext,
            ) -> crate::coding_tools::ToolResult {
                crate::coding_tools::ToolResult::ok("ok")
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HugeTool));
        let promoted = PromotedToolState::new();
        let result = tool_defs_for_messages_checked(&registry, &promoted);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.bytes > MAX_TOOL_SCHEMA_BYTES);
        assert_eq!(error.ceiling, MAX_TOOL_SCHEMA_BYTES);
    }

    /// One tool_search exchange (assistant call + result) naming `names`.
    fn tool_search_turn(
        call_id: &str,
        names: &[String],
    ) -> Vec<rightkit_agent_engine_protocol::TranscriptMessage> {
        use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};
        vec![
            TranscriptMessage::Assistant {
                id: format!("a-{call_id}"),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: call_id.into(),
                    name: "tool_search".into(),
                    arguments_json: "{}".into(),
                }],
                pinned: false,
            },
            TranscriptMessage::Tool {
                id: format!("r-{call_id}"),
                created_at: "t".into(),
                tool_call_id: call_id.into(),
                summary: serde_json::to_string(
                    &names
                        .iter()
                        .map(|n| serde_json::json!({"name": n}))
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
                output: None,
                trust: None,
                is_error: false,
                process_id: None,
                pinned: false,
            },
        ]
    }

    /// CQ-11: promotion order is deterministic — independent states fed the
    /// same history agree on `order`, so which tools ship never depends on
    /// HashSet iteration order.
    #[test]
    fn promotion_order_is_deterministic_across_states() {
        let names: Vec<String> = (0..8).map(|i| format!("tool{i}")).collect();
        let messages = tool_search_turn("s1", &names);

        let mut a = PromotedToolState::new();
        let mut b = PromotedToolState::new();
        a.accumulate(&messages);
        b.accumulate(&messages);
        assert_eq!(
            a.order_snapshot(),
            b.order_snapshot(),
            "identical histories must produce identical promotion order"
        );
        // Within one result names append in sorted order — the scan is not
        // HashSet-iteration order.
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(a.order_snapshot(), sorted.as_slice());
    }

    /// CQ-11: the per-call schema cap is a recency window, not a permanent
    /// first-come lock — promotions beyond the cap still ship (the
    /// earliest-scoped age out), and re-scoping an aged-out tool restores it.
    #[test]
    fn later_promotions_enter_the_recency_window() {
        let mut state = PromotedToolState::new();
        let over_cap = MAX_SCOPED_DEFERRED_TOOL_SCHEMAS + 4;
        let names: Vec<String> = (0..over_cap).map(|i| format!("tool{i:02}")).collect();
        state.accumulate(&tool_search_turn("s1", &names));

        let scoped = state.scoped_for_request();
        assert_eq!(scoped.len(), MAX_SCOPED_DEFERRED_TOOL_SCHEMAS);
        // Sorted append order means tool04..tool19 are the most recently
        // scoped — the later names ship; the earliest four wait.
        for evicted in ["tool00", "tool01", "tool02", "tool03"] {
            assert!(
                !scoped.contains(evicted),
                "{evicted} should age out of the recency window"
            );
        }
        assert!(scoped.contains("tool19"), "latest promotions must ship");

        // A later tool_search re-scopes the starved tool — it re-enters the
        // window (evicting tool04, now the oldest) instead of starving forever.
        let mut messages = tool_search_turn("s1", &names);
        messages.extend(tool_search_turn("s2", &["tool00".to_string()]));
        state.accumulate(&messages);
        let scoped = state.scoped_for_request();
        assert!(
            scoped.contains("tool00"),
            "a re-scoped tool must re-enter the window"
        );
        assert!(
            !scoped.contains("tool04"),
            "the oldest unrescoped tool yields its slot"
        );
        // Re-scope is not a second promotion: no new event, ceiling untouched.
        let events = state.accumulate(&messages);
        assert!(events.is_empty(), "idempotent re-scan emits nothing");
        assert_eq!(state.promoted_count(), over_cap);
    }
}
