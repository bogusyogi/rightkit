mod tests {
    use super::super::*;

    #[test]
    fn tool_match_exact() {
        assert!(tool_matches("Read", "Read"));
        assert!(!tool_matches("Read", "Write"));
    }

    #[test]
    fn tool_match_wildcard() {
        assert!(tool_matches("*", "Read"));
        assert!(tool_matches("*", "Bash"));
        assert!(tool_matches("Edit*", "EditFile"));
        assert!(!tool_matches("Edit*", "Read"));
    }

    #[test]
    fn condition_eq() {
        let cond = Condition::new("env", CondOp::Eq, "prod");
        let attrs = vec![Attribute::new("env", "prod")];
        assert!(cond.matches(&attrs));

        let attrs_no_match = vec![Attribute::new("env", "dev")];
        assert!(!cond.matches(&attrs_no_match));
    }

    #[test]
    fn condition_noteq() {
        let cond = Condition::new("env", CondOp::NotEq, "prod");
        let attrs = vec![Attribute::new("env", "dev")];
        assert!(cond.matches(&attrs));

        let attrs_no_match = vec![Attribute::new("env", "prod")];
        assert!(!cond.matches(&attrs_no_match));
    }

    #[test]
    fn condition_contains() {
        let cond = Condition::new("path", CondOp::Contains, "secret");
        let attrs = vec![Attribute::new("path", "/home/secret/file.txt")];
        assert!(cond.matches(&attrs));

        let attrs_no_match = vec![Attribute::new("path", "/home/public/file.txt")];
        assert!(!cond.matches(&attrs_no_match));
    }

    #[test]
    fn condition_startswith() {
        let cond = Condition::new("path", CondOp::StartsWith, "/home");
        let attrs = vec![Attribute::new("path", "/home/user/file.txt")];
        assert!(cond.matches(&attrs));

        let attrs_no_match = vec![Attribute::new("path", "/tmp/file.txt")];
        assert!(!cond.matches(&attrs_no_match));
    }

    #[test]
    fn condition_in() {
        let cond = Condition::new("tool", CondOp::In, "Read,Write,Edit");
        let attrs_yes = vec![Attribute::new("tool", "Read")];
        assert!(cond.matches(&attrs_yes));

        let attrs_yes2 = vec![Attribute::new("tool", "Write")];
        assert!(cond.matches(&attrs_yes2));

        let attrs_no = vec![Attribute::new("tool", "Bash")];
        assert!(!cond.matches(&attrs_no));
    }

    #[test]
    fn condition_missing_attribute() {
        let cond = Condition::new("env", CondOp::Eq, "prod");
        let attrs = vec![Attribute::new("other", "value")];
        assert!(!cond.matches(&attrs));
    }

    #[test]
    fn rule_matches_exact_tool() {
        let rule = PolicyRule::new("r1", PermissionDecision::Allow, "Read");
        let req = PolicyRequest::new("Read");
        assert!(rule.matches(&req));

        let req_no = PolicyRequest::new("Write");
        assert!(!rule.matches(&req_no));
    }

    #[test]
    fn rule_matches_with_conditions() {
        let mut rule = PolicyRule::new("r1", PermissionDecision::Allow, "Write");
        rule = rule.with_condition(Condition::new("env", CondOp::Eq, "sandbox"));

        let req_yes = PolicyRequest::new("Write").with_attribute("env", "sandbox");
        assert!(rule.matches(&req_yes));

        let req_no = PolicyRequest::new("Write").with_attribute("env", "prod");
        assert!(!rule.matches(&req_no));
    }

    #[test]
    fn pdp_first_match_wins() {
        let mut pdp = PolicyDecisionPoint::new();
        pdp.add_rule(PolicyRule::new("r1", PermissionDecision::Deny, "Bash"));
        pdp.add_rule(PolicyRule::new("r2", PermissionDecision::Allow, "Bash"));

        let req = PolicyRequest::new("Bash");
        assert_eq!(pdp.evaluate(&req), PermissionDecision::Deny);
    }

    #[test]
    fn pdp_default_ask() {
        let pdp = PolicyDecisionPoint::new();
        let req = PolicyRequest::new("UnknownTool");
        assert_eq!(pdp.evaluate(&req), PermissionDecision::Ask);
    }

    #[test]
    fn pdp_no_matching_rule() {
        let mut pdp = PolicyDecisionPoint::new();
        pdp.add_rule(PolicyRule::new("r1", PermissionDecision::Allow, "Read"));

        let req = PolicyRequest::new("Write");
        assert_eq!(pdp.evaluate(&req), PermissionDecision::Ask);
    }

    #[test]
    fn pdp_multiple_conditions() {
        let mut rule = PolicyRule::new("r1", PermissionDecision::Allow, "Bash");
        rule = rule
            .with_condition(Condition::new("env", CondOp::Eq, "sandbox"))
            .with_condition(Condition::new("path", CondOp::StartsWith, "/tmp"));

        let req_yes = PolicyRequest::new("Bash")
            .with_attribute("env", "sandbox")
            .with_attribute("path", "/tmp/test.sh");
        assert!(rule.matches(&req_yes));

        let req_no = PolicyRequest::new("Bash")
            .with_attribute("env", "sandbox")
            .with_attribute("path", "/home/test.sh");
        assert!(!rule.matches(&req_no));
    }

    #[test]
    fn repeated_attributes_match_any_value() {
        let rule = PolicyRule::new("tagged", PermissionDecision::Allow, "Write")
            .with_condition(Condition::new("tags", CondOp::Contains, "reviewed"));
        let request = PolicyRequest::new("Write")
            .with_attribute("tags", "generated")
            .with_attribute("tags", "reviewed");
        assert!(rule.matches(&request));
    }

    #[test]
    fn starts_with_any_respects_path_component_boundary() {
        let rule = PolicyRule::new("paths", PermissionDecision::Allow, "Write").with_condition(
            Condition::new("target", CondOp::StartsWithAny, "src,docs/reference"),
        );
        assert!(rule.matches(&PolicyRequest::new("Write").with_attribute("target", "src")));
        assert!(rule.matches(&PolicyRequest::new("Write").with_attribute("target", "src/lib.rs")));
        assert!(
            !rule.matches(&PolicyRequest::new("Write").with_attribute("target", "src-old/lib.rs"))
        );
    }

    #[test]
    fn matched_exception_capable_deny_still_outranks_allow() {
        let mut pdp = PolicyDecisionPoint::new();
        pdp.add_rule(
            PolicyRule::new("deny", PermissionDecision::Deny, "Write")
                .with_condition(Condition::new("exception", CondOp::Eq, "false"))
                .with_exception_capable(true),
        );
        pdp.add_rule(PolicyRule::new("allow", PermissionDecision::Allow, "Write"));

        let normal = pdp.evaluate_with_evidence(
            &PolicyRequest::new("Write").with_attribute("exception", "false"),
        );
        assert_eq!(normal.decision, PermissionDecision::Deny);
        assert_eq!(normal.rule_id.as_deref(), Some("deny"));

        let excepted = pdp.evaluate_with_evidence(
            &PolicyRequest::new("Write").with_attribute("exception", "true"),
        );
        assert_eq!(excepted.decision, PermissionDecision::Allow);
        assert_eq!(excepted.rule_id.as_deref(), Some("allow"));
    }

    #[test]
    fn exception_capable_metadata_honors_explicit_exception_without_adapter_condition() {
        let mut pdp = PolicyDecisionPoint::new();
        pdp.add_rule(
            PolicyRule::new("deny", PermissionDecision::Deny, "Write").with_exception_capable(true),
        );
        pdp.add_rule(PolicyRule::new("allow", PermissionDecision::Allow, "Write"));

        assert_eq!(
            pdp.evaluate_with_evidence(&PolicyRequest::new("Write"))
                .decision,
            PermissionDecision::Deny
        );
        assert_eq!(
            pdp.evaluate_with_evidence(
                &PolicyRequest::new("Write").with_attribute("exception", "true")
            )
            .decision,
            PermissionDecision::Allow
        );

        let mut ask_only = PolicyDecisionPoint::new();
        ask_only.add_rule(
            PolicyRule::new("a-deny", PermissionDecision::Deny, "Write")
                .with_exception_capable(true),
        );
        ask_only.add_rule(PolicyRule::new("z-ask", PermissionDecision::Ask, "Write"));
        assert_eq!(
            ask_only
                .evaluate_with_evidence(
                    &PolicyRequest::new("Write").with_attribute("exception", "true")
                )
                .decision,
            PermissionDecision::Ask
        );
    }

    #[test]
    fn from_yaml_like_simple() {
        let yaml = r#"
rules:
  - id: allow_read
    effect: allow
    tool: Read
  - id: deny_bash
    effect: deny
    tool: Bash
"#;
        let pdp = PolicyDecisionPoint::from_yaml_like(yaml).expect("parse failed");
        assert_eq!(
            pdp.evaluate(&PolicyRequest::new("Read")),
            PermissionDecision::Allow
        );
        assert_eq!(
            pdp.evaluate(&PolicyRequest::new("Bash")),
            PermissionDecision::Deny
        );
        assert_eq!(
            pdp.evaluate(&PolicyRequest::new("Write")),
            PermissionDecision::Ask
        );
    }

    #[test]
    fn from_yaml_like_with_conditions() {
        let yaml = r#"
rules:
  - id: sandbox_bash
    effect: allow
    tool: Bash
    attribute: env
    op: eq
    value: sandbox
"#;
        let pdp = PolicyDecisionPoint::from_yaml_like(yaml).expect("parse failed");
        let req_yes = PolicyRequest::new("Bash").with_attribute("env", "sandbox");
        assert_eq!(pdp.evaluate(&req_yes), PermissionDecision::Allow);

        let req_no = PolicyRequest::new("Bash").with_attribute("env", "prod");
        assert_eq!(pdp.evaluate(&req_no), PermissionDecision::Ask);
    }

    #[test]
    fn from_yaml_like_ignores_comments() {
        let yaml = r#"
# This is a comment
rules:
  - id: rule1 # inline comments not supported but lines work
    effect: allow
    tool: Read
# Another comment
"#;
        let pdp = PolicyDecisionPoint::from_yaml_like(yaml).expect("parse failed");
        assert_eq!(
            pdp.evaluate(&PolicyRequest::new("Read")),
            PermissionDecision::Allow
        );
    }
}
