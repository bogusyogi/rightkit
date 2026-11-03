//! Typed Cloudflare API tool shared by desktop & iOS runtimes.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use rightkit_http::Request;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareConfig {
    pub api_token: String,
    pub account_id: String,
}

pub struct CloudflareTool;

#[async_trait::async_trait]
impl Tool for CloudflareTool {
    fn name(&self) -> &str {
        "cloudflare"
    }

    fn description(&self) -> &str {
        "Cloudflare account API: inspect Pages, R2 & Workers; upload one workspace file to R2; retry/rollback Pages deployments; trigger an existing Workers Build. Uses host-stored OAuth/API-token credentials. Git-linked Pages/Workers deploy automatically after git push."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type":"string", "enum":[
                    "list_pages_projects", "list_pages_deployments",
                    "retry_pages_deployment", "rollback_pages_deployment",
                    "list_r2_buckets", "list_r2_objects", "upload_r2_object",
                    "list_workers", "list_worker_deployments", "trigger_worker_build"
                ]},
                "project": {"type":"string"},
                "account_id": {"type":"string", "description":"Cloudflare account id, required when no host credential config exists"},
                "deployment_id": {"type":"string"},
                "bucket": {"type":"string"},
                "object_key": {"type":"string"},
                "workspace_path": {"type":"string", "description":"Workspace-relative file to upload"},
                "content_type": {"type":"string"},
                "worker": {"type":"string"},
                "trigger_id": {"type":"string"}
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Network)
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(value) => value,
            None => return ToolResult::error("Missing required argument: action"),
        };
        let verdict = PermissionEngine::new().evaluate(
            CapabilityClass::Network,
            ctx.permission_mode,
            ctx.workspace_trust,
            None,
            false,
        );
        match verdict.decision {
            PermissionDecision::Allow => {}
            PermissionDecision::Ask => {
                return ToolResult::error(format!("Tool requires approval: {}", verdict.reason))
            }
            PermissionDecision::Deny => {
                return ToolResult::error(format!("Permission denied: {}", verdict.reason))
            }
        }
        // A mid-session `update_cloudflare_credential` call lands in the
        // session-keyed cell (see `crate::coding_tools::context::update_cloudflare_credential`)
        // without going through the per-turn engine lock. Prefer it when
        // present so a refreshed token reaches the very next call; otherwise
        // fall back to the session-start value the engine cloned into this
        // context.
        let config = if let Some(value) =
            crate::coding_tools::context::cloudflare_credential(&ctx.session_id)
                .or_else(|| ctx.cloudflare_config.clone())
        {
            value
        } else {
            let account_id = match required(&args, "account_id") {
                Ok(value) => value.to_string(),
                Err(error) => return error,
            };
            let Some(requester) = ctx.credential_requester.as_ref() else {
                return ToolResult::error("Cloudflare is not connected on this execution target.");
            };
            let secret = match requester
                .request(
                    ctx.tool_call_id.clone(),
                    crate::coding_tools::context::CredentialPrompt {
                        provider: "cloudflare".to_string(),
                        reason: "Cloudflare API token is required for this action.".to_string(),
                    },
                )
                .await
            {
                Ok(Some(value)) => value,
                Ok(None) => {
                    return ToolResult::error("Cloudflare credential request was declined.")
                }
                Err(error) => return ToolResult::error(error),
            };
            let value = CloudflareConfig {
                api_token: secret.expose().to_string(),
                account_id,
            };
            crate::coding_tools::context::update_cloudflare_credential(
                &ctx.session_id,
                value.clone(),
            );
            value
        };
        let account = &config.account_id;
        let path = match action {
            "list_pages_projects" => format!("accounts/{account}/pages/projects"),
            "list_pages_deployments" => match required(&args, "project") {
                Ok(project) => format!("accounts/{account}/pages/projects/{project}/deployments"),
                Err(error) => return error,
            },
            "retry_pages_deployment" | "rollback_pages_deployment" => {
                let project = match required(&args, "project") {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                let deployment = match required(&args, "deployment_id") {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                let operation = if action.starts_with("retry") {
                    "retry"
                } else {
                    "rollback"
                };
                format!("accounts/{account}/pages/projects/{project}/deployments/{deployment}/{operation}")
            }
            "list_r2_buckets" => format!("accounts/{account}/r2/buckets"),
            "list_r2_objects" | "upload_r2_object" => {
                let bucket = match required(&args, "bucket") {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                if action == "list_r2_objects" {
                    format!("accounts/{account}/r2/buckets/{bucket}/objects")
                } else {
                    let key = match required(&args, "object_key") {
                        Ok(v) => v,
                        Err(e) => return e,
                    };
                    format!("accounts/{account}/r2/buckets/{bucket}/objects/{key}")
                }
            }
            "list_workers" => format!("accounts/{account}/workers/scripts"),
            "list_worker_deployments" => match required(&args, "worker") {
                Ok(worker) => format!("accounts/{account}/workers/scripts/{worker}/deployments"),
                Err(error) => return error,
            },
            "trigger_worker_build" => match required(&args, "trigger_id") {
                Ok(trigger) => format!("accounts/{account}/builds/triggers/{trigger}/builds"),
                Err(error) => return error,
            },
            _ => return ToolResult::error(format!("Unsupported Cloudflare action: {action}")),
        };

        let mut url = crate::coding_tools::context::local_provider_mock_base("cloudflare")
            .unwrap_or_else(|| {
                url::Url::parse("https://api.cloudflare.com/client/v4/")
                    .expect("Cloudflare API base URL is valid")
            });
        {
            let mut segments = url
                .path_segments_mut()
                .expect("Cloudflare base URL supports segments");
            segments.pop_if_empty();
            for segment in path.split('/') {
                segments.push(segment);
            }
        }
        let client = match crate::coding_tools::http_client::client_for(
            url.as_str(),
            Duration::from_secs(15),
        ) {
            Ok(value) => value,
            Err(error) => {
                return ToolResult::error(format!("Could not create Cloudflare client: {error}"))
            }
        };
        let mut request = match action {
            "retry_pages_deployment" | "rollback_pages_deployment" | "trigger_worker_build" => {
                Request::post(url.as_str())
            }
            "upload_r2_object" => {
                let local = match required(&args, "workspace_path") {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                let file = match crate::coding_tools::read_file::resolve_in_workspace(
                    local,
                    &ctx.workspace_root,
                ) {
                    Ok(value) => value,
                    Err(error) => return ToolResult::error(error),
                };
                let bytes = match tokio::fs::read(&file).await {
                    Ok(value) => value,
                    Err(error) => {
                        return ToolResult::error(format!("Could not read {local}: {error}"))
                    }
                };
                if bytes.len() > 25 * 1024 * 1024 {
                    return ToolResult::error("R2 upload exceeds 25 MiB on-device limit.");
                }
                let content_type = args
                    .get("content_type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                Request::put(url.as_str())
                    .header("Content-Type", content_type)
                    .body(bytes)
            }
            _ => Request::get(url.as_str()),
        };
        request = request
            .bearer(&config.api_token)
            .max_response_bytes(MAX_RESPONSE_BYTES)
            .header("Accept", "application/json");
        let response = match client.send_any(&request).await {
            Ok(value) => value,
            Err(rightkit_http::HttpError::BodyTooLarge { .. }) => {
                return ToolResult::error("Cloudflare response exceeds 512 KiB limit.")
            }
            Err(error) => return ToolResult::error(format!("Cloudflare request failed: {error}")),
        };
        let status = response.status;
        let bytes = response.body;
        let body = match String::from_utf8(bytes) {
            Ok(value) => value,
            Err(_) => return ToolResult::error("Cloudflare returned invalid UTF-8."),
        };
        let body = if config.api_token.is_empty() {
            body
        } else {
            body.replace(&config.api_token, "<redacted>")
        };
        if !(200..300).contains(&status) {
            return ToolResult::error(format!(
                "Cloudflare returned HTTP {}: {}",
                status,
                bounded(&body)
            ));
        }
        ToolResult::ok(bounded(&body))
    }
}

fn required<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolResult> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ToolResult::error(format!("Missing required argument: {name}")))
}

fn bounded(value: &str) -> String {
    const LIMIT: usize = 100_000;
    if value.len() <= LIMIT {
        return value.to_string();
    }
    let mut end = LIMIT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &value[..end])
}
