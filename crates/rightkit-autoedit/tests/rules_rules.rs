use rightkit_autoedit::rules::{
    Condition, Context, Effect, Field, Parameter, Provenance, Rule, RuleError, RuleMatrix, Scope,
    Value,
};

fn prov(by: &str) -> Provenance {
    Provenance {
        set_by: by.to_owned(),
        set_at: "2026-10-16T09:00:00Z".to_owned(),
        note: None,
        source: None,
        edit_id: None,
    }
}

fn rule(id: &str, scope: Scope, parameter: Parameter, value: Value) -> Rule {
    Rule {
        id: id.to_owned(),
        scope,
        condition: Condition::Always,
        effect: Effect { parameter, value },
        priority: 0,
        provenance: prov("reviewer"),
    }
}

fn full_context() -> Context {
    Context {
        project: "p1".to_owned(),
        channel: Some("c1".to_owned()),
        content_type: Some("talking_head".to_owned()),
        platform: Some("shorts".to_owned()),
    }
}

fn pad(ms: i64) -> Value {
    Value::Int(ms)
}

// Precedence

#[test]
fn channel_beats_content_type_platform_and_project() {
    let matrix = RuleMatrix::from_rules(vec![
        rule(
            "project",
            Scope::Project("p1".into()),
            Parameter::PadBeforeMs,
            pad(10),
        ),
        rule(
            "platform",
            Scope::Platform("shorts".into()),
            Parameter::PadBeforeMs,
            pad(20),
        ),
        rule(
            "content",
            Scope::ContentType("talking_head".into()),
            Parameter::PadBeforeMs,
            pad(30),
        ),
        rule(
            "channel",
            Scope::Channel("c1".into()),
            Parameter::PadBeforeMs,
            pad(40),
        ),
    ])
    .unwrap();

    let res = matrix.resolve(&full_context());
    assert!(res.is_clean());
    let winner = &res.effective[&Parameter::PadBeforeMs];
    assert_eq!(winner.rule_id, "channel");
    assert_eq!(winner.value, pad(40));
    assert_eq!(winner.scope_rank, 3);

    let mut losers: Vec<&str> = res.overridden.iter().map(|o| o.rule_id.as_str()).collect();
    losers.sort();
    assert_eq!(losers, vec!["content", "platform", "project"]);
    assert!(res
        .overridden
        .iter()
        .all(|o| o.by == vec!["channel".to_owned()]));
}

#[test]
fn narrower_content_type_beats_broader_platform_when_no_channel() {
    let matrix = RuleMatrix::from_rules(vec![
        rule(
            "platform",
            Scope::Platform("shorts".into()),
            Parameter::MaxPauseMs,
            Value::Int(300),
        ),
        rule(
            "content",
            Scope::ContentType("talking_head".into()),
            Parameter::MaxPauseMs,
            Value::Int(450),
        ),
    ])
    .unwrap();

    let mut ctx = full_context();
    ctx.channel = None;
    let res = matrix.resolve(&ctx);
    assert!(res.is_clean());
    assert_eq!(res.value(Parameter::MaxPauseMs), Some(&Value::Int(450)));
}

#[test]
fn higher_priority_wins_inside_one_scope_rank() {
    let mut low = rule(
        "low",
        Scope::Channel("c1".into()),
        Parameter::Pace,
        Value::Text("slow".into()),
    );
    low.priority = 1;
    let mut high = rule(
        "high",
        Scope::Channel("c1".into()),
        Parameter::Pace,
        Value::Text("fast".into()),
    );
    high.priority = 5;

    let res = RuleMatrix::from_rules(vec![low, high])
        .unwrap()
        .resolve(&full_context());
    assert!(res.is_clean());
    assert_eq!(
        res.value(Parameter::Pace),
        Some(&Value::Text("fast".into()))
    );
    assert_eq!(res.overridden.len(), 1);
    assert_eq!(res.overridden[0].rule_id, "low");
}

#[test]
fn scope_rank_beats_priority() {
    // A broad project rule with a huge priority still loses to a narrow channel rule.
    let mut broad = rule(
        "broad",
        Scope::Project("p1".into()),
        Parameter::RemoveFillers,
        Value::Bool(false),
    );
    broad.priority = 1_000;
    let narrow = rule(
        "narrow",
        Scope::Channel("c1".into()),
        Parameter::RemoveFillers,
        Value::Bool(true),
    );

    let res = RuleMatrix::from_rules(vec![broad, narrow])
        .unwrap()
        .resolve(&full_context());
    assert_eq!(
        res.value(Parameter::RemoveFillers),
        Some(&Value::Bool(true))
    );
}

#[test]
fn resolution_does_not_depend_on_insertion_order() {
    let a = rule(
        "a",
        Scope::Project("p1".into()),
        Parameter::CaptionMaxChars,
        Value::Int(32),
    );
    let b = rule(
        "b",
        Scope::Channel("c1".into()),
        Parameter::CaptionMaxChars,
        Value::Int(24),
    );
    let forward = RuleMatrix::from_rules(vec![a.clone(), b.clone()])
        .unwrap()
        .resolve(&full_context());
    let reverse = RuleMatrix::from_rules(vec![b, a])
        .unwrap()
        .resolve(&full_context());
    assert_eq!(forward, reverse);
}

// Applicability

#[test]
fn rules_for_other_projects_channels_or_missing_fields_are_ignored() {
    let matrix = RuleMatrix::from_rules(vec![
        rule(
            "other_project",
            Scope::Project("p2".into()),
            Parameter::PadAfterMs,
            pad(5),
        ),
        rule(
            "other_channel",
            Scope::Channel("c9".into()),
            Parameter::PadAfterMs,
            pad(6),
        ),
        rule(
            "no_platform",
            Scope::Platform("reels".into()),
            Parameter::PadAfterMs,
            pad(7),
        ),
    ])
    .unwrap();

    let res = matrix.resolve(&full_context());
    assert!(res.effective.is_empty());
    assert!(res.conflicts.is_empty());
    assert!(res.overridden.is_empty());
}

#[test]
fn condition_narrows_applicability() {
    let mut gated = rule(
        "gated",
        Scope::Project("p1".into()),
        Parameter::PadBeforeMs,
        pad(15),
    );
    gated.condition = Condition::Equals {
        field: Field::Platform,
        value: "reels".into(),
    };
    let matrix = RuleMatrix::from_rules(vec![gated]).unwrap();

    assert!(matrix.resolve(&full_context()).effective.is_empty());

    let mut reels = full_context();
    reels.platform = Some("reels".into());
    assert_eq!(
        matrix.resolve(&reels).value(Parameter::PadBeforeMs),
        Some(&pad(15))
    );
}

#[test]
fn all_and_any_conditions_evaluate() {
    let ctx = full_context();
    let yes = Condition::Equals {
        field: Field::Channel,
        value: "c1".into(),
    };
    let no = Condition::Equals {
        field: Field::Channel,
        value: "c2".into(),
    };
    assert!(Condition::All {
        conditions: vec![yes.clone(), Condition::Always]
    }
    .holds(&ctx));
    assert!(!Condition::All {
        conditions: vec![yes.clone(), no.clone()]
    }
    .holds(&ctx));
    assert!(Condition::Any {
        conditions: vec![no.clone(), yes]
    }
    .holds(&ctx));
    assert!(!Condition::Any {
        conditions: vec![no]
    }
    .holds(&ctx));
    assert!(Condition::All { conditions: vec![] }.holds(&ctx));
    assert!(!Condition::Any { conditions: vec![] }.holds(&ctx));
}

// Conflicts

#[test]
fn disagreeing_top_tier_is_reported_and_has_no_effective_value() {
    let matrix = RuleMatrix::from_rules(vec![
        rule(
            "z_rule",
            Scope::Channel("c1".into()),
            Parameter::PadAfterMs,
            pad(50),
        ),
        rule(
            "a_rule",
            Scope::Channel("c1".into()),
            Parameter::PadAfterMs,
            pad(80),
        ),
        rule(
            "broad",
            Scope::Project("p1".into()),
            Parameter::PadAfterMs,
            pad(10),
        ),
    ])
    .unwrap();

    let res = matrix.resolve(&full_context());
    assert!(!res.is_clean());
    assert_eq!(res.value(Parameter::PadAfterMs), None);
    assert_eq!(res.conflicts.len(), 1);

    let c = &res.conflicts[0];
    assert_eq!(c.parameter, Parameter::PadAfterMs);
    assert_eq!(c.scope_rank, 3);
    assert_eq!(c.rule_ids, vec!["a_rule".to_owned(), "z_rule".to_owned()]);
    assert_eq!(c.values, vec![pad(80), pad(50)]);

    // The broad rule is still reported as overridden by the conflicting tier.
    assert_eq!(res.overridden.len(), 1);
    assert_eq!(res.overridden[0].rule_id, "broad");
    assert_eq!(
        res.overridden[0].by,
        vec!["a_rule".to_owned(), "z_rule".to_owned()]
    );
}

#[test]
fn agreeing_top_tier_is_not_a_conflict() {
    let matrix = RuleMatrix::from_rules(vec![
        rule(
            "first",
            Scope::Channel("c1".into()),
            Parameter::RemoveFillers,
            Value::Bool(true),
        ),
        rule(
            "second",
            Scope::Channel("c1".into()),
            Parameter::RemoveFillers,
            Value::Bool(true),
        ),
    ])
    .unwrap();

    let res = matrix.resolve(&full_context());
    assert!(res.is_clean());
    assert_eq!(
        res.value(Parameter::RemoveFillers),
        Some(&Value::Bool(true))
    );
    // Deterministic: the lowest id is the reported winner.
    assert_eq!(res.effective[&Parameter::RemoveFillers].rule_id, "first");
    assert_eq!(res.overridden.len(), 1);
    assert_eq!(res.overridden[0].rule_id, "second");
}

#[test]
fn conflict_on_one_parameter_does_not_block_another() {
    let matrix = RuleMatrix::from_rules(vec![
        rule(
            "a",
            Scope::Channel("c1".into()),
            Parameter::PadBeforeMs,
            pad(1),
        ),
        rule(
            "b",
            Scope::Channel("c1".into()),
            Parameter::PadBeforeMs,
            pad(2),
        ),
        rule(
            "c",
            Scope::Project("p1".into()),
            Parameter::Pace,
            Value::Text("normal".into()),
        ),
    ])
    .unwrap();

    let res = matrix.resolve(&full_context());
    assert_eq!(res.conflicts.len(), 1);
    assert_eq!(res.conflicts[0].parameter, Parameter::PadBeforeMs);
    assert_eq!(
        res.value(Parameter::Pace),
        Some(&Value::Text("normal".into()))
    );
}

// Validation

#[test]
fn out_of_range_integer_is_refused_with_rule_id() {
    let err = RuleMatrix::new()
        .add(rule(
            "too_wide",
            Scope::Project("p1".into()),
            Parameter::PadBeforeMs,
            pad(5_000),
        ))
        .unwrap_err();
    assert_eq!(
        err,
        RuleError::OutOfRange {
            rule: "too_wide".into(),
            parameter: Parameter::PadBeforeMs,
            min: 0,
            max: 2_000,
            got: 5_000,
        }
    );
}

#[test]
fn type_mismatch_is_refused() {
    let err = RuleMatrix::new()
        .add(rule(
            "wrong",
            Scope::Project("p1".into()),
            Parameter::RemoveFillers,
            pad(1),
        ))
        .unwrap_err();
    assert!(matches!(err, RuleError::TypeMismatch { ref rule, .. } if rule == "wrong"));
}

#[test]
fn text_outside_allowed_set_is_refused() {
    let err = RuleMatrix::new()
        .add(rule(
            "pace",
            Scope::Project("p1".into()),
            Parameter::Pace,
            Value::Text("frantic".into()),
        ))
        .unwrap_err();
    assert!(matches!(err, RuleError::NotAllowed { ref value, .. } if value == "frantic"));
}

#[test]
fn missing_provenance_empty_scope_and_empty_id_are_refused() {
    let mut no_prov = rule(
        "p",
        Scope::Project("p1".into()),
        Parameter::PadBeforeMs,
        pad(1),
    );
    no_prov.provenance.set_by = "  ".into();
    assert_eq!(
        RuleMatrix::new().add(no_prov).unwrap_err(),
        RuleError::MissingProvenance("p".into())
    );

    let empty_scope = rule(
        "s",
        Scope::Channel(String::new()),
        Parameter::PadBeforeMs,
        pad(1),
    );
    assert_eq!(
        RuleMatrix::new().add(empty_scope).unwrap_err(),
        RuleError::EmptyScope("s".into())
    );

    let empty_id = rule(
        "",
        Scope::Project("p1".into()),
        Parameter::PadBeforeMs,
        pad(1),
    );
    assert_eq!(
        RuleMatrix::new().add(empty_id).unwrap_err(),
        RuleError::EmptyId
    );
}

#[test]
fn duplicate_ids_are_refused_by_add_and_by_validate() {
    let mut matrix = RuleMatrix::new();
    matrix
        .add(rule(
            "dup",
            Scope::Project("p1".into()),
            Parameter::PadBeforeMs,
            pad(1),
        ))
        .unwrap();
    assert_eq!(
        matrix
            .add(rule(
                "dup",
                Scope::Project("p1".into()),
                Parameter::PadAfterMs,
                pad(2)
            ))
            .unwrap_err(),
        RuleError::DuplicateId("dup".into())
    );
    assert!(matrix.validate().is_ok());

    // Deserialisation does not run `add`, so validate must catch duplicates there.
    let json = r#"{"rules":[
        {"id":"x","scope":{"kind":"project","name":"p1"},"effect":{"parameter":"pad_before_ms","value":{"type":"int","value":1}},"provenance":{"set_by":"a","set_at":"t"}},
        {"id":"x","scope":{"kind":"project","name":"p1"},"effect":{"parameter":"pad_after_ms","value":{"type":"int","value":2}},"provenance":{"set_by":"a","set_at":"t"}}
    ]}"#;
    let loaded: RuleMatrix = serde_json::from_str(json).unwrap();
    assert_eq!(loaded.validate(), Err(RuleError::DuplicateId("x".into())));
}

// Serialisation

#[test]
fn matrix_round_trips_through_json_with_provenance() {
    let mut scoped = rule(
        "channel_pace",
        Scope::Channel("c1".into()),
        Parameter::Pace,
        Value::Text("fast".into()),
    );
    scoped.priority = 2;
    scoped.condition = Condition::All {
        conditions: vec![
            Condition::Equals {
                field: Field::ContentType,
                value: "talking_head".into(),
            },
            Condition::Any {
                conditions: vec![Condition::Equals {
                    field: Field::Platform,
                    value: "shorts".into(),
                }],
            },
        ],
    };
    scoped.provenance = Provenance {
        set_by: "reviewer".into(),
        set_at: "2026-10-16T09:30:00Z".into(),
        note: Some("feedback on episode 4".into()),
        source: None,
        edit_id: None,
    };
    let matrix = RuleMatrix::from_rules(vec![
        scoped,
        rule(
            "pad",
            Scope::Project("p1".into()),
            Parameter::PadBeforeMs,
            pad(120),
        ),
    ])
    .unwrap();

    let json = serde_json::to_string_pretty(&matrix).unwrap();
    let back: RuleMatrix = serde_json::from_str(&json).unwrap();
    assert_eq!(back, matrix);
    assert!(back.validate().is_ok());

    let again = serde_json::to_string_pretty(&back).unwrap();
    assert_eq!(again, json);

    let kept = back
        .rules()
        .iter()
        .find(|r| r.id == "channel_pace")
        .unwrap();
    assert_eq!(kept.provenance.set_by, "reviewer");
    assert_eq!(kept.provenance.set_at, "2026-10-16T09:30:00Z");
    assert_eq!(
        kept.provenance.note.as_deref(),
        Some("feedback on episode 4")
    );
}

#[test]
fn optional_fields_default_when_absent_from_json() {
    let json = r#"{"rules":[{
        "id":"min",
        "scope":{"kind":"platform","name":"shorts"},
        "effect":{"parameter":"remove_fillers","value":{"type":"bool","value":true}},
        "provenance":{"set_by":"reviewer","set_at":"2026-10-16T09:00:00Z"}
    }]}"#;
    let matrix: RuleMatrix = serde_json::from_str(json).unwrap();
    let r = &matrix.rules()[0];
    assert_eq!(r.condition, Condition::Always);
    assert_eq!(r.priority, 0);
    assert_eq!(r.provenance.note, None);
    assert!(matrix.validate().is_ok());
}

#[test]
fn scope_and_value_use_tagged_json() {
    let r = rule(
        "t",
        Scope::ContentType("reel".into()),
        Parameter::MaxPauseMs,
        pad(250),
    );
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["scope"]["kind"], "content_type");
    assert_eq!(v["scope"]["name"], "reel");
    assert_eq!(v["effect"]["parameter"], "max_pause_ms");
    assert_eq!(v["effect"]["value"]["type"], "int");
    assert_eq!(v["effect"]["value"]["value"], 250);
}
