//! Example MCP server built on the host: `example_server stdio` or `example_server http`.
//! Env: EXAMPLE_ALLOW_MUTATE=1 lets mutating tools through; EXAMPLE_TOKEN sets the HTTP bearer token.

use std::io::{Read, Write};
use std::sync::Arc;

use rightkit_mcp::*;
use serde_json::{json, Value};

struct Demo;

impl ToolProvider for Demo {
    fn list(&self) -> Vec<ToolDescriptor> {
        let obj = |p: Value| json!({"type": "object", "properties": p});
        vec![
            ToolDescriptor::new(
                "echo",
                "Echo a message",
                obj(json!({"message": {"type": "string"}})),
            )
            .read_only(true),
            ToolDescriptor::new(
                "add",
                "Add two numbers",
                obj(json!({"a": {"type": "number"}, "b": {"type": "number"}})),
            )
            .read_only(true),
            ToolDescriptor::new(
                "write_marker",
                "Write a file (mutating)",
                obj(json!({"path": {"type": "string"}, "content": {"type": "string"}})),
            ),
            ToolDescriptor::new("secret_tool", "Hidden by policy", obj(json!({}))).read_only(true),
            ToolDescriptor::new("die", "Exit the server process", obj(json!({}))).read_only(true),
        ]
    }

    fn call(&self, name: &str, args: Value) -> Result<ToolResult> {
        match name {
            "echo" => Ok(ToolResult::text(format!(
                "echo: {}",
                args["message"].as_str().unwrap_or("")
            ))),
            "add" => Ok(ToolResult::json(
                json!({"sum": args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0)}),
            )),
            "write_marker" => {
                let path = args["path"].as_str().unwrap_or("");
                std::fs::write(path, args["content"].as_str().unwrap_or(""))
                    .map_err(McpError::from)?;
                Ok(ToolResult::text(format!("wrote {path}")))
            }
            "secret_tool" => Ok(ToolResult::text("secret")),
            "die" => std::process::exit(3),
            other => Err(McpError::ToolNotFound(other.into())),
        }
    }

    fn resources(&self) -> Vec<ResourceDescriptor> {
        vec![ResourceDescriptor {
            uri: "demo://readme".into(),
            name: "readme".into(),
            description: "".into(),
            mime_type: "text/plain".into(),
        }]
    }
    fn read_resource(&self, uri: &str) -> Result<ResourceContents> {
        if uri == "demo://readme" {
            Ok(ResourceContents {
                uri: uri.into(),
                mime_type: "text/plain".into(),
                text: Some("hello resource".into()),
                blob: None,
            })
        } else {
            Err(McpError::ToolNotFound(uri.into()))
        }
    }
    fn prompts(&self) -> Vec<PromptDescriptor> {
        vec![PromptDescriptor {
            name: "greet".into(),
            description: "".into(),
            arguments: vec![PromptArgument {
                name: "who".into(),
                description: "".into(),
                required: true,
            }],
        }]
    }
    fn get_prompt(&self, name: &str, args: Value) -> Result<Vec<PromptMessage>> {
        if name == "greet" {
            Ok(vec![PromptMessage {
                role: "user".into(),
                text: format!("Hello {}", args["who"].as_str().unwrap_or("world")),
            }])
        } else {
            Err(McpError::ToolNotFound(name.into()))
        }
    }
}

/// Managed-host marker: the RightKit build broker socket, set for managed runtimes.
const MANAGED_HOST_MARKER: &str = "RIGHTKIT_BUILD_BROKER_SOCKET";
/// Bearer-token variable this example would read from the environment.
const TOKEN_ENV: &str = "EXAMPLE_TOKEN";

fn is_managed_host(env: impl Fn(&str) -> Option<String>) -> bool {
    env(MANAGED_HOST_MARKER).is_some_and(|v| !v.is_empty())
}

/// Resolves the HTTP bearer token. On managed hosts the environment is never a
/// secret source: the error names the variable only, never its value, and the
/// server does not start unauthenticated. This example has no protected
/// credential source wired, so managed `http` mode always refuses.
fn http_token(
    managed: bool,
    env_token: Option<String>,
) -> std::result::Result<Option<String>, String> {
    if managed {
        return Err(format!(
            "MANAGED_HOST_ENV_SECRET_REFUSED: {TOKEN_ENV} is not read on managed hosts; \
             provision the bearer token in the OS credential store"
        ));
    }
    Ok(env_token)
}

fn main() {
    let allow = std::env::var("EXAMPLE_ALLOW_MUTATE").is_ok_and(|v| v == "1");
    let host = ServerHost::new("example", "0.1.0", Arc::new(Demo)).with_admission(Arc::new(
        ReadOnlyPolicy {
            allow_mutate: allow,
            blocked: vec!["secret_tool".into()],
        },
    ));
    match std::env::args().nth(1).as_deref() {
        Some("http") => {
            let managed = is_managed_host(|k| std::env::var(k).ok());
            let token = match http_token(managed, std::env::var(TOKEN_ENV).ok()) {
                Ok(token) => token,
                Err(message) => {
                    eprintln!("{message}");
                    std::process::exit(2);
                }
            };
            let srv = host
                .serve_http(HttpServeOptions { port: 0, token })
                .expect("bind");
            println!("LISTENING {}", srv.url());
            std::io::stdout().flush().ok();
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink); // run until the parent closes stdin
        }
        _ => host.serve_stdio().expect("stdio"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_host_refuses_env_value_and_names_variable_only() {
        let err = http_token(true, Some("fixture-not-real".into())).unwrap_err();
        assert!(err.contains("MANAGED_HOST_ENV_SECRET_REFUSED"));
        assert!(err.contains(TOKEN_ENV));
        assert!(err.contains("OS credential store"));
        assert!(!err.contains("fixture-not-real"));
    }

    #[test]
    fn managed_host_refuses_http_without_env_value() {
        let err = http_token(true, None).unwrap_err();
        assert!(err.contains("MANAGED_HOST_ENV_SECRET_REFUSED"));
    }

    #[test]
    fn unmanaged_host_keeps_env_fallback() {
        assert_eq!(
            http_token(false, Some("fixture-dev".into())).unwrap(),
            Some("fixture-dev".into())
        );
        assert_eq!(http_token(false, None).unwrap(), None);
    }

    #[test]
    fn managed_marker_detection() {
        let marked = |k: &str| (k == MANAGED_HOST_MARKER).then(|| "/tmp/broker.sock".to_string());
        assert!(is_managed_host(marked));
        assert!(!is_managed_host(|_| None));
        assert!(!is_managed_host(
            |k| (k == MANAGED_HOST_MARKER).then(String::new)
        ));
    }
}
