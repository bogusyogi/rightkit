//! `finalize_artifact` — mark an artifact as finalized/shippable.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct FinalizeArtifactTool;

#[async_trait::async_trait]
impl Tool for FinalizeArtifactTool {
    fn name(&self) -> &str {
        "finalize_artifact"
    }

    fn description(&self) -> &str {
        "Mark an artifact as finalized (shippable). Only valid in trust-gated mode."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["artifact_id"],
            "properties": {
                "artifact_id": { "type": "string", "description": "ID of the artifact to finalize" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // No state root means no trust store: the tool refuses rather than read a fallback file.
        let Some(trust_store) = rightkit_app_settings::agent_config::workspace_trust_store_path()
        else {
            return ToolResult::error(
                "The state directory is unavailable, so workspace trust cannot be checked.",
            );
        };
        self.execute_with_trust_path(args, ctx, &trust_store)
    }
}

impl FinalizeArtifactTool {
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

        let store = match crate::coding_tools::artifact_crud::trust_store_for(
            ctx,
            "finalize_artifact",
            trust_store_path,
        ) {
            Ok(s) => s,
            Err(message) => return ToolResult::error(message),
        };

        let mut store = store.lock().unwrap();
        match store.finalize_artifact(&artifact_id) {
            Some(artifact) => ToolResult::ok(format!(
                "Artifact '{}' finalized. shippable={}",
                artifact.id, artifact.shippable
            )),
            None => ToolResult::error(format!("Artifact not found: {}", artifact_id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx_no_store() -> ToolContext {
        let dir = tempdir().unwrap();
        ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    #[tokio::test]
    async fn finalize_artifact_no_store_returns_error() {
        let tool = FinalizeArtifactTool;
        let result = tool
            .execute(json!({"artifact_id": "art1"}), &ctx_no_store())
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("trust store"));
    }

    #[tokio::test]
    async fn finalize_artifact_missing_id_returns_error() {
        let tool = FinalizeArtifactTool;
        let result = tool.execute(json!({}), &ctx_no_store()).await;
        assert!(result.is_error);
        assert!(result.content.contains("artifact_id"));
    }
}
