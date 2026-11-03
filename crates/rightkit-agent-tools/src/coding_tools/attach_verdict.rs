//! `attach_verdict` — attach a trust verdict to an artifact (A3).

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_engine_protocol::ArtifactVerdict;
use serde_json::{json, Value};

pub struct AttachVerdictTool;

#[async_trait::async_trait]
impl Tool for AttachVerdictTool {
    fn name(&self) -> &str {
        "attach_verdict"
    }

    fn description(&self) -> &str {
        "Attach trust verdict (approved/rejected) to artifact with reasoning + confidence [0-1]. Fail: no trust store or artifact not found."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["artifact_id", "verdict", "reasoning", "confidence"],
            "properties": {
                "artifact_id": { "type": "string" },
                "verdict": { "type": "string", "enum": ["approved", "rejected"] },
                "reasoning": { "type": "string" },
                "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        self.execute_with_trust_path(
            args,
            ctx,
            &rightkit_app_settings::agent_config::workspace_trust_store_path(),
        )
    }
}

impl AttachVerdictTool {
    /// `execute` with an explicit workspace-trust store location (#159).
    pub(crate) fn execute_with_trust_path(
        &self,
        args: Value,
        ctx: &ToolContext,
        trust_store_path: &std::path::Path,
    ) -> ToolResult {
        let artifact_id = match args["artifact_id"].as_str() {
            Some(id) => id.to_string(),
            None => return ToolResult::error("Missing required argument: artifact_id"),
        };
        let verdict_str = match args["verdict"].as_str() {
            Some(v) => v,
            None => return ToolResult::error("Missing required argument: verdict"),
        };
        let reasoning = args["reasoning"].as_str().unwrap_or("").to_string();

        let passed = verdict_str == "approved";
        let verdict = ArtifactVerdict {
            passed,
            juror: ctx.session_id.clone(),
            bound_hash: String::new(),
            summary: reasoning,
        };

        let store = match crate::coding_tools::artifact_crud::trust_store_for(
            ctx,
            "attach_verdict",
            trust_store_path,
        ) {
            Ok(s) => s,
            Err(message) => return ToolResult::error(message),
        };

        let mut store = store.lock().unwrap();
        match store.attach_verdict(&artifact_id, verdict) {
            Some(artifact) => ToolResult::ok(
                serde_json::to_string_pretty(&serde_json::json!({
                    "id": artifact.id,
                    "path": artifact.path,
                    "verdict": artifact.verdict,
                    "shippable": artifact.shippable,
                }))
                .unwrap_or_default(),
            ),
            None => ToolResult::error(format!("Artifact not found: {}", artifact_id)),
        }
    }
}
