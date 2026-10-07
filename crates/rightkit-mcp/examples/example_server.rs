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
            let srv = host
                .serve_http(HttpServeOptions {
                    port: 0,
                    token: std::env::var("EXAMPLE_TOKEN").ok(),
                })
                .expect("bind");
            println!("LISTENING {}", srv.url());
            std::io::stdout().flush().ok();
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink); // run until the parent closes stdin
        }
        _ => host.serve_stdio().expect("stdio"),
    }
}
