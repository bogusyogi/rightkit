//! `attach_verdict` — attach a trust verdict to an artifact (A3).

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_agent_engine_protocol::ArtifactVerdict;
use serde_json::{json, Value};

pub struct AttachVerdictTool;

/// The workspace trust store, only when the host names it: the state root's store, or the explicit override
/// (`RIGHTKIT_WORKSPACE_TRUST_STORE`, honoured by app-settings with its host-env feature). With neither, `None`: a
/// trust-checking tool refuses rather than read the working-directory fallback that `workspace_trust_store_path`
/// returns, which a checked-out repository could supply itself.
pub(crate) fn host_trust_store() -> Option<std::path::PathBuf> {
    use rightkit_agent_config::{state_root, workspace_trust_store_path};
    let path = workspace_trust_store_path();
    // The override variable exists only with the host feature (rightkit-agent-config `host-env`);
    // without it the path above is never the override, so there is nothing to recognise.
    #[cfg(feature = "host")]
    let overridden = {
        use rightkit_agent_config::workspace_trust::WORKSPACE_TRUST_STORE_ENV;
        std::env::var_os(WORKSPACE_TRUST_STORE_ENV)
            .is_some_and(|v| std::path::Path::new(&v) == path.as_path())
    };
    #[cfg(not(feature = "host"))]
    let overridden = false;
    let under_state_root = state_root().is_some_and(|root| path.starts_with(root));
    (overridden || under_state_root).then_some(path)
}

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
        let Some(trust_store) = host_trust_store() else {
            return ToolResult::error(
                "The state directory is unavailable, so workspace trust cannot be checked.",
            );
        };
        self.execute_with_trust_path(args, ctx, &trust_store)
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

        let store =
            match crate::artifact_crud::trust_store_for(ctx, "attach_verdict", trust_store_path) {
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
