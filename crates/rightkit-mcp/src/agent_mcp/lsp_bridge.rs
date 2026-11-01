#![allow(clippy::empty_line_after_doc_comments)]
mod helpers;

pub use helpers::{lsp_mcp_tool_definitions, LspBridgeError};

#[cfg(test)]
use helpers::diagnostics_document_sync_notifications;
use helpers::{
    document_sync_notification, extract_definition_locations, extract_diagnostics,
    extract_hover_content, extract_reference_locations, read_file_text_for_uri,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;

#[cfg(not(test))]
const LSP_OPERATION_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(15);
#[cfg(test)]
const LSP_OPERATION_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_millis(250);
const LSP_SHUTDOWN_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(2);
const MAX_LSP_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LSP_HEADER_LINE_BYTES: usize = 8 * 1024;

/// LSP position (line/character, both 0-indexed per LSP spec).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// LSP location: file URI + range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspLocation {
    /// File URI, e.g. "file:///path/to/file.rs"
    pub uri: String,
    pub start: LspPosition,
    pub end: LspPosition,
}

/// Result of a goto-definition request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GotoDefinitionResult {
    /// One or more definition locations (LSP allows multiple).
    pub locations: Vec<LspLocation>,
}

/// Result of a hover request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoverResult {
    /// Markdown content from the language server.
    pub content: String,
}

/// A single diagnostic from the language server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub uri: String,
    pub start: LspPosition,
    pub end: LspPosition,
    /// 1=Error, 2=Warning, 3=Information, 4=Hint
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
}

/// Configuration for an LSP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerConfig {
    /// Language identifier (e.g. "rust", "typescript", "python").
    pub language_id: String,
    /// Path to the LSP server executable (e.g. "/usr/bin/rust-analyzer").
    pub server_path: String,
    /// Arguments to pass to the server.
    pub server_args: Vec<String>,
    /// Root URI of the workspace.
    pub workspace_root: String,
}

/// Bridge between an LSP server and the MCP tool interface.
pub struct LspMcpBridge {
    config: LspServerConfig,
    /// Whether the server has been initialized (LSP initialize handshake complete).
    initialized: bool,
    /// Spawned LSP server subprocess.
    child: Option<Child>,
    managed_child:
        Option<Arc<tokio::sync::Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>>,
    /// Stdin handle to the server.
    stdin: Option<std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>>,
    /// Buffered stdout reader from the server.
    stdout: Option<BufReader<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>>>,
    launch_context: Option<rightkit_sandbox::execution::ProcessLaunchContext>,
    launch_adapter: Option<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>>,
    /// Atomic counter for JSON-RPC request IDs.
    next_id: AtomicI64,
    /// Last full-document version sent for each open URI.
    document_versions: HashMap<String, i32>,
}

impl LspMcpBridge {
    pub fn new(config: LspServerConfig) -> Self {
        Self {
            config,
            initialized: false,
            child: None,
            managed_child: None,
            stdin: None,
            stdout: None,
            next_id: AtomicI64::new(1),
            document_versions: HashMap::new(),
            launch_context: None,
            launch_adapter: None,
        }
    }

    pub fn with_process_launcher(
        mut self,
        adapter: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
        context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Self {
        self.launch_adapter = Some(adapter);
        self.launch_context = Some(context);
        self
    }

    pub fn with_launch_context(
        mut self,
        context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Self {
        self.launch_context = Some(context);
        self
    }

    /// Initialize the LSP server connection (sends LSP `initialize` request).
    /// Must be called before any capability requests.
    pub async fn initialize(&mut self) -> Result<(), LspBridgeError> {
        if self.launch_adapter.is_none() || self.launch_context.is_none() {
            return Err(LspBridgeError::Communication(
                "LSP launch requires an injected host process adapter".into(),
            ));
        }
        if let (Some(adapter), Some(context)) =
            (self.launch_adapter.as_ref(), self.launch_context.clone())
        {
            let mut child = adapter
                .spawn(rightkit_sandbox::execution::CommandLaunchSpec {
                    program: self.config.server_path.clone(),
                    argv: self.config.server_args.clone(),
                    cwd: context.workspace_root.clone(),
                    context,
                    env: Vec::new(),
                })
                .await
                .map_err(|error| {
                    LspBridgeError::Communication(format!("spawn LSP server: {error:?}"))
                })?;
            let Some(stdin) = child.take_stdin() else {
                let _ = child.cancel().await;
                return Err(LspBridgeError::Communication("no stdin handle".into()));
            };
            let Some(stdout) = child.take_stdout() else {
                let _ = child.cancel().await;
                return Err(LspBridgeError::Communication("no stdout handle".into()));
            };
            self.stdin = Some(stdin);
            self.stdout = Some(BufReader::new(stdout));
            self.managed_child = Some(Arc::new(tokio::sync::Mutex::new(child)));
        } else {
            return Err(LspBridgeError::Communication(
                "LSP launch requires an injected host process adapter".into(),
            ));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let init_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": self.config.workspace_root,
                "capabilities": {}
            }
        });

        let handshake = async {
            self.write_message(&init_req).await?;

            // Servers may emit log/progress notifications during initialization;
            // wait for this request's response rather than treating the first
            // framed message as the handshake result.
            self.read_response(id).await?;

            let initialized_notif = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            });
            self.write_message(&initialized_notif).await
        };
        match tokio::time::timeout(LSP_OPERATION_TIMEOUT, handshake).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.terminate_connection().await;
                return Err(error);
            }
            Err(_) => {
                self.terminate_connection().await;
                return Err(LspBridgeError::Communication(
                    "LSP initialize timed out".into(),
                ));
            }
        }

        self.initialized = true;
        self.document_versions.clear();
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Get definition location(s) for a symbol at the given position.
    ///
    /// Maps to LSP `textDocument/definition` request.
    pub async fn goto_definition(
        &mut self,
        uri: &str,
        position: LspPosition,
    ) -> Result<GotoDefinitionResult, LspBridgeError> {
        self.require_initialized()?;

        let params = serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": position.line, "character": position.character}
        });

        let result = self.request("textDocument/definition", params).await?;
        let locations = extract_definition_locations(&result);

        Ok(GotoDefinitionResult { locations })
    }

    /// Search the workspace using the supervised server's symbol index.
    pub async fn workspace_symbols(
        &mut self,
        query: &str,
    ) -> Result<serde_json::Value, LspBridgeError> {
        self.require_initialized()?;
        self.request("workspace/symbol", serde_json::json!({"query": query}))
            .await
    }

    /// Get hover documentation for a symbol at the given position.
    ///
    /// Maps to LSP `textDocument/hover` request.
    pub async fn hover(
        &mut self,
        uri: &str,
        position: LspPosition,
    ) -> Result<HoverResult, LspBridgeError> {
        self.require_initialized()?;

        let params = serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": position.line, "character": position.character}
        });

        let result = self.request("textDocument/hover", params).await?;

        let content = if result.is_null() {
            String::new()
        } else if let Some(contents) = result.get("contents") {
            extract_hover_content(contents)
        } else {
            String::new()
        };

        Ok(HoverResult { content })
    }

    /// Get all references for a symbol at the given position.
    ///
    /// Maps to LSP `textDocument/references`; unlike the old tool-layer text
    /// search this preserves the language server's semantic result set.
    pub async fn references(
        &mut self,
        uri: &str,
        position: LspPosition,
    ) -> Result<Vec<LspLocation>, LspBridgeError> {
        self.require_initialized()?;
        let params = serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": position.line, "character": position.character},
            "context": {"includeDeclaration": true}
        });
        let result = self.request("textDocument/references", params).await?;
        Ok(extract_reference_locations(&result))
    }

    /// Synchronize a full document snapshot with the persistent server.
    pub async fn sync_document(&mut self, uri: &str, text: &str) -> Result<i32, LspBridgeError> {
        self.require_initialized()?;
        let previous_version = self.document_versions.get(uri).copied();
        let (notification, version) =
            document_sync_notification(uri, &self.config.language_id, text, previous_version);
        self.write_message_with_timeout(&notification, "LSP document synchronization timed out")
            .await?;
        self.document_versions.insert(uri.to_string(), version);
        Ok(version)
    }

    /// Get all diagnostics (errors/warnings) for the given file.
    ///
    /// Note: LSP diagnostics are pushed by the server via `textDocument/publishDiagnostics`
    /// notifications, not pulled. This implementation opens and saves the current
    /// document snapshot with `textDocument/didOpen` + `textDocument/didSave`,
    /// then collects diagnostics that arrive within a short window.
    pub async fn diagnostics(&mut self, uri: &str) -> Result<Vec<Diagnostic>, LspBridgeError> {
        self.require_initialized()?;

        if let Some(text) = read_file_text_for_uri(uri)? {
            self.sync_document(uri, &text).await?;
            self.write_message_with_timeout(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didSave",
                    "params": {
                        "textDocument": {"uri": uri},
                    }
                }),
                "LSP document save notification timed out",
            )
            .await?;
        }

        let diagnostics_map = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        let timeout_duration = tokio::time::Duration::from_millis(500);
        let start = tokio::time::Instant::now();

        loop {
            if start.elapsed() > timeout_duration {
                break;
            }

            match tokio::time::timeout(timeout_duration - start.elapsed(), self.read_message())
                .await
            {
                Ok(Ok(msg)) => {
                    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                        if method == "textDocument/publishDiagnostics" {
                            if let Some(params) = msg.get("params") {
                                if let Some(diags_uri) = params.get("uri").and_then(|u| u.as_str())
                                {
                                    if diags_uri == uri {
                                        let mut diags =
                                            extract_diagnostics(params.get("diagnostics"));
                                        for diagnostic in &mut diags {
                                            diagnostic.uri = diags_uri.to_string();
                                        }
                                        let mut map = diagnostics_map.lock().await;
                                        map.insert(uri.to_string(), diags);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => break,
            }
        }

        let map = diagnostics_map.lock().await;
        Ok(map.get(uri).cloned().unwrap_or_default())
    }

    /// Stop the live language-server process and release its governed resources.
    pub async fn shutdown(&mut self) {
        self.terminate_connection().await;
    }

    /// Send a JSON-RPC request and read the response.
    async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LspBridgeError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let exchange = async {
            self.write_message(&req).await?;
            self.read_response(id).await
        };
        match tokio::time::timeout(LSP_OPERATION_TIMEOUT, exchange).await {
            Ok(result) => result,
            Err(_) => {
                self.terminate_connection().await;
                Err(LspBridgeError::Communication(format!(
                    "LSP request '{method}' timed out"
                )))
            }
        }
    }

    async fn read_response(&mut self, id: i64) -> Result<serde_json::Value, LspBridgeError> {
        loop {
            let msg = self.read_message().await?;
            if msg.get("id").and_then(|value| value.as_i64()) != Some(id) {
                continue;
            }
            if let Some(error) = msg.get("error") {
                return Err(LspBridgeError::Protocol(error.to_string()));
            }
            return msg
                .get("result")
                .cloned()
                .ok_or_else(|| LspBridgeError::Protocol("missing result field".into()));
        }
    }

    /// Write a JSON-RPC message with Content-Length framing.
    async fn write_message(&mut self, msg: &serde_json::Value) -> Result<(), LspBridgeError> {
        let json_str = serde_json::to_string(msg)
            .map_err(|e| LspBridgeError::Communication(format!("serialize JSON: {e}")))?;
        let content_length = json_str.len();
        if content_length > MAX_LSP_MESSAGE_BYTES {
            return Err(LspBridgeError::Communication(format!(
                "outbound LSP message is {content_length} bytes; maximum is {MAX_LSP_MESSAGE_BYTES} bytes"
            )));
        }

        let header = format!("Content-Length: {}\r\n\r\n", content_length);

        if let Some(stdin) = &mut self.stdin {
            stdin
                .write_all(header.as_bytes())
                .await
                .map_err(|e| LspBridgeError::Communication(format!("write header: {e}")))?;
            stdin
                .write_all(json_str.as_bytes())
                .await
                .map_err(|e| LspBridgeError::Communication(format!("write body: {e}")))?;
            stdin
                .flush()
                .await
                .map_err(|e| LspBridgeError::Communication(format!("flush stdin: {e}")))?;
            Ok(())
        } else {
            Err(LspBridgeError::Communication("stdin not available".into()))
        }
    }

    async fn write_message_with_timeout(
        &mut self,
        msg: &serde_json::Value,
        timeout_message: &str,
    ) -> Result<(), LspBridgeError> {
        match tokio::time::timeout(LSP_OPERATION_TIMEOUT, self.write_message(msg)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.terminate_connection().await;
                Err(error)
            }
            Err(_) => {
                self.terminate_connection().await;
                Err(LspBridgeError::Communication(timeout_message.to_string()))
            }
        }
    }

    /// Read a JSON-RPC message with Content-Length framing.
    async fn read_message(&mut self) -> Result<serde_json::Value, LspBridgeError> {
        if let Some(stdout) = &mut self.stdout {
            let mut content_length = 0;

            loop {
                let line = read_lsp_header_line_bounded(stdout).await?;

                if line == "\r\n" || line == "\n" {
                    break;
                }

                if let Some(stripped) = line.strip_prefix("Content-Length:") {
                    content_length = stripped.trim().parse::<usize>().map_err(|e| {
                        LspBridgeError::Communication(format!("parse Content-Length: {e}"))
                    })?;
                    if content_length > MAX_LSP_MESSAGE_BYTES {
                        return Err(LspBridgeError::Communication(format!(
                            "LSP message is {content_length} bytes; maximum is {MAX_LSP_MESSAGE_BYTES} bytes"
                        )));
                    }
                }
            }

            if content_length == 0 {
                return Err(LspBridgeError::Communication(
                    "no Content-Length header".into(),
                ));
            }

            let mut body = vec![0u8; content_length];
            stdout
                .read_exact(&mut body)
                .await
                .map_err(|e| LspBridgeError::Communication(format!("read body: {e}")))?;

            let body_str = String::from_utf8(body)
                .map_err(|e| LspBridgeError::Communication(format!("UTF-8 decode: {e}")))?;
            serde_json::from_str(&body_str)
                .map_err(|e| LspBridgeError::Communication(format!("parse JSON: {e}")))
        } else {
            Err(LspBridgeError::Communication("stdout not available".into()))
        }
    }

    fn require_initialized(&self) -> Result<(), LspBridgeError> {
        if !self.initialized {
            Err(LspBridgeError::NotInitialized)
        } else {
            Ok(())
        }
    }

    async fn terminate_connection(&mut self) {
        self.initialized = false;
        self.stdin = None;
        self.stdout = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if let Some(child) = self.managed_child.take() {
            let mut child = child.lock().await;
            let _ = child.cancel().await;
            let _ = tokio::time::timeout(LSP_SHUTDOWN_TIMEOUT, child.wait()).await;
        }
    }
}

async fn read_lsp_header_line_bounded<R>(reader: &mut R) -> Result<String, LspBridgeError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| LspBridgeError::Communication(format!("read header line: {error}")))?;
        if available.is_empty() {
            if line.is_empty() {
                return Err(LspBridgeError::Communication(
                    "EOF while reading headers".into(),
                ));
            }
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        if line.len().saturating_add(take) > MAX_LSP_HEADER_LINE_BYTES {
            return Err(LspBridgeError::Communication(format!(
                "LSP header line exceeds the {MAX_LSP_HEADER_LINE_BYTES}-byte maximum"
            )));
        }
        let ended = available.get(take.saturating_sub(1)) == Some(&b'\n');
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if ended {
            break;
        }
    }
    String::from_utf8(line)
        .map_err(|error| LspBridgeError::Communication(format!("header UTF-8: {error}")))
}

/// Errors from the LSP bridge.

#[cfg(test)]
#[path = "lsp_bridge_tests.rs"]
mod lsp_bridge_tests;
