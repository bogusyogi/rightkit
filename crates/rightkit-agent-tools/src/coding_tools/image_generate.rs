//! Concrete image-generation tool (ENG-87).
//!
//! Image models use a dedicated route catalogue and HTTP transport
//! (`rightkit_llm::generation`). This tool never routes through chat
//! completions, so a text model cannot be mistaken for an image model. It
//! obtains only an already-configured provider credential and returns
//! provider output verbatim as either an expiring URL or base64 bytes.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use rightkit_llm::agent_providers::load_api_key;
use rightkit_llm::generation::{
    ImageGenerationClient, ImageRequest, ImageResult, ImageRouteCatalogue,
};
use rightkit_llm::StaticCredentials;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// The one image provider with a concrete transport.
const IMAGE_PROVIDER: &str = "openai";
/// Exact image endpoint for [`IMAGE_PROVIDER`].
const OPENAI_IMAGE_BASE_URL: &str = "https://api.openai.com/v1";
const MAX_IMAGE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Resolve the image provider slug only for the exact bundled image endpoint.
/// This keeps a text-provider key from being forwarded to a custom URL merely
/// because that URL is OpenAI-compatible.
pub fn image_provider_for_base_url(base_url: &str) -> Option<&'static str> {
    base_url
        .trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(OPENAI_IMAGE_BASE_URL)
        .then_some(IMAGE_PROVIDER)
}

pub struct ImageGenerateTool {
    catalogue: ImageRouteCatalogue,
}

impl ImageGenerateTool {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            catalogue: ImageRouteCatalogue::from_json(include_str!(
                "../../assets/image-routes.json"
            ))
            .map_err(|error| error.to_string())?,
        })
    }
}

impl Default for ImageGenerateTool {
    fn default() -> Self {
        Self::new().expect("bundled image route catalogue must parse")
    }
}

#[async_trait::async_trait]
impl Tool for ImageGenerateTool {
    fn name(&self) -> &str {
        "image_generate"
    }

    fn description(&self) -> &str {
        "Generate images through the agent's dedicated image-model catalog. Requires a configured image-provider account; image models never use chat completion routes."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["model", "prompt"],
            "properties": {
                "model": {"type": "string", "description": "Image catalog route, e.g. gpt-image-1"},
                "prompt": {"type": "string"},
                "size": {"type": "string", "default": "1024x1024"},
                "count": {"type": "integer", "minimum": 1, "maximum": 4, "default": 1}
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
        let model = match required(&args, "model") {
            Ok(value) => value,
            Err(error) => return error,
        };
        let prompt = match required(&args, "prompt") {
            Ok(value) => value,
            Err(error) => return error,
        };
        let route = match self.catalogue.route(model) {
            Some(route) => route.clone(),
            None => return ToolResult::error(format!("Unknown image model: {model}")),
        };
        let size = args
            .get("size")
            .and_then(Value::as_str)
            .unwrap_or("1024x1024")
            .to_string();
        if !route.sizes.contains(&size) {
            return ToolResult::error(format!(
                "image model '{}' does not support size {size}",
                route.id
            ));
        }
        let count = match args.get("count").and_then(Value::as_u64).unwrap_or(1) {
            value @ 1..=4 => value as usize,
            _ => return ToolResult::error("count must be between 1 and 4"),
        };
        let api_key = match provider_api_key(ctx, IMAGE_PROVIDER) {
            Some(key) => key,
            None => {
                return ToolResult::error(format!(
                    "{IMAGE_PROVIDER} image account is not configured on this execution target."
                ))
            }
        };
        let request = ImageRequest::new(route.model.clone(), prompt).with_size(size);
        // rightkit-llm's image client is blocking and returns one image per call.
        let generated = tokio::task::spawn_blocking(move || {
            let http = rightkit_llm::Client::new(rightkit_llm::ClientConfig {
                request_timeout: Duration::from_secs(60),
                response_timeout: Some(Duration::from_secs(60)),
                retry: rightkit_llm::RetryPolicy::none(),
                proxy: rightkit_llm::ProxyMode::Environment,
                ..rightkit_llm::ClientConfig::default()
            });
            let client = ImageGenerationClient::new(
                IMAGE_PROVIDER,
                OPENAI_IMAGE_BASE_URL,
                http,
                Arc::new(StaticCredentials::default().with(IMAGE_PROVIDER, &api_key)),
            )
            .with_max_response_bytes(MAX_IMAGE_RESPONSE_BYTES);
            let mut images = Vec::new();
            for _ in 0..count {
                match client.generate(&request) {
                    Ok(image) => images.push(image),
                    // Keep what was already generated (and billed) rather than drop it.
                    Err(error) if images.is_empty() => return Err(error.to_string()),
                    Err(_) => break,
                }
            }
            Ok(images)
        })
        .await;
        match generated {
            Ok(Ok(images)) => ToolResult::ok(
                json!({
                    "model": route.id,
                    "provider": IMAGE_PROVIDER,
                    "images": images.into_iter().map(|image| match image.result {
                        ImageResult::Url(url) => json!({"kind": "url", "value": url}),
                        ImageResult::Base64(data) => json!({"kind": "base64", "value": data}),
                    }).collect::<Vec<_>>()
                })
                .to_string(),
            ),
            Ok(Err(error)) => ToolResult::error(error),
            Err(error) => ToolResult::error(format!("image generation task failed: {error}")),
        }
    }
}

fn required<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolResult> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ToolResult::error(format!("Missing required argument: {name}")))
}

/// Resolve only a direct, session-scoped key for this exact image provider
/// before considering desktop's existing configured-account lookup.
fn provider_api_key(ctx: &ToolContext, provider: &str) -> Option<String> {
    ctx.image_provider_keys
        .get(provider)
        .cloned()
        .or_else(|| load_api_key(provider))
}
