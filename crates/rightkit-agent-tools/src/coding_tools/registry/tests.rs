use super::*;

#[test]
fn factory_keys_match_registered_names() {
    // CQ-16: the descriptor table keys are the selection surface — a key
    // that drifts from the tool's registered name() silently breaks
    // select-first builds for that tool.
    let mut built = 0usize;
    for (key, available, factory) in ToolRegistry::builtin_tool_factories() {
        if !available() {
            continue;
        }
        let Some(tool) = factory() else { continue };
        assert_eq!(
            tool.name(),
            key,
            "factory key must equal the tool's registered name"
        );
        built += 1;
    }
    // `tool_search` is generated from the catalog, while `run_program` is
    // constructed with a handle to the registry it invokes.
    let names: std::collections::HashSet<_> = ToolRegistry::build_default()
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(names.contains("run_program"));
    assert_eq!(
        built + 2,
        names.len(),
        "catalog has exactly two non-descriptor built-ins"
    );
}

#[test]
fn unconfigured_wasm_is_neither_registered_nor_advertised() {
    // CQ-16: the WASM seam ships without a runtime — the tool must stay
    // out of every model-callable surface until one is configured, while
    // remaining constructible for the internal seam.
    let reg = ToolRegistry::build_default();
    assert!(
        reg.get("wasm_compute").is_none(),
        "unconfigured WASM runtime must not register"
    );
    assert!(
        !crate::coding_tools::registry::default_tool_capabilities()
            .iter()
            .any(|cap| cap.name == "wasm_compute"),
        "unconfigured WASM must not appear in capability metadata"
    );
    assert!(
        !crate::coding_tools::registry::default_tool_render_classes()
            .iter()
            .any(|(name, _)| name == "wasm_compute"),
        "unconfigured WASM must not appear in render-class metadata"
    );
    // Selection can still name it — availability, not the allow-list,
    // withholds it, so a future runtime flips one probe.
    let selected = ToolRegistry::build_for_allowed(&["wasm_compute".to_string()]);
    assert!(selected.get("wasm_compute").is_none());
    // The seam itself stays constructible and fails closed.
    use crate::coding_tools::tool_trait::Tool;
    let tool = crate::coding_tools::wasm_compute::WasmComputeTool::new();
    assert_eq!(tool.name(), "wasm_compute");
}

#[test]
fn declared_metadata_matches_constructed_tools() {
    // Drift guard, availability-independent: every factory descriptor and
    // the generated tool_search must carry a metadata row that matches what
    // the constructed tool self-reports. Construction happens here in the
    // test only — production metadata queries stay construction-free.
    // Compile-time features gate the descriptor list itself, so this check
    // is automatically feature-scoped; the availability probe is ignored so
    // host-dependent tools (powershell, wasm_compute) are still guarded.
    use crate::coding_tools::registry::registry_impl::BUILTIN_CAPABILITY_META;
    use std::collections::BTreeMap;

    let meta: BTreeMap<&str, (&str, bool)> = BUILTIN_CAPABILITY_META
        .iter()
        .map(|(name, description, read_only)| (*name, (*description, *read_only)))
        .collect();

    let mut declared = std::collections::HashSet::new();
    for (name, _available, build) in ToolRegistry::builtin_tool_factories() {
        declared.insert(name);
        let (description, read_only) = meta
            .get(name)
            .copied()
            .unwrap_or_else(|| panic!("missing metadata row for declared tool {name}"));
        let tool = build().unwrap_or_else(|| panic!("factory for {name} returned no tool"));
        assert_eq!(tool.name(), name);
        assert_eq!(
            description,
            tool.description(),
            "description drift for {name}"
        );
        assert_eq!(read_only, tool.is_read_only(), "read_only drift for {name}");
    }
    declared.insert("tool_search");
    let default_registry = ToolRegistry::build_default();
    let search = default_registry
        .get("tool_search")
        .expect("generated tool_search must register on a default build");
    let (description, read_only) = meta
        .get("tool_search")
        .copied()
        .expect("missing metadata row for tool_search");
    assert_eq!(description, search.description());
    assert_eq!(read_only, search.is_read_only());

    for name in meta.keys() {
        assert!(
            declared.contains(name),
            "metadata row for undeclared tool {name}"
        );
    }
}

#[test]
fn advertised_set_matches_declared_available_tools() {
    // Availability contract: the advertised catalog is exactly the declared
    // tools whose probe passes on this host, plus generated tool_search. An
    // unavailable runtime advertises nothing; every advertised name must
    // carry a declared metadata row.
    use crate::coding_tools::registry::registry_impl::BUILTIN_CAPABILITY_META;

    let declared: std::collections::HashSet<&str> = BUILTIN_CAPABILITY_META
        .iter()
        .map(|(name, _, _)| *name)
        .collect();
    let mut expected: std::collections::HashSet<&str> = ToolRegistry::builtin_tool_factories()
        .into_iter()
        .filter(|(_, available, _)| available())
        .map(|(name, _, _)| name)
        .collect();
    expected.insert("tool_search");

    let advertised: std::collections::HashSet<String> =
        crate::coding_tools::registry::default_tool_capabilities()
            .into_iter()
            .map(|cap| cap.name)
            .collect();
    for name in &advertised {
        assert!(
            declared.contains(name.as_str()),
            "advertised {name} lacks a declared metadata row"
        );
        assert!(
            expected.contains(name.as_str()),
            "advertised {name} failed its availability probe on this host"
        );
    }
    for name in &expected {
        assert!(
            advertised.contains(*name),
            "available {name} is missing from advertised capabilities"
        );
    }
}

#[test]
fn narrow_build_instantiates_only_selected_tools() {
    // CQ-16: build_for_allowed must produce exactly the permitted set —
    // identical membership to the old build-then-retain contract.
    let reg = ToolRegistry::build_for_allowed(&["read_file".to_string()]);
    for name in reg.all().iter().map(|t| t.name()) {
        let permitted = ToolRegistry::permitted_names_for(&["read_file".to_string()]);
        assert!(
            permitted.contains(name) || name == "tool_search",
            "{name} was instantiated outside the permitted set"
        );
    }
}

#[test]
fn tool_registry_get_by_name() {
    let reg = ToolRegistry::build_default();
    assert!(reg.get("read_file").is_some());
    // CR-30: the dedicated retrieval schema is gone — externalized results
    // are read through ordinary `read_file` at
    // `<state-dir>/tool-results/<ref_id>.txt`.
    assert!(reg.get("read_tool_result").is_none());
    assert!(reg.get("write_file").is_some());
    assert!(reg.get("shell").is_some());
    // Offered only where an interpreter exists — `pwsh` is absent by
    // default off Windows.
    assert_eq!(
        reg.get("powershell").is_some(),
        crate::coding_tools::shell::powershell_available()
    );
    assert!(reg.get("web_search").is_some());
    assert!(reg.get("lsp").is_some());
}

#[test]
fn tool_registry_built_in_harness_tools() {
    let reg = ToolRegistry::build_default();
    assert!(
        reg.get("sequentialthinking").is_some(),
        "sequential thinking must be built-in"
    );
    assert!(reg.get("context7").is_some(), "context7 must be built-in");
    assert!(reg.get("get_time").is_some(), "get_time must be built-in");
}

#[test]
fn capabilities_report_real_registry_metadata() {
    let reg = ToolRegistry::build_default();
    let caps = reg.capabilities();
    let read_file = caps
        .iter()
        .find(|cap| cap.name == "read_file")
        .expect("read_file capability");
    assert_eq!(read_file.category, "files");
    assert!(read_file.read_only);
    assert!(read_file.description.contains("Read"));

    let shell = caps
        .iter()
        .find(|cap| cap.name == "shell")
        .expect("shell capability");
    assert_eq!(shell.category, "execution");
    assert!(!shell.read_only);

    assert!(caps.windows(2).all(|pair| {
        (pair[0].category.as_str(), pair[0].name.as_str())
            <= (pair[1].category.as_str(), pair[1].name.as_str())
    }));
}

#[test]
fn default_registry_exposes_background_task_tools() {
    let reg = ToolRegistry::build_default();
    for name in [
        "task_create",
        "task_update",
        "task_get",
        "task_list",
        "task_output",
        "task_stop",
    ] {
        assert!(reg.get(name).is_some(), "{name} should be registered");
    }
}

#[test]
fn default_registry_exposes_media_tools() {
    let reg = ToolRegistry::build_default();
    for name in ["ocr", "web_extract", "media_fetch", "ffmpeg_extract"] {
        assert!(reg.get(name).is_some(), "{name} should be registered");
    }
}

#[test]
fn shell_allowed_registry_excludes_media_tools() {
    let reg = ToolRegistry::build_for_allowed(&["shell".into()]);
    for name in ["ocr", "web_extract", "media_fetch", "ffmpeg_extract"] {
        assert!(
            reg.get(name).is_none(),
            "{name} must require a separate media grant"
        );
    }
}

#[test]
fn media_allowed_registry_includes_media_without_shell() {
    let reg = ToolRegistry::build_for_allowed(&["media".into()]);
    for name in [
        "ocr",
        "ocr_onnx",
        "web_extract",
        "media_fetch",
        "ffmpeg_extract",
    ] {
        assert!(reg.get(name).is_some(), "{name} should be granted by media");
    }
    assert!(reg.get("shell").is_none());
}

#[test]
fn default_registry_exposes_typed_lifecycle_tools() {
    let reg = ToolRegistry::build_default();
    for name in ["file_service", "git_write", "process"] {
        assert!(reg.get(name).is_some(), "{name} should be registered");
    }
}

#[test]
fn default_registry_includes_view_image() {
    let reg = ToolRegistry::build_default();
    assert!(
        reg.get("view_image").is_some(),
        "view_image should be registered by default"
    );
    let caps = reg.capabilities();
    let view_image = caps
        .iter()
        .find(|cap| cap.name == "view_image")
        .expect("view_image capability");
    assert_eq!(view_image.category, "files");
    assert!(view_image.read_only);
}

#[test]
fn read_file_allowed_registry_includes_view_image() {
    let reg = ToolRegistry::build_for_allowed(&["read_file".into()]);
    assert!(
        reg.get("view_image").is_some(),
        "view_image should be available when read_file is allowed"
    );
}

#[test]
fn removed_read_tool_result_is_gone_from_every_catalog_shape() {
    // CR-30: the default registry, the capability-ceiling registry (which
    // used to grant it via the `read_file` alias and ALWAYS_ON), and the
    // capabilities report must all have lost exactly this one entry.
    assert!(ToolRegistry::build_default()
        .get("read_tool_result")
        .is_none());
    assert!(ToolRegistry::build_for_allowed(&["read_file".into()])
        .get("read_tool_result")
        .is_none());
    assert!(!ToolRegistry::build_default()
        .capabilities()
        .iter()
        .any(|cap| cap.name == "read_tool_result"));
}

#[test]
fn default_registry_omits_synthetic_output_tool() {
    let reg = ToolRegistry::build_default();
    assert!(
        reg.get("synthetic_output").is_none(),
        "synthetic output is for tests/evaluation, not normal agents"
    );
}

#[test]
fn evaluation_registry_includes_synthetic_output_tool() {
    let reg = ToolRegistry::build_default_for_evaluation();
    assert!(reg.get("synthetic_output").is_some());
}

#[tokio::test]
async fn tool_search_reflects_default_registry_tools() {
    let dir = tempfile::TempDir::new().unwrap();
    let reg = ToolRegistry::build_default();
    let tool = reg.get("tool_search").expect("tool_search registered");
    let ctx = crate::coding_tools::ToolContext::new(
        "s1",
        dir.path(),
        rightkit_agent_policy::PermissionMode::Bypass,
        rightkit_agent_policy::WorkspaceTrust::Trusted,
    );
    let result = tool.execute(serde_json::json!({}), &ctx).await;

    assert!(!result.is_error);
    let tools: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    let names: std::collections::HashSet<&str> = tools
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for name in ["lsp", "task_create", "mcp_call"] {
        let targeted = tool.execute(serde_json::json!({"query": name}), &ctx).await;
        assert!(!targeted.is_error);
        let targeted: serde_json::Value = serde_json::from_str(&targeted.content).unwrap();
        assert!(
            targeted
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["name"].as_str() == Some(name)),
            "tool_search should find {name}"
        );
    }
    // CR-30: exactly one schema entry left the catalog — the dedicated
    // `read_tool_result` retrieval tool. tool_search must no longer
    // advertise it; the replacement path (read_file at the virtual path)
    // is proven by
    // `externalized_result_retrievable_via_normal_file_read` in
    // tool_search.rs.
    assert!(
        !names.contains("read_tool_result"),
        "read_tool_result schema must be gone from the catalog"
    );
    // Ask for this one directly rather than reading `names`. The no-query
    // catalog is byte-capped at CATALOG_DUMP_MAX_BYTES and drops whole
    // entries past the cut, and `powershell` sorts beyond it — so its
    // absence from `names` says nothing about whether it is registered.
    //
    // Two conditions gate registration and the expectation honours both:
    // `PowerShellTool` registers only under the `host` feature, while
    // `powershell_available()` reports the machine's interpreter
    // irrespective of features.
    let powershell_expected =
        cfg!(feature = "coding_tools-host") && crate::coding_tools::shell::powershell_available();
    let powershell_hit = tool
        .execute(serde_json::json!({"query": "powershell"}), &ctx)
        .await;
    assert!(!powershell_hit.is_error);
    let powershell_hit: serde_json::Value = serde_json::from_str(&powershell_hit.content).unwrap();
    let powershell_listed = powershell_hit
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"].as_str() == Some("powershell"));
    assert_eq!(
            powershell_listed, powershell_expected,
            "powershell must be searchable exactly when the host feature is on and an interpreter exists"
        );
    assert!(
        !names.contains("synthetic_output"),
        "normal tool_search must not advertise synthetic_output"
    );
}

#[tokio::test]
async fn registry_loads_enabled_plugin_tools_from_plugin_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let plugin_dir = dir.path().join("plugins").join("fixture");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let script = plugin_dir.join("plugin.py");
    std::fs::write(
        &script,
        r#"
import json, sys
payload = json.load(sys.stdin)
print(json.dumps({"content": "plugin saw " + payload["args"]["value"], "isError": False}))
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::json!({
            "id": "fixture",
            "name": "Fixture",
            "version": "1.0.0",
            "description": "fixture plugin",
            "entry": "plugin.py",
            "provides_tools": ["fixture_tool"],
            "provides_commands": [],
            "enabled": true
        })
        .to_string(),
    )
    .unwrap();

    let reg = ToolRegistry::build_default_with_plugins(dir.path().join("plugins"));
    let tool = reg.get("fixture_tool").expect("plugin tool registered");
    let ctx = crate::coding_tools::ToolContext::new(
        "s1",
        dir.path(),
        rightkit_agent_policy::PermissionMode::Bypass,
        rightkit_agent_policy::WorkspaceTrust::Trusted,
    );
    // This test's subject is plugin dispatch/loading, not sandbox
    // confinement: swap in the unconfined test-double launch adapter so
    // the child can reach the tempdir-backed fixture plugin script.
    crate::coding_tools::plugin_tool::set_test_launch_adapter(Some(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    )));
    let result = tool
        .execute(serde_json::json!({ "value": "hello" }), &ctx)
        .await;
    crate::coding_tools::plugin_tool::set_test_launch_adapter(None);

    assert!(!result.is_error, "{}", result.content);
    assert_eq!(result.content, "plugin saw hello");
}

#[test]
fn registry_uses_plugin_tool_metadata() {
    let dir = tempfile::TempDir::new().unwrap();
    let plugin_dir = dir.path().join("plugins").join("fixture");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.py"), "print('{}')").unwrap();
    std::fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::json!({
            "id": "fixture",
            "name": "Fixture",
            "version": "1.0.0",
            "description": "fixture plugin",
            "entry": "plugin.py",
            "providesTools": ["fixture_tool"],
            "providesCommands": [],
            "tools": {
                "fixture_tool": {
                    "description": "Inspect fixture metadata",
                    "readOnly": true,
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "value": { "type": "string" }
                        },
                        "required": ["value"]
                    }
                }
            },
            "enabled": true
        })
        .to_string(),
    )
    .unwrap();

    let reg = ToolRegistry::build_default_with_plugins(dir.path().join("plugins"));
    let tool = reg.get("fixture_tool").expect("plugin tool registered");
    assert_eq!(tool.description(), "Inspect fixture metadata");
    assert!(tool.is_read_only());
    let schema = tool.input_schema();
    assert_eq!(schema["required"][0], "value");
    assert_eq!(schema["properties"]["value"]["type"], "string");
}

#[tokio::test]
async fn tool_search_reflects_enabled_plugin_tools() {
    let dir = tempfile::TempDir::new().unwrap();
    let plugin_dir = dir.path().join("plugins").join("fixture");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.py"), "print('{}')").unwrap();
    std::fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::json!({
            "id": "fixture",
            "name": "Fixture",
            "version": "1.0.0",
            "description": "fixture plugin",
            "entry": "plugin.py",
            "provides_tools": ["fixture_tool"],
            "provides_commands": [],
            "enabled": true
        })
        .to_string(),
    )
    .unwrap();

    let reg = ToolRegistry::build_default_with_plugins(dir.path().join("plugins"));
    let tool = reg.get("tool_search").expect("tool_search registered");
    let ctx = crate::coding_tools::ToolContext::new(
        "s1",
        dir.path(),
        rightkit_agent_policy::PermissionMode::Bypass,
        rightkit_agent_policy::WorkspaceTrust::Trusted,
    );
    let result = tool
        .execute(serde_json::json!({ "query": "fixture" }), &ctx)
        .await;

    assert!(!result.is_error);
    let tools: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert!(tools
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"].as_str() == Some("fixture_tool")));
}

#[test]
fn registry_skips_disabled_plugin_tools() {
    let dir = tempfile::TempDir::new().unwrap();
    let plugin_dir = dir.path().join("plugins").join("fixture");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.py"), "print('unused')").unwrap();
    std::fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::json!({
            "id": "fixture",
            "name": "Fixture",
            "version": "1.0.0",
            "description": "fixture plugin",
            "entry": "plugin.py",
            "provides_tools": ["fixture_tool"],
            "provides_commands": [],
            "enabled": false
        })
        .to_string(),
    )
    .unwrap();

    let reg = ToolRegistry::build_default_with_plugins(dir.path().join("plugins"));
    assert!(reg.get("fixture_tool").is_none());
}

#[test]
fn tool_registry_get_unknown_returns_none() {
    let reg = ToolRegistry::build_default();
    assert!(reg.get("zzz_does_not_exist").is_none());
}

#[test]
fn tool_registry_all_has_all_tools() {
    let reg = ToolRegistry::build_default();
    assert!(reg.all().len() >= 17);
}

#[test]
fn tool_registry_new_is_empty() {
    let reg = ToolRegistry::new();
    assert!(reg.all().is_empty());
}

#[test]
fn build_for_allowed_caps_tools_and_keeps_meta() {
    // A read-only researcher: search + read + web, nothing else.
    let reg = ToolRegistry::build_for_allowed(&["read_file".into(), "search".into(), "web".into()]);
    assert!(reg.get("read_file").is_some());
    assert!(reg.get("lsp").is_some());
    assert!(reg.get("search").is_some());
    assert!(
        reg.get("web_search").is_some(),
        "web alias expands to web_search"
    );
    assert!(
        reg.get("browser_fetch").is_some(),
        "web alias expands to browser_fetch"
    );
    // Meta tools always present.
    assert!(reg.get("tool_search").is_some());
    assert!(reg.get("finalize_artifact").is_some());
    // Capability ceiling: no write, no shell, no spawning.
    assert!(reg.get("write_file").is_none());
    assert!(reg.get("shell").is_none());
    assert!(reg.get("powershell").is_none());
    assert!(reg.get("spawn_subagent").is_none());
}

#[test]
fn build_for_allowed_git_alias_expands() {
    let reg = ToolRegistry::build_for_allowed(&["git".into()]);
    assert!(reg.get("git_read").is_some());
    assert!(reg.get("git_diff").is_some());
    assert!(reg.get("write_file").is_none());
}

#[test]
fn build_for_allowed_shell_grants_background_task_tools() {
    let reg = ToolRegistry::build_for_allowed(&["shell".into()]);
    assert!(reg.get("shell").is_some());
    assert_eq!(
        reg.get("powershell").is_some(),
        crate::coding_tools::shell::powershell_available()
    );
    assert!(reg.get("get_shell_output").is_some());
    assert!(reg.get("kill_shell").is_some());
    assert!(reg.get("task_create").is_some());
    assert!(reg.get("task_output").is_some());
    assert!(reg.get("task_stop").is_some());
}

#[test]
fn every_builtin_tool_has_a_finite_render_class_and_unknown_is_generic() {
    let reg = ToolRegistry::build_default();
    for tool in reg.all() {
        let class = crate::coding_tools::registry::tool_render_class(tool.name());
        // No tool should ever be unclassified; unknown/dynamic tools are
        // explicitly Generic (bounded), never an unbounded fallback.
        assert!(
            matches!(
                class,
                crate::coding_tools::registry::ToolRenderClass::FileRead
                    | crate::coding_tools::registry::ToolRenderClass::FileWrite
                    | crate::coding_tools::registry::ToolRenderClass::Shell
                    | crate::coding_tools::registry::ToolRenderClass::Search
                    | crate::coding_tools::registry::ToolRenderClass::Network
                    | crate::coding_tools::registry::ToolRenderClass::Generic
            ),
            "tool {} has no render class",
            tool.name()
        );
    }
    assert_eq!(
        crate::coding_tools::registry::tool_render_class("some_mcp_tool_nobody_registered"),
        crate::coding_tools::registry::ToolRenderClass::Generic
    );
}

#[test]
fn render_classes_covers_every_production_tool() {
    let reg = ToolRegistry::build_default();
    let classes = reg.render_classes();
    assert_eq!(classes.len(), reg.all().len());
    for (name, class) in classes {
        assert_eq!(
            class,
            crate::coding_tools::registry::tool_render_class(name)
        );
        // Every built-in must have an intentional class, not just Generic
        // by default — Generic is deliberate for a subset, but the mapping
        // must exist.
    }
}

#[test]
fn collab_subagent_tools_are_registered_and_in_agents_toolset() {
    // P-11: followup_subagent, wait_for_subagents and interrupt_subagent
    // must be registered beside spawn_subagent, always available, and
    // grouped under the same "agents" toolset.
    let collab = [
        "spawn_subagent",
        "followup_subagent",
        "wait_for_subagents",
        "interrupt_subagent",
    ];
    let factory_names: std::collections::HashSet<&str> = ToolRegistry::builtin_tool_factories()
        .into_iter()
        .filter(|(_, available, _)| available())
        .map(|(name, _, _)| name)
        .collect();
    let reg = ToolRegistry::build_default();
    for name in collab {
        assert!(
            factory_names.contains(name),
            "{name} must have a registered factory"
        );
        assert!(
            reg.get(name).is_some(),
            "{name} must be present in the default registry"
        );
        assert_eq!(
            super::registry_impl::tool_category(name),
            "agents",
            "{name} must belong to the agents toolset"
        );
    }
}
