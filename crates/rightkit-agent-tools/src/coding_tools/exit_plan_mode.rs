//! `exit_plan_mode` — present a plan and leave Plan mode on approval
//! (Claude-Code `ExitPlanMode` parity).
//!
//! Validates the plan, calls `ctx.plan_exit.present(plan)` to surface it to the
//! operator, and on approval reports that the session has left Plan mode (the
//! daemon bridge flips the permission mode to AcceptEdits).

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct ExitPlanModeTool;

#[async_trait::async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "exit_plan_mode"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        // Presenting the plan is itself the operator consent surface. Without a declared
        // class it falls to `Unknown`, which Plan mode denies, so the plan could never be
        // presented from the one mode that exists to present it.
        Some(rightkit_agent_policy::CapabilityClass::ReadFile)
    }

    fn description(&self) -> &str {
        "Present your implementation plan and ask the operator to approve leaving Plan mode so you \
         can start making changes. Call this only after you've finished researching and have a \
         concrete plan."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["plan"],
            "properties": {
                "plan": { "type": "string", "description": "The plan to present (markdown)." }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let plan = match args["plan"].as_str() {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            _ => return ToolResult::error("Missing required argument: plan"),
        };
        let plan_exit = match &ctx.plan_exit {
            Some(p) => p,
            None => {
                return ToolResult::error(
                    "Plan approval is not available in this context (headless run).",
                )
            }
        };
        match plan_exit.present(plan).await {
            Ok(true) => {
                ToolResult::ok("Plan approved — leaving Plan mode. You may now make changes.")
            }
            Ok(false) => {
                ToolResult::ok("Plan rejected — staying in Plan mode. Revise and re-present.")
            }
            Err(e) => ToolResult::error(format!("Plan approval failed: {e}")),
        }
    }
}
