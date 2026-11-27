//! `sleep` — sleep for N milliseconds (capped at 60s).
//!
//! Used for timing, polling, and test delays. Returns elapsed time in ms.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct SleepTool;

const MAX_SLEEP_MS: u64 = 60_000; // 60 seconds

host_option!(
    /// An operator cancel (`ToolContext::cancel_rx`) ends a running `sleep` at once and returns an
    /// error naming the milliseconds slept, instead of waiting out the rest of it. `false` (the
    /// default) sleeps the full duration.
    SLEEP_HONOURS_CANCEL,
    set_sleep_honours_cancel,
    sleep_honours_cancel
);

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

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
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
        if sleep_honours_cancel() {
            // An operator cancel ends the sleep at once instead of waiting out the rest of it.
            let cancelled = async {
                let Some(mut rx) = ctx.cancel_rx.clone() else {
                    return std::future::pending::<()>().await;
                };
                loop {
                    if *rx.borrow_and_update() {
                        return;
                    }
                    if rx.changed().await.is_err() {
                        return std::future::pending::<()>().await;
                    }
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(ms)) => {}
                _ = cancelled => {
                    return ToolResult::error(format!(
                        "Sleep cancelled by operator after {} ms",
                        start.elapsed().as_millis()
                    ));
                }
            }
        } else {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
        let elapsed = start.elapsed().as_millis() as u64;

        ToolResult::ok(format!("Slept for {} ms (requested: {} ms)", elapsed, ms))
    }
}
