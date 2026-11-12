//! CR3 #57 item 6: supervise non-provider awaits with the turn budget.
//!
//! Provider calls have their own deadlines. The awaits that remain (MCP calls,
//! membrane CLI tools, network-bound tools) previously had no turn-level bound,
//! so a wedged peer could hold a turn in `running` forever. The deadline is
//! derived from the session's configured turn budget (`max_iterations`) using
//! the same 45s-per-turn scale and 60..=900s clamp as the CLI subagent timeout,
//! and expiry yields a typed [`NonProviderAwaitTimeout`] surfaced as an error
//! tool result (never a silent success).

use std::future::Future;
use std::time::Duration;

pub(crate) const SECONDS_PER_BUDGET_TURN: u64 = 45;
pub(crate) const MIN_AWAIT_SECONDS: u64 = 60;
pub(crate) const MAX_AWAIT_SECONDS: u64 = 900;
/// Stable machine-readable prefix of the timeout tool result.
pub const TIMEOUT_CODE: &str = "non_provider_await_timeout";

/// Typed timeout for a supervised non-provider await.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonProviderAwaitTimeout {
    pub tool: String,
    pub seconds: u64,
}

impl std::fmt::Display for NonProviderAwaitTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{TIMEOUT_CODE}: tool `{}` did not finish within the {}s turn-budget deadline and was cancelled",
            self.tool, self.seconds
        )
    }
}

impl std::error::Error for NonProviderAwaitTimeout {}

/// Deadline for one non-provider await, scaled from the turn budget.
pub(crate) fn non_provider_await_deadline(max_iterations: usize) -> Duration {
    let seconds = (max_iterations as u64)
        .saturating_mul(SECONDS_PER_BUDGET_TURN)
        .clamp(MIN_AWAIT_SECONDS, MAX_AWAIT_SECONDS);
    Duration::from_secs(seconds)
}

/// Tools whose await is on a non-provider peer and has no deadline of its own.
/// Interactive waits (questions, approvals), subagent waits, sleeps, and shell
/// or process tools (which carry their own timeouts) are deliberately excluded.
pub(crate) fn is_supervised_tool(name: &str) -> bool {
    name.starts_with("mcp__")
        || matches!(
            name,
            "mcp_call"
                | "read_mcp_resource"
                | "list_mcp_resources"
                | "list_mcp_tools"
                | "mcp_auth"
                | "browser_fetch"
                | "recursive_fetch"
                | "web_search"
                | "github"
                | "cloudflare"
                | "flyio"
                | "image_generate"
        )
}

/// Await `fut`, bounded by `deadline` when `tool` is a supervised tool.
pub(crate) async fn supervise_tool_await<F>(
    tool: &str,
    deadline: Duration,
    fut: F,
) -> rightkit_agent_coding_tools::ToolResult
where
    F: Future<Output = rightkit_agent_coding_tools::ToolResult>,
{
    if !is_supervised_tool(tool) {
        return fut.await;
    }
    match tokio::time::timeout(deadline, fut).await {
        Ok(result) => result,
        Err(_) => {
            let timeout = NonProviderAwaitTimeout {
                tool: tool.to_string(),
                seconds: deadline.as_secs(),
            };
            tracing::warn!(
                tool,
                seconds = timeout.seconds,
                "non-provider await timed out"
            );
            rightkit_agent_coding_tools::ToolResult::error(timeout.to_string())
        }
    }
}
