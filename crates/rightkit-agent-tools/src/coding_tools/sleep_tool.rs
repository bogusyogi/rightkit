//! `sleep` — sleep for N milliseconds (capped at 60s).
//!
//! Used for timing, polling, and test delays. Returns elapsed time in ms.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct SleepTool;

const MAX_SLEEP_MS: u64 = 60_000; // 60 seconds

#[async_trait::async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Sleep for N milliseconds (capped at 60s). Returns elapsed time. Use for timing, polling, test delays. Fail: duration > 60000ms."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["milliseconds"],
            "properties": {
                "milliseconds": {
                    "type": "integer",
                    "description": "Duration to sleep in milliseconds (0-60000)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let ms = match args["milliseconds"].as_u64() {
            Some(m) => m,
            None => {
                return ToolResult::error(
                    "Missing required argument: milliseconds (must be a non-negative integer)",
                )
            }
        };

        if ms > MAX_SLEEP_MS {
            return ToolResult::error(format!(
                "Sleep duration {} ms exceeds maximum of {} ms",
                ms, MAX_SLEEP_MS
            ));
        }

        let start = std::time::Instant::now();
        tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        let elapsed = start.elapsed().as_millis() as u64;

        ToolResult::ok(format!("Slept for {} ms (requested: {} ms)", elapsed, ms))
    }
}
