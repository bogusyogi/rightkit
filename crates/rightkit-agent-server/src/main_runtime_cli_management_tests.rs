// Behavioral tests for MCP test/login CLI formatting.
// Included from main_runtime_cli_management.rs — items and imports are
// provided by the parent `mod mcp_cli_format_tests { use super::*; .. }`.

#[test]
fn test_probe_known_good_server() {
    let value = serde_json::json!({
        "ok": true,
        "toolCount": 7,
        "latencyMs": 142,
    });
    let (line, code) = mcp_test_probe_format("my-server", &value);
    assert_eq!(code, 0);
    assert_eq!(line, "PASS | my-server | 7 tool(s) | 142ms");
}

#[test]
fn test_probe_unreachable_server() {
    let value = serde_json::json!({
        "ok": false,
        "error": "connection refused",
    });
    let (line, code) = mcp_test_probe_format("my-server", &value);
    assert_eq!(code, 1);
    assert_eq!(line, "FAIL | my-server | connection refused");
}

#[test]
fn test_probe_unknown_server_distinct_from_unreachable() {
    // Unknown = server not in config → "no such server configured" error
    // Unreachable = configured but down → "connection refused" / timeout
    let unknown = serde_json::json!({
        "ok": false,
        "error": "no such server configured: bogus-server",
    });
    let unreachable = serde_json::json!({
        "ok": false,
        "error": "connection refused",
    });

    let (line_u, code_u) = mcp_test_probe_format("bogus-server", &unknown);
    let (line_r, code_r) = mcp_test_probe_format("bogus-server", &unreachable);

    // Both exit non-zero
    assert_eq!(code_u, 1);
    assert_eq!(code_r, 1);
    // Error messages are distinguishable
    assert!(
        line_u.contains("no such server configured"),
        "unknown-server error must mention configuration: got {line_u}"
    );
    assert_eq!(line_r, "FAIL | bogus-server | connection refused");
    assert_ne!(line_u, line_r, "errors must be distinguishable");
}
