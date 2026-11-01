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
    async fn attach_verdict_no_store_returns_error() {
        let tool = AttachVerdictTool;
        let result = tool
            .execute(
                json!({
                    "artifact_id": "art1",
                    "verdict": "approved",
                    "reasoning": "looks good",
                    "confidence": 0.9
                }),
                &ctx_no_store(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("trust store"));
    }

    /// Writes a workspace-trust store recording `workspace` with `label`.
    fn trust_store_file(
        dir: &std::path::Path,
        workspace: &std::path::Path,
        label: &str,
    ) -> std::path::PathBuf {
        let key =
            rightkit_app_settings::agent_config::workspace_trust_key(&workspace.to_string_lossy());
        let path = dir.join("workspace-trust.json");
        let body = json!({"projects": {key: {"trust": label}}});
        std::fs::write(&path, body.to_string()).unwrap();
        path
    }

    async fn create_artifact(ctx: &ToolContext) -> String {
        let created = crate::coding_tools::artifact_crud::ArtifactCreateTool
            .execute(
                json!({"path": "notes/a.md", "kind": "markdown", "content": "hello"}),
                ctx,
            )
            .await;
        assert!(!created.is_error, "{}", created.content);
        let value: Value = serde_json::from_str(&created.content).unwrap();
        value["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn trusted_workspace_attaches_verdict_and_finalizes() {
        let ws = tempdir().unwrap();
        let state = tempdir().unwrap();
        let store = trust_store_file(state.path(), ws.path(), "trusted");
        let ctx = ToolContext::new(
            "s1",
            ws.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );
        let id = create_artifact(&ctx).await;

        let attached = AttachVerdictTool.execute_with_trust_path(
            json!({"artifact_id": id, "verdict": "approved", "reasoning": "ok", "confidence": 0.9}),
            &ctx,
            &store,
        );
        assert!(!attached.is_error, "{}", attached.content);
        let value: Value = serde_json::from_str(&attached.content).unwrap();
        assert_eq!(value["verdict"]["passed"], true);
        assert_eq!(value["shippable"], false);

        let finalized = crate::coding_tools::finalize_artifact::FinalizeArtifactTool
            .execute_with_trust_path(json!({"artifact_id": id}), &ctx, &store);
        assert!(!finalized.is_error, "{}", finalized.content);
        assert!(
            finalized.content.contains("shippable=true"),
            "{}",
            finalized.content
        );
    }

    #[tokio::test]
    async fn untrusted_workspace_refuses_both_tools() {
        let ws = tempdir().unwrap();
        let state = tempdir().unwrap();
        let ctx = ToolContext::new(
            "s1",
            ws.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );
        let id = create_artifact(&ctx).await;

        // Explicitly untrusted, and absent from the store entirely: both refuse.
        let untrusted = trust_store_file(state.path(), ws.path(), "untrusted");
        let missing = state.path().join("absent.json");
        for store in [&untrusted, &missing] {
            let attached = AttachVerdictTool.execute_with_trust_path(
                json!({"artifact_id": id, "verdict": "approved", "reasoning": "ok", "confidence": 0.9}),
                &ctx,
                store,
            );
            assert!(attached.is_error);
            assert!(attached
                .content
                .starts_with("attach_verdict requires a trust store."));
            let finalized = crate::coding_tools::finalize_artifact::FinalizeArtifactTool
                .execute_with_trust_path(json!({"artifact_id": id}), &ctx, store);
            assert!(finalized.is_error);
            assert!(finalized
                .content
                .starts_with("finalize_artifact requires a trust store."));
        }
    }

    #[tokio::test]
    async fn attach_verdict_missing_id_returns_error() {
        let tool = AttachVerdictTool;
        let result = tool
            .execute(
                json!({"verdict": "approved", "reasoning": "ok", "confidence": 1.0}),
                &ctx_no_store(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("artifact_id"));
    }
}
