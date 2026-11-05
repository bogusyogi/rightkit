//! Context7 — built-in library documentation lookup tool.
//!
//! Resolves up-to-date library docs via the context7.com public API.
//! No external MCP server needed; calls the API directly over HTTP.
//!
//! Two operations:
//! - `resolve-library-id`: search for a library by name, get its context7 ID
//! - `get-library-docs`: fetch focused documentation for a library ID

use serde_json::Value;

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};

const API_BASE: &str = "https://context7.com/api/v1";

/// API root. `RIGHTKIT_CONTEXT7_API_BASE` redirects the lookups (self-hosted
/// mirror, or a local fake in offline qualification); unset means context7.com.
///
/// The override is a `host` capability; the mobile profile always uses context7.com.
#[cfg(not(feature = "coding_tools-host"))]
fn api_base() -> String {
    API_BASE.to_string()
}

#[cfg(feature = "coding_tools-host")]
fn api_base() -> String {
    std::env::var("RIGHTKIT_CONTEXT7_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
        .unwrap_or_else(|| API_BASE.to_string())
}

fn url_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => vec![c.to_string()],
            ' ' => vec!["+".to_string()],
            c => vec![format!("%{:02X}", c as u32)],
        })
        .collect()
}

pub struct Context7Tool;

#[async_trait::async_trait]
impl Tool for Context7Tool {
    fn name(&self) -> &str {
        "context7"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::Network)
    }

    fn description(&self) -> &str {
        "Fetch up-to-date library documentation from context7.com. \
        Use 'resolve-library-id' first to find the library's context7 ID, \
        then 'get-library-docs' to retrieve focused docs. \
        Always use this before answering questions about library APIs, \
        configuration options, or version-specific behaviour."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["resolve-library-id", "get-library-docs"],
                    "description": "Which operation to perform."
                },
                "libraryName": {
                    "type": "string",
                    "description": "Library name to search for (use with resolve-library-id)."
                },
                "context7CompatibleLibraryID": {
                    "type": "string",
                    "description": "Library ID from resolve-library-id (use with get-library-docs)."
                },
                "topic": {
                    "type": "string",
                    "description": "Focus the docs on a specific topic, e.g. 'routing', 'authentication'."
                },
                "tokens": {
                    "type": "integer",
                    "description": "Max tokens of documentation to return (default 5000, max 20000).",
                    "minimum": 1000,
                    "maximum": 20000
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let operation = match args.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return ToolResult::error("missing required field 'operation'"),
        };

        match operation {
            "resolve-library-id" => {
                let name = match args.get("libraryName").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => {
                        return ToolResult::error(
                            "'libraryName' is required for resolve-library-id",
                        )
                    }
                };
                resolve_library_id(name).await
            }
            "get-library-docs" => {
                let lib_id = match args
                    .get("context7CompatibleLibraryID")
                    .and_then(|v| v.as_str())
                {
                    Some(id) => id,
                    None => {
                        return ToolResult::error(
                            "'context7CompatibleLibraryID' is required for get-library-docs",
                        )
                    }
                };
                let topic = args.get("topic").and_then(|v| v.as_str());
                let tokens = args.get("tokens").and_then(|v| v.as_u64()).unwrap_or(5000) as u32;
                get_library_docs(lib_id, topic, tokens).await
            }
            other => ToolResult::error(format!(
                "unknown operation '{other}'; use 'resolve-library-id' or 'get-library-docs'"
            )),
        }
    }
}

async fn resolve_library_id(name: &str) -> ToolResult {
    let url = format!("{}/search?q={}", api_base(), url_encode(name));
    match fetch_json(&url).await {
        Ok(val) => {
            // Return the top results so the agent can pick the right one.
            let results = val.get("results").cloned().unwrap_or(val);
            ToolResult::ok(
                serde_json::to_string_pretty(&results).unwrap_or_else(|_| results.to_string()),
            )
        }
        Err(e) => ToolResult::error(format!("context7 search failed: {e}")),
    }
}

async fn get_library_docs(lib_id: &str, topic: Option<&str>, tokens: u32) -> ToolResult {
    let mut url = format!("{}/libraries/{}", api_base(), url_encode(lib_id));
    let mut params = Vec::new();
    if let Some(t) = topic {
        params.push(format!("topic={}", url_encode(t)));
    }
    params.push(format!("tokens={tokens}"));
    url.push('?');
    url.push_str(&params.join("&"));

    match fetch_json(&url).await {
        Ok(val) => {
            // Surface `content` directly if present (the docs text), else return full JSON.
            let content = val
                .get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string())
                });
            ToolResult::ok(content)
        }
        Err(e) => ToolResult::error(format!("context7 docs fetch failed: {e}")),
    }
}

async fn fetch_json(url: &str) -> Result<Value, String> {
    let client = crate::coding_tools::http_client::client_following(
        url,
        crate::coding_tools::http_client::DEFAULT_TIMEOUT,
    )?;
    let resp =
        crate::coding_tools::http_client::send_any(&client, &rightkit_http::Request::get(url))
            .await?;
    if !crate::coding_tools::http_client::is_success(&resp) {
        return Err(format!("HTTP {}", resp.status));
    }
    resp.json::<Value>().map_err(|e| e.to_string())
}
