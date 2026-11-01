//! A067/A166/A167: dynamic catalog over a fake MCP server whose tool list can
//! change at runtime and which can signal `tools/list_changed`.

use super::*;
use crate::agent_mcp::types::{McpToolAnnotations, McpToolResult};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;

fn tool(name: &str) -> McpToolDef {
    McpToolDef {
        name: name.into(),
        description: format!("{name} tool"),
        annotations: McpToolAnnotations {
            read_only_hint: Some(true),
            ..Default::default()
        },
        input_schema: serde_json::json!({"type": "object"}),
    }
}

#[derive(Default)]
struct FakeServer {
    tools: StdMutex<Vec<McpToolDef>>,
    list_calls: AtomicUsize,
    changed: AtomicU64,
    fail_list: StdMutex<Option<String>>,
}

impl FakeServer {
    fn with(names: &[&str]) -> Arc<Self> {
        let server = Self::default();
        *server.tools.lock().unwrap() = names.iter().map(|n| tool(n)).collect();
        Arc::new(server)
    }
    fn set_tools(&self, names: &[&str]) {
        *self.tools.lock().unwrap() = names.iter().map(|n| tool(n)).collect();
    }
    fn signal_list_changed(&self) {
        self.changed.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl McpTransport for FakeServer {
    fn tools_list_changed_seq(&self) -> u64 {
        self.changed.load(Ordering::SeqCst)
    }
    async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = self.fail_list.lock().unwrap().clone() {
            return Err(McpError::Transport(message));
        }
        Ok(self.tools.lock().unwrap().clone())
    }
    async fn call_tool(
        &self,
        name: &str,
        _args: serde_json::Value,
        _session: Option<&str>,
    ) -> Result<McpToolResult, McpError> {
        if self.tools.lock().unwrap().iter().any(|t| t.name == name) {
            Ok(McpToolResult {
                content: format!("ran {name}"),
                is_error: false,
            })
        } else {
            Err(McpError::ToolNotFound(name.to_string()))
        }
    }
    async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
        Ok(vec![])
    }
    async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
        Err(McpError::ResourceNotFound(uri.to_string()))
    }
}

fn manager(server: &Arc<FakeServer>) -> McpManager {
    let mut manager = McpManager::new();
    manager.register("fake", server.clone() as Arc<dyn McpTransport>);
    manager
}

fn names(tools: &[McpToolDef]) -> Vec<String> {
    tools.iter().map(|t| t.name.clone()).collect()
}

#[tokio::test]
async fn catalog_reflects_added_and_removed_tools_after_list_changed() {
    let server = FakeServer::with(&["alpha"]);
    let manager = manager(&server);
    assert_eq!(names(&manager.discover_all().await), ["mcp__fake__alpha"]);

    server.set_tools(&["alpha", "beta"]);
    server.signal_list_changed();
    assert_eq!(
        names(&manager.discover_all().await),
        ["mcp__fake__alpha", "mcp__fake__beta"]
    );
    let ok = manager
        .invoke("mcp__fake__beta", serde_json::json!({}), None)
        .await;
    assert_eq!(ok.unwrap().content, "ran beta");

    server.set_tools(&["beta"]);
    server.signal_list_changed();
    assert_eq!(names(&manager.discover_all().await), ["mcp__fake__beta"]);
    let gone = manager
        .invoke("mcp__fake__alpha", serde_json::json!({}), None)
        .await;
    assert!(matches!(gone, Err(McpError::ToolNotFound(_))), "{gone:?}");
}

#[tokio::test]
async fn catalog_is_live_even_without_a_list_changed_signal() {
    let server = FakeServer::with(&["alpha"]);
    let manager = manager(&server);
    manager.discover_all().await;
    server.set_tools(&["alpha", "gamma"]);
    assert_eq!(
        names(&manager.discover_all().await),
        ["mcp__fake__alpha", "mcp__fake__gamma"]
    );
}

#[tokio::test]
async fn list_changed_drops_a_stale_inflight_listing_and_registry() {
    let server = FakeServer::with(&["alpha"]);
    let manager = manager(&server);
    manager.discover_all().await;
    assert!(!manager.registry.lock().unwrap().is_empty());
    manager.notify_tools_list_changed("fake");
    assert!(manager.registry.lock().unwrap().is_empty());
    assert!(manager.inflight_listings.lock().unwrap().is_empty());
    assert_eq!(names(&manager.discover_all().await), ["mcp__fake__alpha"]);
}

#[tokio::test]
async fn unchanged_seq_does_not_reset_the_registry() {
    let server = FakeServer::with(&["alpha"]);
    let manager = manager(&server);
    manager.discover_all().await;
    manager.sync_list_changed("fake", &(server.clone() as Arc<dyn McpTransport>));
    // First observation of seq 0 equals the implicit baseline: no reset.
    assert!(!manager.registry.lock().unwrap().is_empty());
}

#[tokio::test]
async fn name_filter_hides_tools_from_listing_and_types_the_call_error() {
    let server = FakeServer::with(&["alpha", "danger"]);
    let mut manager = manager(&server);
    manager.set_tool_name_filter(McpToolNameFilter {
        include: vec![],
        exclude: vec!["mcp__fake__danger".into()],
    });
    assert_eq!(names(&manager.discover_all().await), ["mcp__fake__alpha"]);
    let filtered = manager
        .invoke("mcp__fake__danger", serde_json::json!({}), None)
        .await;
    let err = filtered.unwrap_err();
    assert!(matches!(err, McpError::ToolFiltered(_)), "{err:?}");
    assert_eq!(err.kind(), "tool_filtered");

    // The filter also applies to tools that appear after a list_changed.
    server.set_tools(&["alpha", "danger", "danger2"]);
    server.signal_list_changed();
    manager.set_tool_name_filter(McpToolNameFilter {
        include: vec!["mcp__fake__alpha".into()],
        exclude: vec![],
    });
    assert_eq!(names(&manager.discover_all().await), ["mcp__fake__alpha"]);
}

#[tokio::test]
async fn call_errors_are_typed() {
    let server = FakeServer::with(&["alpha"]);
    let manager = manager(&server);

    let unknown_tool = manager
        .invoke("mcp__fake__nope", serde_json::json!({}), None)
        .await;
    assert_eq!(unknown_tool.unwrap_err().kind(), "tool_not_found");

    let unknown_server = manager
        .invoke("mcp__ghost__alpha", serde_json::json!({}), None)
        .await;
    assert_eq!(unknown_server.unwrap_err().kind(), "tool_not_found");

    *server.fail_list.lock().unwrap() = Some("server crashed".into());
    let transport = manager
        .invoke("mcp__fake__alpha", serde_json::json!({}), None)
        .await;
    assert_eq!(transport.unwrap_err().kind(), "transport");
}
