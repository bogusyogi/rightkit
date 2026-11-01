use rightkit_remote_gateway::config::{GatewayConfig, config};
use rightkit_remote_gateway::{Call, Origin, execute, manifest, redact};

#[test]
fn the_manifest_lists_built_tools_and_refuses_unknown_ones() {
    let tools = manifest::tools();
    assert!(tools.len() > 20, "the bundled manifest is readable");
    assert!(manifest::require("chat_peers").is_ok());
    let err = manifest::require("no.such.tool").expect_err("unknown tools are refused");
    assert_eq!(err.code, "unknown_tool");
}

#[test]
fn an_unknown_tool_is_refused_before_anything_runs() {
    let call = Call {
        client: "test".to_string(),
        tool: "no.such.tool".to_string(),
        ..Call::default()
    };
    let err = execute(&call).expect_err("refused");
    assert_eq!(err.code, "unknown_tool");
}

#[test]
fn origins_are_named_for_the_activity_record() {
    assert_eq!(Origin::Local.as_str(), "local");
    assert_eq!(Origin::Gateway.as_str(), "gateway");
    assert_eq!(Origin::default(), Origin::Local);
}

#[test]
fn the_default_configuration_is_product_neutral() {
    let defaults = GatewayConfig::default();
    assert_eq!(defaults.engine_service, "rightkit");
    assert_eq!(defaults.engine_executable, "rightkit-engine");
    assert!(defaults.protected_app_prefixes.is_empty());
    assert_eq!(config().default_label, "rightkit-cli");
}

#[test]
fn redaction_is_re_exported_from_the_approvals_crate() {
    let token = format!("gh{}{}", "p_", "c".repeat(36));
    let (clean, count) = redact::redact(&format!("a {token} b"));
    assert_eq!((clean.as_str(), count), ("a [redacted] b", 1));
}
