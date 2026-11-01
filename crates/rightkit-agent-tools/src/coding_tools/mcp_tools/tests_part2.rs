#[tokio::test]
async fn call_error_result_propagated() {
    let stub: Arc<dyn McpTransport> = Arc::new(
        StubTransport::new(vec![tool_def("get_pr", "get a PR")]).with_response(
            "get_pr",
            McpToolResult {
                content: "not found".to_string(),
                is_error: true,
            },
        ),
    );
    let mut mgr = McpManager::new();
    mgr.register("github", stub);
    let ctx = make_ctx_with_mcp(mgr);

    let result = McpCallTool
        .execute(json!({"name": "mcp__github__get_pr"}), &ctx)
        .await;
    assert!(result.is_error);
    assert_eq!(result.content, "not found");
}

// ── mcp_call: unknown tool returns error ──────────────────────────────────

#[tokio::test]
async fn call_unknown_tool_returns_error() {
    let stub: Arc<dyn McpTransport> =
        Arc::new(StubTransport::new(vec![tool_def("get_pr", "get a PR")]));
    let mut mgr = McpManager::new();
    mgr.register("github", stub);
    let ctx = make_ctx_with_mcp(mgr);

    let result = McpCallTool
        .execute(json!({"name": "mcp__github__unknown_tool"}), &ctx)
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("MCP call failed"));
}

// ── name filter: list and call agree, errors are typed ───────────────────

#[tokio::test]
async fn filtered_tool_is_unlisted_and_call_returns_typed_error() {
    let stub: Arc<dyn McpTransport> = Arc::new(
        StubTransport::new(vec![tool_def("keep", "kept"), tool_def("secret", "hidden")])
            .with_response("keep", ok_result("kept-ok"))
            .with_response("secret", ok_result("leaked")),
    );
    let mut mgr = McpManager::new();
    mgr.register("svc", stub);
    mgr.set_tool_name_filter(rightkit_mcp::agent_mcp::McpToolNameFilter {
        include: vec![],
        exclude: vec!["mcp__svc__secret".to_string()],
    });
    let ctx = make_ctx_with_mcp(mgr);

    let listed = ListMcpToolsTool.execute(json!({}), &ctx).await;
    assert!(listed.content.contains("mcp__svc__keep"));
    assert!(!listed.content.contains("mcp__svc__secret"));

    let denied = McpCallTool
        .execute(json!({"name": "mcp__svc__secret"}), &ctx)
        .await;
    assert!(denied.is_error);
    assert!(
        denied.content.contains("[tool_filtered]"),
        "{}",
        denied.content
    );
    assert!(!denied.content.contains("leaked"));

    let unknown = McpCallTool
        .execute(json!({"name": "mcp__svc__nope"}), &ctx)
        .await;
    assert!(
        unknown.content.contains("[tool_not_found]"),
        "{}",
        unknown.content
    );
}

// ── mcp_call: arguments default to {} ────────────────────────────────────

#[tokio::test]
async fn call_without_arguments_uses_empty_object() {
    let stub: Arc<dyn McpTransport> = Arc::new(
        StubTransport::new(vec![tool_def("ping", "ping")]).with_response("ping", ok_result("pong")),
    );
    let mut mgr = McpManager::new();
    mgr.register("svc", stub);
    let ctx = make_ctx_with_mcp(mgr);

    let result = McpCallTool
        .execute(json!({"name": "mcp__svc__ping"}), &ctx)
        .await;
    assert!(!result.is_error);
    assert!(result.content.contains("pong"));
}

// ── schema hint helper ────────────────────────────────────────────────────

#[test]
fn schema_hint_required_and_optional() {
    let schema = json!({
        "type": "object",
        "properties": {"q": {}, "limit": {}},
        "required": ["q"]
    });
    let hint = build_schema_hint(&schema);
    assert!(hint.contains("required: q"));
    assert!(hint.contains("optional: limit"));
}

#[test]
fn schema_hint_no_properties_returns_empty() {
    let hint = build_schema_hint(&json!({"type": "object"}));
    assert!(hint.is_empty());
}

#[test]
fn schema_hint_empty_properties_returns_empty() {
    let hint = build_schema_hint(&json!({"type": "object", "properties": {}}));
    assert!(hint.is_empty());
}

// ── list_mcp_resources ────────────────────────────────────────────────────

fn resource_def(uri: &str, name: &str) -> McpResourceDef {
    McpResourceDef {
        uri: uri.to_string(),
        name: name.to_string(),
        description: String::new(),
        mime_type: "text/plain".to_string(),
    }
}

#[tokio::test]
async fn list_resources_no_mcp_returns_error() {
    let ctx = make_ctx_no_mcp();
    let result = ListMcpResourcesTool
        .execute(json!({"server": "fs"}), &ctx)
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("No MCP servers"));
}

#[tokio::test]
async fn list_resources_missing_server_arg_returns_error() {
    let ctx = make_ctx_with_mcp(McpManager::new());
    let result = ListMcpResourcesTool.execute(json!({}), &ctx).await;
    assert!(result.is_error);
    assert!(result.content.contains("server"));
}

#[tokio::test]
async fn list_resources_returns_json_array() {
    let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![]).with_resources(vec![
        resource_def("file:///a.txt", "a.txt"),
        resource_def("file:///b.txt", "b.txt"),
    ]));
    let mut mgr = McpManager::new();
    mgr.register("fs", stub);
    let ctx = make_ctx_with_mcp(mgr);

    let result = ListMcpResourcesTool
        .execute(json!({"server": "fs"}), &ctx)
        .await;
    assert!(!result.is_error);
    assert!(result.content.contains("coderight-untrusted-context"));
    assert!(result.content.contains("file:///a.txt"));
    assert!(result.content.contains("file:///b.txt"));
}

#[tokio::test]
async fn list_resources_denies_server_not_allowed_for_context_channel() {
    let stub: Arc<dyn McpTransport> = Arc::new(
        StubTransport::new(vec![]).with_resources(vec![resource_def("file:///a.txt", "a.txt")]),
    );
    let mut mgr = McpManager::new();
    mgr.register("fs", stub);
    mgr.register_channel_permissions(
        "fs",
        rightkit_mcp::agent_mcp::McpChannelPermissions {
            allowed_channels: vec!["telegram".to_string()],
            allow_mutating: false,
        },
    );
    let ctx = make_ctx_with_mcp_channel(mgr, "desktop");

    let result = ListMcpResourcesTool
        .execute(json!({"server": "fs"}), &ctx)
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("not allowed"));
    assert!(result.content.contains("desktop"));
    assert!(result.content.contains("fs"));
}

#[tokio::test]
async fn list_resources_empty_returns_ok_message() {
    let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![]));
    let mut mgr = McpManager::new();
    mgr.register("empty", stub);
    let ctx = make_ctx_with_mcp(mgr);

    let result = ListMcpResourcesTool
        .execute(json!({"server": "empty"}), &ctx)
        .await;
    assert!(!result.is_error);
    assert!(result.content.contains("no resources"));
}

// ── read_mcp_resource ─────────────────────────────────────────────────────

#[tokio::test]
async fn read_resource_no_mcp_returns_error() {
    let ctx = make_ctx_no_mcp();
    let result = ReadMcpResourceTool
        .execute(json!({"server": "fs", "uri": "file:///a.txt"}), &ctx)
        .await;
    assert!(result.is_error);
}

#[tokio::test]
async fn read_resource_missing_server_returns_error() {
    let ctx = make_ctx_with_mcp(McpManager::new());
    let result = ReadMcpResourceTool
        .execute(json!({"uri": "file:///a.txt"}), &ctx)
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("server"));
}

#[tokio::test]
async fn read_resource_missing_uri_returns_error() {
    let ctx = make_ctx_with_mcp(McpManager::new());
    let result = ReadMcpResourceTool
        .execute(json!({"server": "fs"}), &ctx)
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("uri"));
}

#[tokio::test]
async fn read_resource_returns_text_content() {
    let content = McpResourceContent {
        uri: "file:///hello.txt".to_string(),
        text: Some("hello world".to_string()),
        blob: None,
        mime_type: "text/plain".to_string(),
    };
    let stub: Arc<dyn McpTransport> =
        Arc::new(StubTransport::new(vec![]).with_resource_content("file:///hello.txt", content));
    let mut mgr = McpManager::new();
    mgr.register("fs", stub);
    let ctx = make_ctx_with_mcp(mgr);

    let result = ReadMcpResourceTool
        .execute(json!({"server": "fs", "uri": "file:///hello.txt"}), &ctx)
        .await;
    assert!(!result.is_error);
    assert!(result.content.contains("hello world"));
    assert!(result.content.contains("coderight-untrusted-context"));
}

#[tokio::test]
async fn read_resource_denies_server_not_allowed_for_context_channel() {
    let content = McpResourceContent {
        uri: "file:///hello.txt".to_string(),
        text: Some("hello world".to_string()),
        blob: None,
        mime_type: "text/plain".to_string(),
    };
    let stub: Arc<dyn McpTransport> =
        Arc::new(StubTransport::new(vec![]).with_resource_content("file:///hello.txt", content));
    let mut mgr = McpManager::new();
    mgr.register("fs", stub);
    mgr.register_channel_permissions(
        "fs",
        rightkit_mcp::agent_mcp::McpChannelPermissions {
            allowed_channels: vec!["cli".to_string()],
            allow_mutating: false,
        },
    );
    let ctx = make_ctx_with_mcp_channel(mgr, "desktop");

    let result = ReadMcpResourceTool
        .execute(json!({"server": "fs", "uri": "file:///hello.txt"}), &ctx)
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("not allowed"));
    assert!(result.content.contains("desktop"));
    assert!(result.content.contains("fs"));
}

#[tokio::test]
async fn read_resource_returns_blob_content() {
    let content = McpResourceContent {
        uri: "file:///image.png".to_string(),
        text: None,
        blob: Some("abc123base64==".to_string()),
        mime_type: "image/png".to_string(),
    };
    let stub: Arc<dyn McpTransport> =
        Arc::new(StubTransport::new(vec![]).with_resource_content("file:///image.png", content));
    let mut mgr = McpManager::new();
    mgr.register("fs", stub);
    let ctx = make_ctx_with_mcp(mgr);

    let result = ReadMcpResourceTool
        .execute(json!({"server": "fs", "uri": "file:///image.png"}), &ctx)
        .await;
    assert!(!result.is_error);
    assert!(result.content.contains("image/png"));
    assert!(result.content.contains("abc123base64=="));
}
