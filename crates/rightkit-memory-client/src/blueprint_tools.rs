//! Local aliases for read-only Membrane operations, served by Pulse's Membrane CLI.
//! Only `blueprint_search` has a CLI equivalent; the rest return a clear error.

use crate::membrane::MembraneBackend as _;
use rightkit_agent_coding_tools::context::ToolContext;
use rightkit_agent_coding_tools::{Tool, ToolRegistry, ToolResult};
use serde_json::{json, Map, Value};

pub const TOOL_NAMES: [&str; 5] = [
    "blueprint_recall",
    "blueprint_search",
    "blueprint_expand",
    "membrane_memory_read",
    "membrane_source_read",
];

pub fn register(registry: &mut ToolRegistry, allowed: impl Fn(&str) -> bool) {
    for name in TOOL_NAMES {
        if allowed(name) {
            registry.register(Box::new(CanonicalReadTool::new(name)));
        }
    }
}

struct CanonicalReadTool {
    name: &'static str,
}

impl CanonicalReadTool {
    fn new(name: &'static str) -> Self {
        Self { name }
    }

    fn schema(&self) -> Value {
        match self.name {
            "blueprint_recall" => json!({
                "type":"object","additionalProperties":false,
                "properties":{"query":{"type":"string"},"task":{"type":"string"},
                    "generationId":{"type":"string","minLength":1},
                    "limit":{"type":"integer","minimum":1,"maximum":64},
                    "budget":{"type":"integer","minimum":1,"maximum":4000},
                    "deadlineMs":{"type":"integer","minimum":10,"maximum":30000}}
            }),
            "blueprint_search" => json!({
                "type":"object","additionalProperties":false,"required":["query"],
                "properties":{"query":{"type":"string","minLength":1},
                    "generationId":{"type":"string","minLength":1},
                    "limit":{"type":"integer","minimum":1,"maximum":64},
                    "budget":{"type":"integer","minimum":1,"maximum":4000},
                    "deadlineMs":{"type":"integer","minimum":10,"maximum":30000}}
            }),
            "blueprint_expand" => json!({
                "type":"object","additionalProperties":false,"required":["anchor"],
                "properties":{"anchor":{"type":"string","minLength":1},
                    "depth":{"type":"integer","minimum":1,"maximum":5},
                    "generationId":{"type":"string","minLength":1},
                    "limit":{"type":"integer","minimum":1,"maximum":64},
                    "budget":{"type":"integer","minimum":1,"maximum":4000},
                    "deadlineMs":{"type":"integer","minimum":10,"maximum":30000}}
            }),
            "membrane_memory_read" => json!({
                "type":"object","additionalProperties":false,
                "required":["id","expectedContentHash"],
                "properties":{"id":{"type":"string","minLength":1},
                    "expectedContentHash":{"type":"string","pattern":"^sha256:[a-f0-9]{64}$"},
                    "offset":{"type":"integer","minimum":0,"maximum":1000000},
                    "maxChars":{"type":"integer","minimum":1,"maximum":12000}}
            }),
            "membrane_source_read" => json!({
                "type":"object","additionalProperties":false,
                "required":["sourceRef","anchorId","expectedContentHash"],
                "properties":{"sourceRef":{"type":"string","minLength":1},
                    "anchorId":{"type":"string","minLength":1},
                    "expectedContentHash":{"type":"string","minLength":1}}
            }),
            _ => unreachable!("unknown canonical read alias"),
        }
    }
}

#[async_trait::async_trait]
impl Tool for CanonicalReadTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        match self.name {
            "blueprint_recall" => {
                "Recall current repository Blueprint context through canonical Membrane."
            }
            "blueprint_search" => {
                "Search the selected repository's canonical Membrane Blueprint graph."
            }
            "blueprint_expand" => {
                "Expand a bounded neighborhood in the canonical Membrane Blueprint graph."
            }
            "membrane_memory_read" => {
                "Read one exact Membrane memory by identity and content hash."
            }
            "membrane_source_read" => "Read one exact Ledger source through canonical Membrane.",
            _ => "Canonical Membrane read operation.",
        }
    }

    fn input_schema(&self) -> Value {
        self.schema()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Mcp)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        const NOT_AVAILABLE: &str = "not available via Membrane CLI (Orthic-Labs/pulse#129)";
        if self.name != "blueprint_search" {
            return ToolResult::error(format!("{} is {NOT_AVAILABLE}", self.name));
        }
        if ctx.cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            return ToolResult::error(format!("{} cancelled", self.name));
        }
        let Some(object) = args.as_object() else {
            return ToolResult::error(format!("{} requires object arguments", self.name));
        };
        let Some(query) = object
            .get("query")
            .and_then(Value::as_str)
            .filter(|q| !q.trim().is_empty())
            .map(str::to_owned)
        else {
            return ToolResult::error("blueprint_search requires query");
        };
        let limit = object
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(10, |n| n.clamp(1, 64) as usize);
        let repo = ctx.workspace_root.clone();
        // The adapter enforces its own process timeout; run it off the async
        // threads so a slow Membrane never stalls the runtime.
        let result = tokio::task::spawn_blocking(move || {
            crate::membrane::MembraneCli::backend().blueprint_search(
                &query,
                limit,
                Some(repo.as_path()),
            )
        })
        .await;
        match result {
            Ok(Ok(answer)) => {
                let mut doc = match answer.body {
                    Value::Object(map) => map,
                    other => {
                        let mut map = Map::new();
                        map.insert("body".into(), other);
                        map
                    }
                };
                doc.insert("schema".into(), Value::String(answer.schema));
                if let Some(payload_schema) = answer.payload_schema {
                    doc.insert("payload_schema".into(), Value::String(payload_schema));
                }
                ToolResult::ok(Value::Object(doc).to_string())
            }
            Ok(Err(error)) => ToolResult::error(format!("{} unavailable: {error}", self.name)),
            Err(error) => ToolResult::error(format!("{} failed: {error}", self.name)),
        }
    }
}
