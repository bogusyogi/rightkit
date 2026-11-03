//! Typed Fly.io Machines API tool shared by desktop & iOS runtimes.
//!
//! ENG-72 (direct Fly.io BYOK execution): the user supplies their own Fly.io
//! API token. the host app provisions or resumes a *user-owned* Fly Machine
//! backed by a persistent Volume directly through the Fly Machines API.
//! There is no the host app account, proxy, or control plane in this path —
//! Fly spend and ownership stay entirely in the user's account. Mirrors the
//! request hardening of `crate::coding_tools::cloudflare::CloudflareTool`: HTTPS bearer
//! auth, 15s timeout, redirects disabled, bounded response size.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use rightkit_http::Request;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const FLY_MACHINES_API_BASE: &str = "https://api.machines.dev/v1";

/// User-supplied Fly.io credential. The token is the user's own Fly.io API
/// token (personal access token or org-scoped deploy token); the host app never
/// issues or proxies it.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlyioConfig {
    pub api_token: String,
    /// Fly.io org slug (e.g. "personal") the app/machine belongs to.
    pub org_slug: String,
}

pub struct FlyioTool;

#[async_trait::async_trait]
impl Tool for FlyioTool {
    fn name(&self) -> &str {
        "flyio"
    }

    fn description(&self) -> &str {
        "Fly.io Machines API: list apps/machines in the user's own Fly org, create or resume a user-owned Machine backed by a persistent Volume, stop/destroy a Machine, and list/create Volumes. Uses a host-stored, user-supplied Fly.io API token. No Agent account, proxy, or control plane is involved; all Fly spend and ownership stay in the user's Fly.io account."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type":"string", "enum":[
                    "list_apps", "create_app",
                    "list_machines", "get_machine",
                    "create_machine", "resume_machine",
                    "stop_machine", "destroy_machine",
                    "list_volumes", "create_volume"
                ]},
                "app_name": {"type":"string", "description":"Fly app slug"},
                "org_slug": {"type":"string", "description":"Fly.io org slug, required when no host credential config exists"},
                "machine_id": {"type":"string"},
                "region": {"type":"string", "description":"Fly region code, e.g. iad, lhr, syd"},
                "size": {"type":"string", "description":"Fly Machine guest size preset, e.g. shared-cpu-1x, performance-2x"},
                "image": {"type":"string", "description":"OCI image reference for the Machine, e.g. the Agent daemon image"},
                "volume_id": {"type":"string"},
                "volume_name": {"type":"string"},
                "volume_size_gb": {"type":"number", "description":"Persistent volume size in GiB"},
                "mount_path": {"type":"string", "description":"Path inside the Machine to mount the persistent volume"}
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
        // Prefer a mid-session refreshed credential over the session-start
        // clone, mirroring `crate::coding_tools::cloudflare::CloudflareTool::execute`.
        let config = if let Some(value) =
            crate::coding_tools::context::flyio_credential(&ctx.session_id)
                .or_else(|| ctx.flyio_config.clone())
        {
            value
        } else {
            let org_slug = match required(&args, "org_slug") {
                Ok(value) => value.to_string(),
                Err(error) => return error,
            };
            let Some(requester) = ctx.credential_requester.as_ref() else {
                return ToolResult::error("Fly.io is not connected on this execution target.");
            };
            let secret = match requester
                .request(
                    ctx.tool_call_id.clone(),
                    crate::coding_tools::context::CredentialPrompt {
                        provider: "flyio".to_string(),
                        reason: "Fly.io API token is required for this action.".to_string(),
                    },
                )
                .await
            {
                Ok(Some(value)) => value,
                Ok(None) => return ToolResult::error("Fly.io credential request was declined."),
                Err(error) => return ToolResult::error(error),
            };
            let value = FlyioConfig {
                api_token: secret.expose().to_string(),
                org_slug,
            };
            crate::coding_tools::context::update_flyio_credential(&ctx.session_id, value.clone());
            value
        };
        let (method, path, body) = match build_request(action, &args, &config.org_slug) {
            Ok(value) => value,
            Err(error) => return error,
        };

        let mut url = crate::coding_tools::context::local_provider_mock_base("flyio")
            .unwrap_or_else(|| {
                url::Url::parse(FLY_MACHINES_API_BASE).expect("Fly Machines API base URL is valid")
            });
        {
            let mut segments = url
                .path_segments_mut()
                .expect("Fly Machines API base URL supports segments");
            segments.pop_if_empty();
            for segment in path.split('/').filter(|s| !s.is_empty()) {
                segments.push(segment);
            }
        }
        let client = match crate::coding_tools::http_client::client_for(
            url.as_str(),
            Duration::from_secs(15),
        ) {
            Ok(value) => value,
            Err(error) => {
                return ToolResult::error(format!("Could not create Fly.io client: {error}"))
            }
        };
        let mut request = match method {
            HttpMethod::Get => Request::get(url.as_str()),
            HttpMethod::Post => Request::post(url.as_str()),
            HttpMethod::Delete => Request::delete(url.as_str()),
        };
        if let Some(body) = body {
            request = match request.json(&body) {
                Ok(value) => value,
                Err(error) => {
                    return ToolResult::error(format!("Fly.io request body failed: {error}"))
                }
            };
        }
        request = request
            .bearer(&config.api_token)
            .max_response_bytes(MAX_RESPONSE_BYTES)
            .header("Accept", "application/json");

        let response = match client.send_any(&request).await {
            Ok(value) => value,
            Err(rightkit_http::HttpError::BodyTooLarge { .. }) => {
                return ToolResult::error("Fly.io response exceeds 512 KiB limit.")
            }
            Err(error) => return ToolResult::error(format!("Fly.io request failed: {error}")),
        };
        let status = response.status;
        let bytes = response.body;
        let text = match String::from_utf8(bytes) {
            Ok(value) => value,
            Err(_) => return ToolResult::error("Fly.io returned invalid UTF-8."),
        };
        let text = if config.api_token.is_empty() {
            text
        } else {
            text.replace(&config.api_token, "<redacted>")
        };
        if !(200..300).contains(&status) {
            return ToolResult::error(format!(
                "Fly.io returned HTTP {}: {}",
                status,
                bounded(&text)
            ));
        }
        ToolResult::ok(bounded(&text))
    }
}

enum HttpMethod {
    Get,
    Post,
    Delete,
}

/// Builds the (method, path, optional JSON body) for a given action. Kept as
/// a free function so request construction can be unit-tested without a live
/// HTTP client.
fn build_request(
    action: &str,
    args: &Value,
    org_slug: &str,
) -> Result<(HttpMethod, String, Option<Value>), ToolResult> {
    match action {
        "list_apps" => Ok((HttpMethod::Get, format!("apps?org_slug={org_slug}"), None)),
        "create_app" => {
            let app_name = required(args, "app_name")?;
            Ok((
                HttpMethod::Post,
                "apps".to_string(),
                Some(json!({ "app_name": app_name, "org_slug": org_slug })),
            ))
        }
        "list_machines" => {
            let app_name = required(args, "app_name")?;
            Ok((HttpMethod::Get, format!("apps/{app_name}/machines"), None))
        }
        "get_machine" => {
            let app_name = required(args, "app_name")?;
            let machine_id = required(args, "machine_id")?;
            Ok((
                HttpMethod::Get,
                format!("apps/{app_name}/machines/{machine_id}"),
                None,
            ))
        }
        "create_machine" => {
            let app_name = required(args, "app_name")?;
            let image = required(args, "image")?;
            let region = args.get("region").and_then(Value::as_str);
            let size = args
                .get("size")
                .and_then(Value::as_str)
                .unwrap_or("shared-cpu-1x");
            let mut config = json!({
                "image": image,
                "guest": { "cpu_kind": "shared", "size": size },
            });
            if let (Some(volume_id), Some(mount_path)) = (
                args.get("volume_id").and_then(Value::as_str),
                args.get("mount_path").and_then(Value::as_str),
            ) {
                config["mounts"] = json!([{ "volume": volume_id, "path": mount_path }]);
            }
            let mut body = json!({ "config": config });
            if let Some(region) = region {
                body["region"] = json!(region);
            }
            Ok((
                HttpMethod::Post,
                format!("apps/{app_name}/machines"),
                Some(body),
            ))
        }
        "resume_machine" => {
            let app_name = required(args, "app_name")?;
            let machine_id = required(args, "machine_id")?;
            Ok((
                HttpMethod::Post,
                format!("apps/{app_name}/machines/{machine_id}/start"),
                None,
            ))
        }
        "stop_machine" => {
            let app_name = required(args, "app_name")?;
            let machine_id = required(args, "machine_id")?;
            Ok((
                HttpMethod::Post,
                format!("apps/{app_name}/machines/{machine_id}/stop"),
                None,
            ))
        }
        "destroy_machine" => {
            let app_name = required(args, "app_name")?;
            let machine_id = required(args, "machine_id")?;
            Ok((
                HttpMethod::Delete,
                format!("apps/{app_name}/machines/{machine_id}?force=true"),
                None,
            ))
        }
        "list_volumes" => {
            let app_name = required(args, "app_name")?;
            Ok((HttpMethod::Get, format!("apps/{app_name}/volumes"), None))
        }
        "create_volume" => {
            let app_name = required(args, "app_name")?;
            let volume_name = required(args, "volume_name")?;
            let region = required(args, "region")?;
            let size_gb = args
                .get("volume_size_gb")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            Ok((
                HttpMethod::Post,
                format!("apps/{app_name}/volumes"),
                Some(json!({
                    "name": volume_name,
                    "region": region,
                    "size_gb": size_gb,
                })),
            ))
        }
        _ => Err(ToolResult::error(format!(
            "Unsupported Fly.io action: {action}"
        ))),
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
