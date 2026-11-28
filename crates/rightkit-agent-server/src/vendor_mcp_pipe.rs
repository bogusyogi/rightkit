//! Run-scoped local transport. The MCP child is a protocol proxy; only its
//! parent owns session execution, policy, storage and context authority.
//!
//! Windows: a named pipe that rejects remote clients. macOS and Linux: a Unix domain socket in
//! a folder only this user can enter. On both, the child must send the run's key first.
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

const MAX_FRAME: u64 = 1024 * 1024;

pub(crate) struct VendorMcpPipe {
    name: String,
    key: String,
    task: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    folder: std::path::PathBuf,
}

/// Serve one connected child: the key line first, then one JSON-RPC line per request.
async fn serve_connection<S>(
    stream: S,
    expected: &str,
    server: &mut crate::mcp_server::McpStdioServer,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    let mut auth = String::new();
    let authenticated = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        (&mut stream).take(128).read_line(&mut auth),
    )
    .await;
    if !(matches!(authenticated, Ok(Ok(_))) && auth.trim_end() == expected) {
        tracing::warn!(
            operation = "vendor_mcp_rejected",
            "vendor tool transport authentication failed"
        );
        return;
    }
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
}

impl VendorMcpPipe {
    #[cfg(windows)]
    pub(crate) fn start(mut server: crate::mcp_server::McpStdioServer) -> Result<Self, String> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = format!(
            r"\\.\pipe\{app}-vendor-{}",
            rightkit_agent_engine_protocol::uuid_v7(),
            app = crate::host_identity::app()
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
                serve_connection(&mut listener, &expected, &mut server).await;
                drop(listener);
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

    #[cfg(unix)]
    pub(crate) fn start(mut server: crate::mcp_server::McpStdioServer) -> Result<Self, String> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        // The socket lives in its own folder that only this user can enter, so no other user can
        // reach it; the key is still required from whoever connects. The path is kept short:
        // a Unix socket path has a small fixed limit (104 bytes on macOS).
        let folder = std::env::temp_dir().join(format!(
            "vendor-{}",
            &rightkit_agent_engine_protocol::uuid_v7().to_string()[..13]
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&folder)
            .map_err(|error| format!("create vendor approval socket folder: {error}"))?;
        let path = folder.join("s");
        let listener = tokio::net::UnixListener::bind(&path).map_err(|error| {
            let _ = std::fs::remove_dir_all(&folder);
            format!("create vendor approval socket: {error}")
        })?;
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_dir_all(&folder);
            return Err(format!("protect vendor approval socket: {error}"));
        }
        let key = rightkit_agent_engine_protocol::uuid_v7().to_string();
        let expected = key.clone();
        let task = tokio::spawn(async move {
            tracing::info!(
                operation = "vendor_mcp_ready",
                "vendor tool transport ready"
            );
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    tracing::warn!(
                        operation = "vendor_mcp_failed",
                        "vendor tool transport connection failed"
                    );
                    break;
                };
                serve_connection(stream, &expected, &mut server).await;
            }
        });
        Ok(Self {
            name: path.to_string_lossy().into_owned(),
            key,
            task,
            folder,
        })
    }

    pub(crate) fn attach(&self, spec: &mut rightkit_agents::driver_host::LaunchSpec) {
        spec.env.insert(
            rightkit_agent_config::env_name("VENDOR_PIPE"),
            self.name.clone(),
        );
        spec.env.insert(
            rightkit_agent_config::env_name("VENDOR_PIPE_KEY"),
            self.key.clone(),
        );
    }
}

impl Drop for VendorMcpPipe {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        {
            let _ = std::fs::remove_dir_all(&self.folder);
        }
    }
}

pub(crate) async fn proxy_stdio() -> i32 {
    let Ok(name) = std::env::var(rightkit_agent_config::env_name("VENDOR_PIPE")) else {
        return 1;
    };
    let Ok(key) = std::env::var(rightkit_agent_config::env_name("VENDOR_PIPE_KEY")) else {
        return 1;
    };
    #[cfg(windows)]
    let Ok(pipe) = tokio::net::windows::named_pipe::ClientOptions::new().open(name) else {
        return 1;
    };
    #[cfg(unix)]
    let Ok(pipe) = tokio::net::UnixStream::connect(name).await
    else {
        return 1;
    };
    proxy_over(pipe, &key).await
}

async fn proxy_over<S>(mut pipe: S, key: &str) -> i32
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use std::io::{BufRead, Write};
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
