//! Pre-serialised JSON reaches the wire unchanged (rightkit#174).

use std::io::Cursor;
use std::sync::Arc;

use rightkit_mcp::*;
use serde_json::{json, Value};

// Key order not alphabetical, 1.0 vs 1, exponent, trailing zero, escapes, a >u64 integer.
const BODY: &str = r#"{"z":1.0,"a":1,"m":[1E2,0.10,18446744073709551617],"u":"é😀\/","k":{"b":null,"a":true}}"#;

const CALL: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"raw","arguments":{}}}"#;

struct Raw;

impl ToolProvider for Raw {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor::new("raw", "Raw", json!({"type":"object"}))]
    }
    fn call(&self, _name: &str, _args: Value) -> Result<ToolResult> {
        Ok(ToolResult::raw_json(RawJson::new(BODY).unwrap()))
    }
}

fn serve(host: &ServerHost, request: &str) -> String {
    let mut out = Vec::new();
    host.serve_stdio_on(Cursor::new(format!("{request}\n")), &mut out)
        .unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn stdio_reply_carries_raw_bytes_unchanged() {
    let host = ServerHost::new("t", "0", Arc::new(Raw));
    let line = serve(&host, CALL);
    assert!(
        line.contains(&format!(r#""structuredContent":{BODY}"#)),
        "structuredContent bytes changed: {line}"
    );
    // The text block is the same bytes as a JSON string.
    let reply: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(reply["result"]["content"][0]["text"], BODY);
}

#[test]
fn size_limit_counts_raw_bytes() {
    let host = ServerHost::new("t", "0", Arc::new(Raw)).with_response_limits(ResponseLimits {
        max_response_bytes: 64,
        ..ResponseLimits::default()
    });
    let req: Value = serde_json::from_str(CALL).unwrap();
    assert!(matches!(
        host.dispatch_checked(&req, &RequestContext::default()),
        Err(McpError::ResponseTooLarge { .. })
    ));
}

#[test]
fn value_roundtrip_would_have_changed_bytes() {
    // Documents why RawJson exists: a parsed Value does not reproduce BODY.
    let reparsed: Value = serde_json::from_str(BODY).unwrap();
    assert_ne!(reparsed.to_string(), BODY);
}
