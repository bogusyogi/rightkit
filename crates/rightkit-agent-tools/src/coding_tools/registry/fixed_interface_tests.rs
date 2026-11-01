use super::*;
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};
use serde_json::{json, Value};

struct ScopedStubTool {
    name: String,
}

#[async_trait::async_trait]
impl Tool for ScopedStubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "scoped stub"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::ok("stub")
    }
}

fn fixed_registry() -> ToolRegistry {
    ToolRegistry::build_default()
        .with_lazy_loading(true)
        .with_model_interface(ModelToolInterface::FixedV1)
}

fn def_names(defs: &[Value]) -> Vec<String> {
    defs.iter()
        .filter_map(|d| d["function"]["name"].as_str().map(str::to_string))
        .collect()
}

fn add_scoped_stubs(registry: &mut ToolRegistry, scope: &str, count: usize, base: usize) {
    let scope_id = RegistrationScopeId::new(scope);
    for i in 0..count {
        registry
            .register_scoped(
                &scope_id,
                Box::new(ScopedStubTool {
                    name: format!("stub_{base}_{i}"),
                }),
            )
            .expect("register scoped stub");
    }
}

#[test]
fn fixed_interface_defs_byte_identical_after_0_5_50_scoped_registrations() {
    let mut registry = fixed_registry();
    let promoted = PromotedToolState::default();
    let baseline = serde_json::to_vec(&fixed_v1_tool_defs(&registry)).unwrap();
    assert_eq!(
        baseline,
        serde_json::to_vec(&tool_defs_for_messages(&registry, &promoted)).unwrap()
    );
    assert_eq!(
        baseline,
        serde_json::to_vec(
            &tool_defs_for_messages_checked(&registry, &promoted).expect("budget ok")
        )
        .unwrap()
    );

    add_scoped_stubs(&mut registry, "mcp:a", 5, 0);
    assert_eq!(
        baseline,
        serde_json::to_vec(&tool_defs_for_messages_checked(&registry, &promoted).unwrap()).unwrap(),
        "5 scoped registrations must not change the fixed projection"
    );

    add_scoped_stubs(&mut registry, "mcp:b", 45, 5);
    assert_eq!(registry.all().len(), 50 + baseline_kernel_count(&registry));
    assert_eq!(
        baseline,
        serde_json::to_vec(&tool_defs_for_messages_checked(&registry, &promoted).unwrap()).unwrap(),
        "50 scoped registrations must not change the fixed projection"
    );
}

fn baseline_kernel_count(registry: &ToolRegistry) -> usize {
    registry
        .all()
        .iter()
        .filter(|t| !t.name().starts_with("stub_"))
        .count()
}

#[test]
fn fixed_interface_defs_use_exact_canonical_kernel_order_and_live_subset() {
    let registry = fixed_registry();
    let defs = fixed_v1_tool_defs(&registry);
    assert_eq!(
        def_names(&defs),
        FIXED_V1_TOOL_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "fixed defs must follow the canonical kernel order exactly"
    );
    assert!(!def_names(&defs).iter().any(|n| n == "github"));
    assert!(def_names(&defs).iter().any(|n| n == "exec"));
}

#[test]
fn fixed_interface_omits_missing_kernel_names_from_live_subset() {
    let registry = ToolRegistry::new()
        .with_model_interface(ModelToolInterface::FixedV1)
        .with_lazy_loading(true);
    let defs = fixed_v1_tool_defs(&registry);
    assert_eq!(
        def_names(&defs),
        vec!["exec".to_string()],
        "unregistered kernel names are omitted; exec is admitted by the interface"
    );

    let mut registry = ToolRegistry::new()
        .with_model_interface(ModelToolInterface::FixedV1)
        .with_lazy_loading(true);
    registry.register(Box::new(ScopedStubTool {
        name: "read_file".to_string(),
    }));
    let defs = fixed_v1_tool_defs(&registry);
    assert_eq!(
        def_names(&defs),
        vec!["read_file".to_string(), "exec".to_string()]
    );
}

#[test]
fn fixed_interface_legacy_promoted_still_promotes_through_tool_search() {
    let registry = ToolRegistry::build_default().with_lazy_loading(true);
    assert_eq!(
        registry.model_interface(),
        ModelToolInterface::LegacyPromoted
    );
    let promoted = PromotedToolState::default();
    let before = tool_defs_for_messages(&registry, &promoted);
    assert!(
        !def_names(&before).iter().any(|n| n == "github"),
        "deferred tools are absent before promotion"
    );

    let mut promoted = PromotedToolState::default();
    let messages = vec![
        TranscriptMessage::Assistant {
            id: "a1".into(),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "ts-1".into(),
                name: "tool_search".into(),
                arguments_json: "{}".into(),
            }],
            pinned: false,
        },
        TranscriptMessage::Tool {
            id: "r1".into(),
            created_at: "t".into(),
            tool_call_id: "ts-1".into(),
            summary: json!([{"name": "github"}]).to_string(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        },
    ];
    promoted.accumulate(&messages);
    let after = tool_defs_for_messages(&registry, &promoted);
    assert!(
        def_names(&after).iter().any(|n| n == "github"),
        "LegacyPromoted must surface the promoted deferred tool"
    );

    let fixed = fixed_registry();
    let fixed_after = tool_defs_for_messages(&fixed, &promoted);
    assert!(
        !def_names(&fixed_after).iter().any(|n| n == "github"),
        "FixedV1 ignores promotion history"
    );
}

#[test]
fn fixed_interface_descriptor_reports_identity_digest_and_flags() {
    let registry = ToolRegistry::build_default();
    let descriptor = registry
        .capability_descriptor("read_file")
        .expect("read_file descriptor");
    assert_eq!(descriptor.id, "tool:read_file");
    assert_eq!(descriptor.name, "read_file");
    assert_eq!(descriptor.availability, "available");
    assert!(descriptor.read_only);
    assert_eq!(
        descriptor.input_schema,
        registry.get("read_file").unwrap().input_schema()
    );
    let expected = {
        let digest =
            rightkit_crypto::sha256(&serde_json::to_vec(&descriptor.input_schema).unwrap());
        rightkit_crypto::hex_encode(&digest)
    };
    assert_eq!(descriptor.schema_digest, expected);
    assert_eq!(descriptor.schema_digest.len(), 64);
    assert_eq!(
        descriptor.schema_digest,
        descriptor.schema_digest.to_lowercase()
    );
    assert!(registry.capability_descriptor("missing_tool").is_none());

    let descriptors = registry.capability_descriptors();
    let mut sorted = descriptors.clone();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(descriptors, sorted, "descriptors are sorted by id");

    let fixed = fixed_registry();
    assert!(fixed
        .capability_descriptors()
        .iter()
        .any(|d| d.id == "tool:exec"));
}

#[test]
fn fixed_interface_gates_exec_membership() {
    let legacy = ToolRegistry::build_default();
    assert_eq!(legacy.model_interface(), ModelToolInterface::LegacyPromoted);
    assert!(legacy.get("exec").is_none());
    assert!(!legacy
        .capability_descriptors()
        .iter()
        .any(|d| d.id == "tool:exec"));

    let narrow = ToolRegistry::build_for_allowed(&["read_file".to_string(), "exec".to_string()]);
    assert!(narrow.get("exec").is_none());

    let mobile = ToolRegistry::build_for_allowed_mobile(
        &["read_file".to_string()],
        MobileToolCapabilities::default(),
    );
    assert!(mobile.get("exec").is_none());

    let fixed = fixed_registry();
    assert_eq!(
        fixed
            .all()
            .iter()
            .filter(|tool| tool.name() == "exec")
            .count(),
        1
    );
    assert!(def_names(&fixed_v1_tool_defs(&fixed))
        .iter()
        .any(|name| name == "exec"));

    let reselected = fixed_registry().with_model_interface(ModelToolInterface::FixedV1);
    assert_eq!(
        reselected
            .all()
            .iter()
            .filter(|tool| tool.name() == "exec")
            .count(),
        1
    );

    let back_to_legacy = fixed_registry().with_model_interface(ModelToolInterface::LegacyPromoted);
    assert!(back_to_legacy.get("exec").is_none());

    let mut denied = fixed_registry();
    denied.remove("exec");
    assert!(denied.get("exec").is_none());
    assert!(!def_names(&fixed_v1_tool_defs(&denied))
        .iter()
        .any(|name| name == "exec"));
}

#[test]
fn fixed_interface_refuses_scoped_exec_registration() {
    let mut registry = ToolRegistry::build_default();
    let scope = RegistrationScopeId::new("plugin:foreign");
    let error = registry
        .register_scoped(
            &scope,
            Box::new(ScopedStubTool {
                name: "exec".to_string(),
            }),
        )
        .expect_err("scoped exec registration must be refused");
    assert!(matches!(
        error,
        ScopedRegistrationError::ShadowsBuiltin { ref tool } if tool == "exec"
    ));
    assert!(registry.get("exec").is_none());
}

#[test]
fn fixed_interface_replaces_foreign_exec_with_builtin_handler() {
    let mut registry = ToolRegistry::build_default();
    registry.register(Box::new(ScopedStubTool {
        name: "exec".to_string(),
    }));
    assert_eq!(registry.get("exec").unwrap().description(), "scoped stub");

    let registry = registry.with_model_interface(ModelToolInterface::FixedV1);
    let execs: Vec<&str> = registry
        .all()
        .iter()
        .filter(|tool| tool.name() == "exec")
        .map(|tool| tool.description())
        .collect();
    assert_eq!(
        execs,
        vec![crate::coding_tools::exec::EXEC_TOOL_DESCRIPTION]
    );

    let reselected = registry.with_model_interface(ModelToolInterface::FixedV1);
    assert_eq!(
        reselected
            .all()
            .iter()
            .filter(|tool| tool.name() == "exec")
            .count(),
        1
    );
    assert_eq!(
        reselected.get("exec").unwrap().description(),
        crate::coding_tools::exec::EXEC_TOOL_DESCRIPTION
    );
}

#[test]
fn fixed_interface_resolve_capability_distinguishes_failure_modes() {
    let registry = ToolRegistry::build_default();
    let descriptor = registry.capability_descriptor("read_file").unwrap();

    let err = registry
        .resolve_capability("read_file", &descriptor.schema_digest)
        .err()
        .unwrap();
    assert!(err.contains("invalid capability id"), "{err}");

    let err = registry
        .resolve_capability("tool:missing_tool", &descriptor.schema_digest)
        .err()
        .unwrap();
    assert!(err.contains("unavailable"), "{err}");

    let err = registry
        .resolve_capability("tool:read_file", "0".repeat(64).as_str())
        .err()
        .unwrap();
    assert!(err.contains("stale schema digest"), "{err}");

    let tool = registry
        .resolve_capability("tool:read_file", &descriptor.schema_digest)
        .expect("exact id + digest resolves");
    assert_eq!(tool.name(), "read_file");
}
