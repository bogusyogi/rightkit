//! Persistent Sentinel/Beacon MCP transport for the host.
//!
//! One client process serves a session.  Calls are serialized over its stdio
//! transport; no tool event creates a child process.

use rightkit_mcp::{CallOptions, CancellationToken, McpClient};
// Only the host spawn path below builds a launch spec and connects.
#[cfg(feature = "host")]
use rightkit_mcp::{ClientExtensions, ClientOptions, ServerSource, ServerSpec, TrustStore};
#[cfg(feature = "host")]
use rightkit_sandbox::execution::ProcessLaunchContext;
use serde_json::Value;
#[cfg(feature = "host")]
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct SentinelClient {
    client: Arc<McpClient>,
}

impl SentinelClient {
    pub async fn spawn(node: &str, server: &str) -> std::io::Result<Self> {
        #[cfg(not(feature = "host"))]
        {
            Err(std::io::Error::other(format!(
                "Sentinel requires the host feature; process launch is unavailable in the portable profile (server {server} on {node} was not started)"
            )))
        }
        #[cfg(feature = "host")]
        {
            let cwd = std::env::current_dir()?;
            let context = ProcessLaunchContext {
                workspace_root: cwd.display().to_string(),
                sandbox_profile: Some("sentinel".into()),
                sandbox_required: true,
                network_required: true,
            };
            let runtime = tokio::runtime::Handle::current();
            let program = node.to_owned();
            let server = server.to_owned();
            let client = tokio::task::spawn_blocking(move || {
                let launcher = rightkit_mcp::agent_mcp::rightkit_process_launcher(
                    Arc::new(rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter),
                    context,
                    runtime,
                );
                let spec = ServerSpec::Stdio {
                    command: program,
                    args: vec![server, "--host-stdio".into()],
                    env: BTreeMap::new(),
                    cwd: Some(cwd),
                };
                let options = ClientOptions {
                    client_name: crate::assurance_broker::host_issuer(),
                    client_version: "h2-v1".into(),
                    ..ClientOptions::default()
                };
                McpClient::connect_with_extensions(
                    "sentinel",
                    // Host-configured server (<PREFIX>_SENTINEL_SERVER), not workspace-declared.
                    &ServerSource::User,
                    &TrustStore::new(),
                    spec,
                    options,
                    ClientExtensions {
                        protocol_versions: Some(vec!["2025-11-25".into()]),
                        launcher: Some(launcher),
                        ..ClientExtensions::default()
                    },
                )
                .map(Arc::new)
                .map_err(rightkit_error)
            })
            .await
            .map_err(std::io::Error::other)??;
            Ok(Self { client })
        }
    }

    pub async fn stop(&mut self) -> std::io::Result<()> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || client.close())
            .await
            .map_err(std::io::Error::other)?;
        Ok(())
    }

    pub fn host_authorized(&self) -> bool {
        true
    }

    /// Host-only: the Sentinel server and node executable come from the
    /// environment. The portable (mobile) profile reads neither variable.
    #[cfg(feature = "host")]
    pub async fn spawn_from_env() -> std::io::Result<Option<Self>> {
        let Some(server) = std::env::var_os(rightkit_agent_config::env_name("SENTINEL_SERVER"))
        else {
            return Ok(None);
        };
        let node = std::env::var(rightkit_agent_config::env_name("SENTINEL_NODE"))
            .unwrap_or_else(|_| "node".into());
        Ok(Some(Self::spawn(&node, &server.to_string_lossy()).await?))
    }

    #[cfg(not(feature = "host"))]
    pub async fn spawn_from_env() -> std::io::Result<Option<Self>> {
        Ok(None)
    }

    pub async fn beacon(&mut self, arguments: Value) -> std::io::Result<Value> {
        let client = self.client.clone();
        let cancellation = CancellationToken::new();
        let guard = CancellationGuard(cancellation.clone());
        let options = CallOptions {
            cancellation: Some(cancellation),
            ..CallOptions::default()
        };
        let result = tokio::task::spawn_blocking(move || {
            client
                .call_tool_with_options("beacon", arguments, &options)
                .map(|result| result.to_value())
                .map_err(rightkit_error)
        })
        .await
        .map_err(std::io::Error::other)??;
        drop(guard);
        extract_structured_result(result)
    }
}

struct CancellationGuard(CancellationToken);

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Drop for SentinelClient {
    fn drop(&mut self) {
        let client = self.client.clone();
        if let Err(error) = std::thread::Builder::new()
            .name(format!(
                "{}-sentinel-close",
                crate::assurance_broker::host_agent_name()
            ))
            .spawn(move || client.close())
        {
            tracing::warn!(%error, "could not start the thread that closes the Sentinel client");
        }
    }
}

fn rightkit_error(error: rightkit_mcp::McpError) -> std::io::Error {
    let mapped = rightkit_mcp::agent_mcp::map_rightkit_error(error);
    match mapped {
        rightkit_mcp::agent_mcp::McpError::ChildExited(_) => {
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, mapped.to_string())
        }
        _ => std::io::Error::other(mapped.to_string()),
    }
}

fn extract_structured_result(result: Value) -> std::io::Result<Value> {
    if let Some(value) = result.get("structuredContent") {
        return Ok(value.clone());
    }
    let Some(text) = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.iter().find_map(|item| item.get("text")))
        .and_then(Value::as_str)
    else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Sentinel response lacks structured content",
        ));
    };
    serde_json::from_str(text).map_err(std::io::Error::other)
}
