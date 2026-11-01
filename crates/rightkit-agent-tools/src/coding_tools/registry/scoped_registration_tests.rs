// EXT-001 closure fixture: dynamic registrations are scoped and disposable.
//
// Required lifecycle shape:
//   register scope A -> catalog generation N -> provider request A snapshots N
//   -> unload A while request A is active -> request A stays on N
//   -> a future request sees N+1 -> no stale tools -> reconnect/re-register
//   produces exactly one live registration.
//
// This drives the ONE canonical dynamic catalog mechanism shared with
// HCTX-003 (`ToolCatalogGenerationTracker`); it deliberately does not
// introduce a second registry or a second generation counter.
use super::*;
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

struct ScopedFixtureTool {
    name: String,
    description: String,
}

impl ScopedFixtureTool {
    fn boxed(name: &str, description: &str) -> Box<dyn Tool> {
        Box::new(Self {
            name: name.to_string(),
            description: description.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl Tool for ScopedFixtureTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::ok("scoped-fixture")
    }
}

/// Provider-visible schema projection, in the same shape the production
/// provider client observes through `ToolCatalogGenerationTracker`.
fn definitions(registry: &ToolRegistry) -> Vec<Value> {
    let mut defs = registry
        .all()
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.input_schema(),
                }
            })
        })
        .collect::<Vec<_>>();
    // Deterministic order so generation reflects membership, not iteration luck.
    defs.sort_by(|a, b| {
        a["function"]["name"]
            .as_str()
            .cmp(&b["function"]["name"].as_str())
    });
    defs
}

#[test]
fn scope_unload_during_active_request_advances_only_future_generations() {
    let mut registry = ToolRegistry::new();
    let mut tracker = ToolCatalogGenerationTracker::default();
    let scope_a = RegistrationScopeId::new("mcp:scope-a");

    // register scope A
    registry
        .register_scoped(&scope_a, ScopedFixtureTool::boxed("alpha_one", "first"))
        .expect("register alpha_one");
    registry
        .register_scoped(&scope_a, ScopedFixtureTool::boxed("alpha_two", "second"))
        .expect("register alpha_two");
    assert_eq!(
        registry.scope_registrations(&scope_a),
        vec!["alpha_one".to_string(), "alpha_two".to_string()]
    );

    // provider request A snapshots generation N over an immutable schema set
    let request_a = tracker.observe(&definitions(&registry)).unwrap();
    let request_a_bytes = serde_json::to_vec(&definitions(&registry)).unwrap();
    let generation_n = request_a.generation;

    // unload scope A *while request A is in flight*
    let removed = registry.unload_scope(&scope_a);
    assert_eq!(removed, 2, "unload must remove every registration it owns");

    // request A is unaffected: its snapshot is by value, immutable.
    assert_eq!(request_a.generation, generation_n);
    assert_eq!(
            request_a_bytes,
            serde_json::to_vec(&vec![
                json!({"type":"function","function":{"name":"alpha_one","description":"first","parameters":{"type":"object","properties":{}}}}),
                json!({"type":"function","function":{"name":"alpha_two","description":"second","parameters":{"type":"object","properties":{}}}}),
            ])
            .unwrap(),
            "in-flight request bytes must stay byte-identical after unload"
        );

    // no stale tools survive the unload
    assert!(registry.get("alpha_one").is_none());
    assert!(registry.get("alpha_two").is_none());
    assert!(registry.scope_registrations(&scope_a).is_empty());
    assert!(registry.live_scopes().is_empty());

    // a future request sees a NEW generation
    let request_b = tracker.observe(&definitions(&registry)).unwrap();
    assert_eq!(
        request_b.generation,
        generation_n + 1,
        "unload must advance the catalog generation for future requests"
    );
    assert_ne!(request_a.digest_hex, request_b.digest_hex);
}

#[test]
fn reconnect_produces_exactly_one_live_registration() {
    let mut registry = ToolRegistry::new();
    let scope = RegistrationScopeId::new("mcp:flaky-server");

    for attempt in 0..5 {
        // A reconnect re-registers the same names; it must not accumulate.
        registry
            .register_scoped(&scope, ScopedFixtureTool::boxed("svc_tool", "v"))
            .expect("re-register on reconnect");
        assert_eq!(
            registry
                .all()
                .iter()
                .filter(|tool| tool.name() == "svc_tool")
                .count(),
            1,
            "attempt {attempt} produced a duplicate registration"
        );
        assert_eq!(registry.scope_registrations(&scope), vec!["svc_tool"]);
    }

    // A disconnect removes it entirely.
    assert_eq!(registry.unload_scope(&scope), 1);
    assert!(registry.get("svc_tool").is_none());
    // Unloading twice is idempotent, not an error.
    assert_eq!(registry.unload_scope(&scope), 0);
}

#[test]
fn scopes_cannot_steal_names_from_each_other_or_from_builtins() {
    let mut registry = ToolRegistry::new();
    let scope_a = RegistrationScopeId::new("ext:a");
    let scope_b = RegistrationScopeId::new("ext:b");

    registry
        .register_scoped(&scope_a, ScopedFixtureTool::boxed("shared_name", "a"))
        .expect("first owner registers");

    // Ownership is unambiguous: B cannot silently take A's name, because that
    // would break unload_scope(A)'s total-removal guarantee.
    let conflict = registry
        .register_scoped(&scope_b, ScopedFixtureTool::boxed("shared_name", "b"))
        .expect_err("second scope must be refused");
    assert_eq!(
        conflict,
        ScopedRegistrationError::OwnedByAnotherScope {
            tool: "shared_name".into(),
            owner: scope_a.clone(),
        }
    );

    // A built-in (unscoped) registration cannot be shadowed by a scope either.
    registry.register(ScopedFixtureTool::boxed("builtin_like", "builtin"));
    let shadow = registry
        .register_scoped(&scope_b, ScopedFixtureTool::boxed("builtin_like", "b"))
        .expect_err("shadowing a builtin must be refused");
    assert_eq!(
        shadow,
        ScopedRegistrationError::ShadowsBuiltin {
            tool: "builtin_like".into()
        }
    );

    // Unloading A leaves the builtin untouched.
    assert_eq!(registry.unload_scope(&scope_a), 1);
    assert!(registry.get("builtin_like").is_some());
}

/// EXT-001 production wiring: plugin tools registered from a real plugin
/// directory are owned by that plugin's scope, and unloading the plugin
/// removes every one of them. This exercises the SAME code path the
/// daemon uses (`build_default_with_plugins` ->
/// `register_plugin_tools_from_dir` -> `register_scoped`), not a
/// hand-built registry.
#[test]
fn plugin_tools_are_scope_owned_and_fully_unloadable() {
    let dir = tempfile::TempDir::new().unwrap();
    let plugins_root = dir.path().join("plugins");
    for (id, tools) in [
        ("alpha", vec!["alpha_one", "alpha_two"]),
        ("beta", vec!["beta_one"]),
    ] {
        let plugin_dir = plugins_root.join(id);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.py"), "").unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            serde_json::json!({
                "id": id,
                "name": id,
                "version": "1.0.0",
                "description": "fixture plugin",
                "entry": "plugin.py",
                "provides_tools": tools,
                "provides_commands": [],
                "enabled": true
            })
            .to_string(),
        )
        .unwrap();
    }

    let mut registry = ToolRegistry::build_default_with_plugins(&plugins_root);
    // Ownership is attributable through the real registration path.
    assert_eq!(
        registry.owning_scope("alpha_one"),
        Some(&ToolRegistry::plugin_scope("alpha"))
    );
    assert_eq!(
        registry.scope_registrations(&ToolRegistry::plugin_scope("alpha")),
        vec!["alpha_one".to_string(), "alpha_two".to_string()]
    );
    assert!(registry.get("beta_one").is_some());

    // Unloading one plugin removes ALL of its tools and none of the other's.
    assert_eq!(registry.unload_plugin("alpha"), 2);
    assert!(registry.get("alpha_one").is_none());
    assert!(registry.get("alpha_two").is_none());
    assert!(
        registry.get("beta_one").is_some(),
        "unloading one plugin must not disturb another"
    );
    // Built-ins survive an unload.
    assert!(registry.get("read_file").is_some());
    // Idempotent.
    assert_eq!(registry.unload_plugin("alpha"), 0);

    // The session-disable entry point used by the daemon.
    assert_eq!(registry.unload_disabled_plugins(&["beta".to_string()]), 1);
    assert!(registry.get("beta_one").is_none());
}
