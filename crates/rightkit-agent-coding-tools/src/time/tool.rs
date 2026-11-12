// Time and timezone tool — built-in, no external server needed.
//
// Provides current time in any IANA timezone and converts between timezones.
// Agents need this for logging, scheduling, date math, and timezone-aware responses.

use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};

pub struct TimeTool;

#[async_trait::async_trait]
impl Tool for TimeTool {
    fn name(&self) -> &str {
        "get_time"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Current time/date, timezone conversion (IANA names like America/New_York, Asia/Tokyo, UTC or offset like +05:30), and duration calc. Operations: current_time, convert (ISO 8601), diff."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["current_time", "convert", "diff"],
                    "description": "current_time: get now in one or more timezones. convert: convert a specific datetime to another timezone. diff: compute duration between two datetimes."
                },
                "timezone": {
                    "type": "string",
                    "description": "IANA timezone for current_time (default: UTC). E.g. 'America/New_York'."
                },
                "timezones": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of IANA timezones to show simultaneously (current_time only)."
                },
                "datetime": {
                    "type": "string",
                    "description": "ISO 8601 datetime string for convert/diff operations (e.g. '2026-06-02T14:30:00Z')."
                },
                "from_timezone": {
                    "type": "string",
                    "description": "Source IANA timezone for convert operation."
                },
                "to_timezone": {
                    "type": "string",
                    "description": "Target IANA timezone for convert operation."
                },
                "start": {
                    "type": "string",
                    "description": "Start ISO 8601 datetime for diff operation."
                },
                "end": {
                    "type": "string",
                    "description": "End ISO 8601 datetime for diff operation."
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let operation = match args.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return ToolResult::error("missing required field 'operation'"),
        };

        match operation {
            "current_time" => current_time(&args),
            "convert" => convert_time(&args),
            "diff" => time_diff(&args),
            other => ToolResult::error(format!(
                "unknown operation '{other}'; use 'current_time', 'convert', or 'diff'"
            )),
        }
    }
}

// ── Unix timestamp → naive datetime components (UTC) ──────────────────────
