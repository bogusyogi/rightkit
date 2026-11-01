use crate::pdp::{PolicyDecisionPoint, PolicyRequest, PolicyRule};
use crate::types::{PermissionDecision, PermissionMode, WorkspaceTrust};
use crate::{CapabilityClass, PermissionEngine};

fn abac_engine_from_yaml(yaml: &str) -> PermissionEngine {
    let pdp = PolicyDecisionPoint::from_yaml_like(yaml).expect("yaml parse");
    let mut e = PermissionEngine::new();
    e.set_pdp(pdp);
    e
}

#[test]
fn abac_allow_overrides_ask_mode() {
    // Ask mode normally -> Ask for shell, but ABAC allow should win with abac source
    let yaml = r#"
rules:
  - id: allow-bash-for-sandbox
    effect: allow
    tool: shell
    attribute: env
    op: eq
    value: sandbox
"#;
    let e = abac_engine_from_yaml(yaml);
    let req = PolicyRequest::new("shell").with_attribute("env", "sandbox");
    let v = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::Shell,
        PermissionMode::Ask,
        WorkspaceTrust::Trusted,
        None,
        false,
    );
    assert_eq!(v.decision, PermissionDecision::Allow);
    assert_eq!(v.policy_source, "abac:allow-bash-for-sandbox");
    assert!(v.reason.contains("allow-bash-for-sandbox"));
}

#[test]
fn abac_first_match_wins() {
    let yaml = r#"
rules:
  - id: deny-first
    effect: deny
    tool: shell
  - id: allow-second
    effect: allow
    tool: shell
"#;
    let e = abac_engine_from_yaml(yaml);
    let req = PolicyRequest::new("shell");
    let v = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::Shell,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
        None,
        false,
    );
    // first rule wins even though second would allow
    assert_eq!(v.decision, PermissionDecision::Deny);
    assert_eq!(v.policy_source, "abac:deny-first");
}

#[test]
fn abac_deny_overrides_bypass_allow() {
    let yaml = r#"
rules:
  - id: block-shell-prod
    effect: deny
    tool: shell
    attribute: env
    op: eq
    value: prod
"#;
    let e = abac_engine_from_yaml(yaml);
    let req = PolicyRequest::new("shell").with_attribute("env", "prod");
    let v = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::Shell,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
        None,
        false,
    );
    assert_eq!(v.decision, PermissionDecision::Deny);
    assert_eq!(v.policy_source, "abac:block-shell-prod");
}

#[test]
fn abac_no_match_falls_through_to_mode() {
    let yaml = r#"
rules:
  - id: allow-only-read
    effect: allow
    tool: read_file
"#;
    let e = abac_engine_from_yaml(yaml);
    let req = PolicyRequest::new("shell");
    let v = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::Shell,
        PermissionMode::Ask,
        WorkspaceTrust::Trusted,
        None,
        false,
    );
    // no ABAC match, so mode Ask -> Ask
    assert_eq!(v.decision, PermissionDecision::Ask);
    assert_eq!(v.policy_source, "mode:ask");
}

#[test]
fn abac_from_yaml_like_first_match_wins_with_conditions() {
    let yaml = r#"
rules:
  - id: prod-deny
    effect: deny
    tool: Write
    attribute: env
    op: eq
    value: prod
  - id: general-allow
    effect: allow
    tool: Write
"#;
    let pdp = PolicyDecisionPoint::from_yaml_like(yaml).expect("parse");
    // prod should hit first rule
    let req_prod = PolicyRequest::new("Write").with_attribute("env", "prod");
    assert_eq!(pdp.evaluate(&req_prod), PermissionDecision::Deny);
    assert_eq!(pdp.matched_rule(&req_prod).unwrap().id, "prod-deny");

    // sandbox hits second
    let req_sand = PolicyRequest::new("Write").with_attribute("env", "sandbox");
    assert_eq!(pdp.evaluate(&req_sand), PermissionDecision::Allow);
    assert_eq!(pdp.matched_rule(&req_sand).unwrap().id, "general-allow");
}

#[test]
fn abac_hard_denial_not_overridden() {
    // Even a permissive ABAC allow cannot bypass deny-glob or locked trust
    let yaml = r#"
rules:
  - id: allow-all
    effect: allow
    tool: "*"
"#;
    let e = abac_engine_from_yaml(yaml);
    let req = PolicyRequest::new("write_file");
    // locked trust is hard denial
    let v = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::WriteFile,
        PermissionMode::Bypass,
        WorkspaceTrust::Locked,
        Some("src/main.rs"),
        false,
    );
    assert_eq!(v.decision, PermissionDecision::Deny);
    assert_eq!(v.policy_source, "trust:locked");

    // deny-glob is hard denial
    let v2 = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::WriteFile,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
        Some(".git/config"),
        false,
    );
    assert_eq!(v2.decision, PermissionDecision::Deny);
    assert!(v2.policy_source.starts_with("deny-glob:"));
}

#[test]
fn abac_clear_pdp_restores_baseline() {
    let yaml = r#"
rules:
  - id: allow-shell
    effect: allow
    tool: shell
"#;
    let mut e = abac_engine_from_yaml(yaml);
    let req = PolicyRequest::new("shell");
    let v1 = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::Shell,
        PermissionMode::Ask,
        WorkspaceTrust::Trusted,
        None,
        false,
    );
    assert_eq!(v1.policy_source, "abac:allow-shell");

    e.clear_pdp();
    let v2 = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::Shell,
        PermissionMode::Ask,
        WorkspaceTrust::Trusted,
        None,
        false,
    );
    assert_eq!(v2.policy_source, "mode:ask");
}

#[test]
fn abac_policy_source_format() {
    let mut pdp = PolicyDecisionPoint::new();
    pdp.add_rule(PolicyRule::new(
        "my-rule-123",
        PermissionDecision::Allow,
        "Read",
    ));
    let mut e = PermissionEngine::new();
    e.set_pdp(pdp);
    let req = PolicyRequest::new("Read");
    let v = e.evaluate_with_request(
        Some(&req),
        CapabilityClass::ReadFile,
        PermissionMode::Ask,
        WorkspaceTrust::Trusted,
        None,
        false,
    );
    assert_eq!(v.policy_source, "abac:my-rule-123");
}
