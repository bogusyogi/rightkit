use super::{Diagnostic, LspLocation, LspPosition};
use std::io::Read;

pub(super) const MAX_LSP_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum LspBridgeError {
    #[error("LSP bridge not initialized — call initialize() first")]
    NotInitialized,
    #[error("LSP server communication error: {0}")]
    Communication(String),
    #[error("LSP protocol error: {0}")]
    Protocol(String),
}

/// Extract definition locations from LSP response (handles null, single Location, or Location[]).
pub(super) fn extract_definition_locations(result: &serde_json::Value) -> Vec<LspLocation> {
    if result.is_null() {
        return Vec::new();
    }

    if let Some(arr) = result.as_array() {
        arr.iter().filter_map(parse_lsp_location).collect()
    } else if result.is_object() {
        parse_lsp_location(result).into_iter().collect()
    } else {
        Vec::new()
    }
}

/// Extract reference locations from an LSP response.
///
/// `textDocument/references` returns the same `Location[]` shape as
/// `textDocument/definition`, so keep one parser for both capabilities.
pub(super) fn extract_reference_locations(result: &serde_json::Value) -> Vec<LspLocation> {
    extract_definition_locations(result)
}

/// Parse a single LSP Location object.
fn parse_lsp_location(loc: &serde_json::Value) -> Option<LspLocation> {
    let uri = loc.get("uri")?.as_str()?.to_string();
    let range = loc.get("range")?;
    let start_line = range.get("start")?.get("line")?.as_u64()? as u32;
    let start_char = range.get("start")?.get("character")?.as_u64()? as u32;
    let end_line = range.get("end")?.get("line")?.as_u64()? as u32;
    let end_char = range.get("end")?.get("character")?.as_u64()? as u32;

    Some(LspLocation {
        uri,
        start: LspPosition {
            line: start_line,
            character: start_char,
        },
        end: LspPosition {
            line: end_line,
            character: end_char,
        },
    })
}

/// Extract hover content from various LSP response formats.
pub(super) fn extract_hover_content(contents: &serde_json::Value) -> String {
    if let Some(s) = contents.as_str() {
        return s.to_string();
    }

    if let Some(obj) = contents.as_object() {
        if let Some(value) = obj.get("value").and_then(|v| v.as_str()) {
            if let Some(kind) = obj.get("kind").and_then(|k| k.as_str()) {
                return format!("({})\n{}", kind, value);
            }
            return value.to_string();
        }
    }

    if let Some(arr) = contents.as_array() {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|item| {
                if let Some(s) = item.as_str() {
                    Some(s.to_string())
                } else if let Some(obj) = item.as_object() {
                    obj.get("value").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect();
        return parts.join("\n");
    }

    String::new()
}

#[cfg(test)]
pub(super) fn diagnostics_document_sync_notifications(
    uri: &str,
    language_id: &str,
    text: &str,
) -> Vec<serde_json::Value> {
    let (open, _) = document_sync_notification(uri, language_id, text, None);
    vec![
        open,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": {
                "textDocument": {"uri": uri},
                "text": text,
            }
        }),
    ]
}

/// Build the next full-document LSP synchronization notification.
///
/// The first snapshot opens the document at version 1. Later snapshots use
/// `didChange` and monotonically increment the version, which lets a persistent
/// language server observe writes made by the host app without being restarted.
pub(super) fn document_sync_notification(
    uri: &str,
    language_id: &str,
    text: &str,
    previous_version: Option<i32>,
) -> (serde_json::Value, i32) {
    let version = previous_version.unwrap_or(0).saturating_add(1);
    let notification = if previous_version.is_none() {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text,
                }
            }
        })
    } else {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "version": version,
                },
                "contentChanges": [{"text": text}],
            }
        })
    };
    (notification, version)
}

pub(super) fn read_file_text_for_uri(uri: &str) -> Result<Option<String>, LspBridgeError> {
    let Some(path) = file_uri_to_path(uri) else {
        return Ok(None);
    };
    let file = std::fs::File::open(&path).map_err(|error| {
        LspBridgeError::Communication(format!(
            "open diagnostics document '{}': {error}",
            path.display()
        ))
    })?;
    let declared = file.metadata().map_err(|error| {
        LspBridgeError::Communication(format!(
            "inspect diagnostics document '{}': {error}",
            path.display()
        ))
    })?;
    if declared.len() > MAX_LSP_DOCUMENT_BYTES as u64 {
        return Err(LspBridgeError::Communication(format!(
            "diagnostics document is {} bytes; maximum is {MAX_LSP_DOCUMENT_BYTES} bytes",
            declared.len()
        )));
    }
    let mut bytes = Vec::with_capacity(declared.len() as usize);
    file.take(MAX_LSP_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            LspBridgeError::Communication(format!(
                "read diagnostics document '{}': {error}",
                path.display()
            ))
        })?;
    if bytes.len() > MAX_LSP_DOCUMENT_BYTES {
        return Err(LspBridgeError::Communication(format!(
            "diagnostics document exceeds the {MAX_LSP_DOCUMENT_BYTES}-byte maximum"
        )));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        LspBridgeError::Communication(format!("diagnostics document UTF-8: {error}"))
    })
}

fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    let decoded = percent_decode(raw)?;
    #[cfg(windows)]
    let path = {
        let trimmed = decoded
            .strip_prefix('/')
            .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
            .unwrap_or(&decoded);
        std::path::PathBuf::from(trimmed.replace('/', "\\"))
    };
    #[cfg(not(windows))]
    let path = std::path::PathBuf::from(decoded);
    Some(path)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).copied()?;
            let lo = bytes.get(i + 2).copied()?;
            out.push((hex_value(hi)? << 4) | hex_value(lo)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Extract diagnostics from LSP response format.
pub(super) fn extract_diagnostics(diags_value: Option<&serde_json::Value>) -> Vec<Diagnostic> {
    diags_value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    let range = d.get("range")?;
                    let start_line = range.get("start")?.get("line")?.as_u64()? as u32;
                    let start_char = range.get("start")?.get("character")?.as_u64()? as u32;
                    let end_line = range.get("end")?.get("line")?.as_u64()? as u32;
                    let end_char = range.get("end")?.get("character")?.as_u64()? as u32;
                    let message = d.get("message")?.as_str()?.to_string();
                    let severity = d.get("severity").and_then(|s| s.as_u64()).unwrap_or(1) as u8;
                    let source = d.get("source").and_then(|s| s.as_str()).map(String::from);

                    Some(Diagnostic {
                        uri: String::new(),
                        start: LspPosition {
                            line: start_line,
                            character: start_char,
                        },
                        end: LspPosition {
                            line: end_line,
                            character: end_char,
                        },
                        severity,
                        message,
                        source,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Returns MCP tool definitions that expose LSP capabilities.
/// Register these with the existing MCP tool registry to make LSP
/// available via `mcp_call`.
pub fn lsp_mcp_tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "lsp_goto_definition",
            "description": "Get the definition location(s) for a symbol at a file position. Uses the language server for the file's language. Returns file URI + line/character.",
            "inputSchema": {
                "type": "object",
                "required": ["uri", "line", "character"],
                "properties": {
                    "uri": {"type": "string", "description": "File URI, e.g. file:///path/to/file.rs"},
                    "line": {"type": "integer", "description": "0-indexed line number"},
                    "character": {"type": "integer", "description": "0-indexed character offset"}
                }
            }
        }),
        serde_json::json!({
            "name": "lsp_hover",
            "description": "Get hover documentation for a symbol at a file position. Returns markdown from the language server.",
            "inputSchema": {
                "type": "object",
                "required": ["uri", "line", "character"],
                "properties": {
                    "uri": {"type": "string"},
                    "line": {"type": "integer"},
                    "character": {"type": "integer"}
                }
            }
        }),
        serde_json::json!({
            "name": "lsp_references",
            "description": "Get semantic reference locations for a symbol from the language server.",
            "inputSchema": {
                "type": "object",
                "required": ["uri", "line", "character"],
                "properties": {
                    "uri": {"type": "string"},
                    "line": {"type": "integer"},
                    "character": {"type": "integer"}
                }
            }
        }),
        serde_json::json!({
            "name": "lsp_diagnostics",
            "description": "Get all diagnostics (errors/warnings) for a file from the language server.",
            "inputSchema": {
                "type": "object",
                "required": ["uri"],
                "properties": {
                    "uri": {"type": "string", "description": "File URI to get diagnostics for"}
                }
            }
        }),
    ]
}
