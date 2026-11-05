//! Run-scoped local transport. The MCP child is a protocol proxy; only its
//! parent owns session execution, policy, storage and context authority.
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

const MAX_FRAME: u64 = 1024 * 1024;

/// The host-side MCP request handler behind the pipe. The host owns policy, session and
/// context authority; this crate only moves framed JSON-RPC requests to it.
pub trait VendorMcpHandler: Send + 'static {
    /// Answer one MCP request. `None` acknowledges a notification with no response.
    fn handle_request(
        &mut self,
        request: serde_json::Value,
    ) -> impl std::future::Future<Output = Option<serde_json::Value>> + Send;
}

pub struct VendorMcpPipe {
    name: String,
    key: String,
    task: tokio::task::JoinHandle<()>,
}

impl VendorMcpPipe {
    pub fn start<H: VendorMcpHandler>(mut server: H) -> Result<Self, String> {
        let name = format!(
            r"\\.\pipe\coderight-vendor-{}",
            rightkit_agent_engine_protocol::uuid_v7()
        );
        let key = rightkit_agent_engine_protocol::uuid_v7().to_string();
        let listener = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)
            .map_err(|error| format!("create vendor approval pipe: {error}"))?;
        let expected = key.clone();
        let next_name = name.clone();
        let task = tokio::spawn(async move {
            tracing::info!(
                operation = "vendor_mcp_ready",
                "vendor tool transport ready"
            );
            let mut listener = listener;
            loop {
                if listener.connect().await.is_err() {
                    tracing::warn!(
                        operation = "vendor_mcp_failed",
                        "vendor tool transport connection failed"
                    );
                    break;
                }
                let mut stream = BufReader::new(listener);
                let mut auth = String::new();
                let authenticated = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    (&mut stream).take(128).read_line(&mut auth),
                )
                .await;
                if matches!(authenticated, Ok(Ok(_))) && auth.trim_end() == expected {
                    loop {
                        let mut line = String::new();
                        let read = (&mut stream).take(MAX_FRAME + 1).read_line(&mut line).await;
                        if !matches!(read, Ok(n) if n > 0) || line.len() as u64 > MAX_FRAME {
                            break;
                        }
                        let Ok(request) = serde_json::from_str(&line) else {
                            break;
                        };
                        // A null envelope acknowledges notifications without producing
                        // an MCP response on the child's stdout.
                        let response = server.handle_request(request).await;
                        let Ok(encoded) = serde_json::to_vec(&response) else {
                            break;
                        };
                        if stream.get_mut().write_all(&encoded).await.is_err()
                            || stream.get_mut().write_all(b"\n").await.is_err()
                        {
                            break;
                        }
                    }
                } else {
                    tracing::warn!(
                        operation = "vendor_mcp_rejected",
                        "vendor tool transport authentication failed"
                    );
                }
                drop(stream);
                listener = match ServerOptions::new()
                    .first_pipe_instance(true)
                    .reject_remote_clients(true)
                    .create(&next_name)
                {
                    Ok(listener) => listener,
                    Err(_) => break,
                };
            }
        });
        Ok(Self { name, key, task })
    }

    pub fn attach(&self, spec: &mut crate::driver_host::LaunchSpec) {
        spec.env
            .insert("RIGHTKIT_VENDOR_PIPE".into(), self.name.clone());
        spec.env
            .insert("RIGHTKIT_VENDOR_PIPE_KEY".into(), self.key.clone());
    }
}

impl Drop for VendorMcpPipe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn proxy_stdio() -> i32 {
    use std::io::{BufRead, Write};
    let Ok(name) = std::env::var("RIGHTKIT_VENDOR_PIPE") else {
        return 1;
    };
    let Ok(key) = std::env::var("RIGHTKIT_VENDOR_PIPE_KEY") else {
        return 1;
    };
    let Ok(mut pipe) = ClientOptions::new().open(name) else {
        return 1;
    };
    if pipe.write_all(format!("{key}\n").as_bytes()).await.is_err() {
        return 1;
    }
    let mut pipe = BufReader::new(pipe);
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stdout();
    loop {
        let mut line = String::new();
        let read = std::io::Read::take(&mut input, MAX_FRAME + 1).read_line(&mut line);
        if matches!(read, Ok(0)) {
            return 0;
        }
        if read.is_err() || line.len() as u64 > MAX_FRAME {
            return 1;
        }
        if line.trim().is_empty() {
            continue;
        }
        if pipe.get_mut().write_all(line.as_bytes()).await.is_err() {
            return 1;
        }
        let mut reply = String::new();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(330),
            (&mut pipe).take(MAX_FRAME + 1).read_line(&mut reply),
        )
        .await;
        if !matches!(response, Ok(Ok(n)) if n > 0) || reply.len() as u64 > MAX_FRAME {
            return 1;
        }
        let Ok(response) = serde_json::from_str::<Option<serde_json::Value>>(&reply) else {
            return 1;
        };
        if let Some(response) = response {
            if writeln!(output, "{response}").is_err() || output.flush().is_err() {
                return 1;
            }
        }
    }
}
