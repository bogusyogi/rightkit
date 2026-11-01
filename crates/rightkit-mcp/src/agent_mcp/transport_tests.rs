mod tests {
    use super::super::*;
    use serde_json::json;

    #[test]
    fn parse_tool_list_preserves_annotations() {
        let payload = json!({
            "tools": [
                {
                    "name": "search",
                    "description": "search docs",
                    "inputSchema": {"type": "object"},
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                }
            ]
        });
        let descriptors: Vec<crate::ToolDescriptor> =
            serde_json::from_value(payload["tools"].clone()).unwrap();
        let tools = descriptors
            .into_iter()
            .map(tool_definition)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].annotations.read_only_hint, Some(true));
        assert_eq!(tools[0].annotations.destructive_hint, Some(false));
        assert_eq!(tools[0].annotations.idempotent_hint, Some(true));
        assert_eq!(tools[0].annotations.open_world_hint, Some(false));
    }

    #[test]
    fn protocol_negotiation_rejects_unknown_versions() {
        let error = negotiate_version(&json!({"protocolVersion": "2099-01-01"})).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported MCP protocolVersion"));
    }

    #[test]
    fn initialize_advertises_latest_supported_protocol_version() {
        assert_eq!(initialize_params()["protocolVersion"], "2025-03-26");
    }

    #[test]
    fn protocol_negotiation_accepts_only_explicitly_supported_versions() {
        for version in ["2025-03-26", "2024-11-05", "2024-10-07"] {
            assert_eq!(
                negotiate_version(&json!({"protocolVersion": version})).unwrap(),
                version
            );
        }
        let error = negotiate_version(&json!({"protocolVersion": "2099-01-01"})).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported MCP protocolVersion"));
    }

    #[test]
    fn parse_tool_result_prefers_canonical_structured_content_over_summary() {
        let structured = json!({
            "operation": "blueprint_recall",
            "result": {
                "kind": "success",
                "data": {"nodes": [{"id": "node-1", "content": "canonical evidence"}]}
            }
        });
        let parsed = parse_tool_result(json!({
            "content": [{"type": "text", "text": "membrane_result operation=blueprint_recall kind=success code=none"}],
            "structuredContent": structured,
            "isError": false
        }))
        .unwrap();

        assert_eq!(parsed.content, structured.to_string());
        assert!(!parsed.is_error);
    }

    #[test]
    fn parse_tool_result_retains_legacy_text_and_error_flag() {
        let parsed = parse_tool_result(json!({
            "content": [{"type": "text", "text": "legacy result"}],
            "isError": true
        }))
        .unwrap();

        assert_eq!(parsed.content, "legacy result");
        assert!(parsed.is_error);
    }

    #[test]
    fn parse_tool_result_rejects_non_object_structured_content() {
        let error = parse_tool_result(json!({"structuredContent": ["invalid"]})).unwrap_err();
        assert!(error
            .to_string()
            .contains("expected 'structuredContent' object"));
    }
}
