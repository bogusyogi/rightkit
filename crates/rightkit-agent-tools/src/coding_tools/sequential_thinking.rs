//! Sequential thinking — built-in structured reasoning tool.
//!
//! Allows the agent to think through problems step-by-step, revise earlier thoughts,
//! and branch hypotheses. Each call adds one thought to the session's chain and
//! returns the current state. No external server needed.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Thought {
    number: u32,
    total: u32,
    text: String,
    is_revision: bool,
    revises_thought: Option<u32>,
    branch_from: Option<u32>,
    branch_id: Option<String>,
}

/// Per-session thought chain. Stored as a shared Arc so the tool instance
/// (which is cloned per-session) owns its own independent chain.
#[derive(Debug, Default)]
struct ThoughtChain {
    thoughts: Vec<Thought>,
}

pub struct SequentialThinkingTool {
    chain: Arc<Mutex<ThoughtChain>>,
}

impl SequentialThinkingTool {
    pub fn new() -> Self {
        Self {
            chain: Arc::new(Mutex::new(ThoughtChain::default())),
        }
    }
}

impl Default for SequentialThinkingTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for SequentialThinkingTool {
    fn name(&self) -> &str {
        "sequentialthinking"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Step-by-step reasoning chain (use for complex analysis). Each call adds one thought; revise or branch earlier ones. Final thought: nextThoughtNeeded=false."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "thought": {
                    "type": "string",
                    "description": "The current reasoning step."
                },
                "thoughtNumber": {
                    "type": "integer",
                    "description": "1-based index of this thought.",
                    "minimum": 1
                },
                "totalThoughts": {
                    "type": "integer",
                    "description": "Estimated total thoughts needed (can be revised upward).",
                    "minimum": 1
                },
                "nextThoughtNeeded": {
                    "type": "boolean",
                    "description": "true if more thoughts are needed; false when reasoning is complete."
                },
                "isRevision": {
                    "type": "boolean",
                    "description": "true if this thought revises an earlier one."
                },
                "revisesThought": {
                    "type": "integer",
                    "description": "Which thought number this revises (when isRevision=true)."
                },
                "branchFromThought": {
                    "type": "integer",
                    "description": "Branch from this thought number to explore an alternative."
                },
                "branchId": {
                    "type": "string",
                    "description": "Identifier for this branch (e.g. 'hypothesis-a')."
                }
            },
            "required": ["thought", "thoughtNumber", "totalThoughts", "nextThoughtNeeded"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let thought_text = match args.get("thought").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolResult::error("missing required field 'thought'"),
        };
        let number = args
            .get("thoughtNumber")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32;
        let total = args
            .get("totalThoughts")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32;
        let next_needed = args
            .get("nextThoughtNeeded")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let is_revision = args
            .get("isRevision")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let revises = args
            .get("revisesThought")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let branch_from = args
            .get("branchFromThought")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let branch_id = args
            .get("branchId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let thought = Thought {
            number,
            total,
            text: thought_text.clone(),
            is_revision,
            revises_thought: revises,
            branch_from,
            branch_id,
        };

        let chain_len = {
            let mut chain = self.chain.lock().unwrap();
            chain.thoughts.push(thought);
            chain.thoughts.len()
        };

        let status = if next_needed {
            format!("Thought {number}/{total} recorded. Continue reasoning.")
        } else {
            format!("Thought {number}/{total} recorded. Reasoning complete ({chain_len} thoughts total).")
        };

        ToolResult::ok(
            serde_json::json!({
                "thought": thought_text,
                "thoughtNumber": number,
                "totalThoughts": total,
                "nextThoughtNeeded": next_needed,
                "status": status
            })
            .to_string(),
        )
    }
}
