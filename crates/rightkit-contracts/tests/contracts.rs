//! Contract-level tests: embedded schemas compile, their own examples validate,
//! version negotiation runs before schema work, and the schema metadata is present.

#![cfg(feature = "schema")]

use rightkit_contracts::types::content_brief::{FactStatus, StageId};
use rightkit_contracts::types::{ContentBrief, DeliveryEnvelope};
use rightkit_contracts::{check_typed, parse, validate, Contract, ContractError, Version};
use serde_json::Value;

#[test]
fn every_embedded_schema_is_draft_2020_12_json_with_an_id_and_version() {
    for contract in Contract::ALL {
        let schema: Value = serde_json::from_str(contract.schema_source()).expect("schema is JSON");
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema",
            "{}",
            contract.name()
        );
        assert_eq!(
            schema["$id"].as_str(),
            Some(contract.schema_id()),
            "{}",
            contract.name()
        );
        let version = schema["version"]
            .as_str()
            .expect("schema declares a version");
        assert_eq!(
            Version::parse(version).expect("semver"),
            contract.supported_version(),
            "{}",
            contract.name()
        );
    }
}

#[test]
fn every_schema_declares_examples_and_they_validate() {
    for contract in Contract::ALL {
        if contract == Contract::ChannelMemory {
            // Its schema carries no `examples`; its fixtures under
            // contracts/fixtures/channel-memory cover validation (tests/fixtures.rs).
            continue;
        }
        let schema: Value = serde_json::from_str(contract.schema_source()).expect("schema is JSON");
        let examples = schema["examples"]
            .as_array()
            .unwrap_or_else(|| panic!("{} has no examples", contract.name()));
        assert!(
            !examples.is_empty(),
            "{} has an empty examples array",
            contract.name()
        );
        for example in examples {
            validate(contract, example)
                .unwrap_or_else(|error| panic!("{} example: {error}", contract.name()));
            let text = example.to_string();
            check_typed(contract, &text)
                .unwrap_or_else(|error| panic!("{} example typed: {error}", contract.name()));
        }
    }
}

#[test]
fn contract_names_round_trip() {
    for contract in Contract::ALL {
        assert_eq!(Contract::from_name(contract.name()), Some(contract));
    }
    assert_eq!(Contract::from_name("content_brief"), None);
}

#[test]
fn incompatible_major_is_refused_before_schema_checks() {
    // The document is badly malformed as well, so a schema-first implementation
    // would report missing fields. Version negotiation must come first.
    let document = serde_json::json!({ "contract": "record-session", "contractVersion": "2.0.0" });
    let error = validate(Contract::RecordSession, &document).unwrap_err();
    assert!(
        matches!(error, ContractError::IncompatibleMajor { .. }),
        "got {error:?}"
    );
}

#[test]
fn missing_version_is_refused_before_schema_checks() {
    let document = serde_json::json!({ "contract": "demo-plan" });
    let error = validate(Contract::DemoPlan, &document).unwrap_err();
    assert!(
        matches!(error, ContractError::MissingVersion { .. }),
        "got {error:?}"
    );
}

#[test]
fn wrong_contract_name_is_refused() {
    let document = serde_json::json!({ "contract": "record-event", "contractVersion": "1.0.0" });
    let error = validate(Contract::DemoPlan, &document).unwrap_err();
    assert!(
        matches!(error, ContractError::WrongContract { .. }),
        "got {error:?}"
    );
}

#[test]
fn schema_errors_name_the_failing_path() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/fixtures/record-event/invalid/input-missing-phase.json"
    ))
    .expect("fixture");
    let error = check_typed(Contract::RecordEvent, &text).unwrap_err();
    match error {
        ContractError::Schema { errors, .. } => assert!(
            errors.iter().any(|line| line.contains("phase")),
            "errors: {errors:?}"
        ),
        other => panic!("expected schema error, got {other:?}"),
    }
}

#[test]
fn contract_1_1_fields_reach_the_typed_views() {
    let brief: ContentBrief = parse(
        Contract::ContentBrief,
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/fixtures/content-brief/valid/fact-superseded.json"
        ))
        .expect("fixture"),
    )
    .expect("typed brief");
    assert_eq!(brief.rule_revision, Some(4));
    assert_eq!(brief.facts[0].status, FactStatus::Superseded);
    assert_eq!(
        brief.facts[0].superseded_by.as_deref(),
        Some("fact.export-price-2")
    );

    let stage: StageId = serde_json::from_str("\"labelling\"").expect("stage id");
    assert_eq!(stage, StageId::Labelling);
    assert!(serde_json::from_str::<StageId>("\"scrape\"").is_err());

    let envelope: DeliveryEnvelope = parse(
        Contract::DeliveryEnvelope,
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/fixtures/delivery-envelope/valid/rule-revision.json"
        ))
        .expect("fixture"),
    )
    .expect("typed envelope");
    assert_eq!(envelope.rule_revision, Some(4));
    assert_eq!(envelope.policy_revision, 1);
}

#[test]
fn contract_1_0_documents_have_no_rule_revision() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/fixtures/delivery-envelope/valid/text.json"
    ))
    .expect("fixture");
    let envelope: DeliveryEnvelope = parse(Contract::DeliveryEnvelope, &text).expect("typed");
    assert_eq!(envelope.rule_revision, None);
}

#[test]
fn contract_1_2_rule_revision_ref_reaches_the_typed_views() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contracts/fixtures");
    let brief: ContentBrief = parse(
        Contract::ContentBrief,
        &std::fs::read_to_string(format!("{root}/content-brief/valid/rule-revision-ref.json"))
            .expect("fixture"),
    )
    .expect("typed brief");
    assert_eq!(brief.rule_revision, Some(4));
    assert_eq!(
        brief.rule_revision_ref.as_deref(),
        Some("youtube_shorts:0123456789abcdef0123456789abcdef")
    );

    let envelope: DeliveryEnvelope = parse(
        Contract::DeliveryEnvelope,
        &std::fs::read_to_string(format!(
            "{root}/delivery-envelope/valid/rule-revision-ref.json"
        ))
        .expect("fixture"),
    )
    .expect("typed envelope");
    assert_eq!(envelope.rule_revision, Some(4));
    assert_eq!(
        envelope.rule_revision_ref.as_deref(),
        Some("x:0123456789abcdef0123456789abcdef")
    );

    // A 1.1.0 document stays valid and carries no ref.
    let old: DeliveryEnvelope = parse(
        Contract::DeliveryEnvelope,
        &std::fs::read_to_string(format!("{root}/delivery-envelope/valid/rule-revision.json"))
            .expect("fixture"),
    )
    .expect("typed envelope");
    assert_eq!(old.rule_revision_ref, None);
}

#[test]
fn channel_memory_is_registered_with_schema_and_negotiation() {
    assert_eq!(
        Contract::from_name("channel-memory"),
        Some(Contract::ChannelMemory)
    );
    assert_eq!(
        Contract::ChannelMemory.supported_version(),
        Version::new(1, 0, 0)
    );
    assert!(!Contract::ChannelMemory.has_semantic_rules());
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/fixtures/channel-memory"
    );
    let valid =
        std::fs::read_to_string(format!("{root}/valid/kids-cartoon-rev1.json")).expect("fixture");
    check_typed(Contract::ChannelMemory, &valid).expect("valid channel-memory document");
    let newer = std::fs::read_to_string(format!("{root}/version-rejected/newer-minor.json"))
        .expect("fixture");
    let error = validate(
        Contract::ChannelMemory,
        &serde_json::from_str(&newer).unwrap(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ContractError::NewerMinor { .. }),
        "got {error:?}"
    );
}
