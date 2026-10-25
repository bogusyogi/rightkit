// rightkit-contracts 0.1.7 consumed by exact registry pin (default feature "schema", the set the
// PostRight engine uses), outside the workspace.
//
// Proves, through the public API only:
// - the contract table: Contract::ALL lists the seven contracts in a fixed order; name and
//   from_name round trip; every embedded schema is Draft 2020-12 JSON whose $id equals schema_id()
//   and whose version equals supported_version(); the semantic-rule flag is false for
//   channel-memory only;
// - every schema that declares examples validates them through validate and check_typed;
// - Version::parse accepts strict major.minor.patch and refuses eleven malformed texts, and
//   Version orders and prints as expected;
// - negotiate accepts the same major with an older or equal minor, and refuses a different major
//   (IncompatibleMajor), a newer minor (NewerMinor) and malformed text (InvalidVersion);
// - validate refuses in the documented order: wrong contract name, missing contractVersion, an
//   incompatible major before any schema work, then schema errors, then semantic rules;
// - validate_str and parse return a document and a typed ContentBrief / DeliveryEnvelope, and
//   parse refuses text that is not JSON with ContractError::Parse;
// - a schema error names the failing path; a semantic rule (carousel index gap) is a separate
//   Semantic error; Display on a ContractError is non-empty and names the contract;
// - eyes_gate for every ReviewMode, the EyesGate wire names and EYES_APPROVAL_REQUIRED.
// Does not prove: the fixtures module and the rightkit-contracts command line binary (they read a
// fixture tree from disk), every field of every typed struct, or the shipped schema bytes beyond
// their $id, version and examples. Public API only.
// State: in-memory strings only. No network, no files, no process spawn, no user data.
use rightkit_contracts::review::{eyes_gate, EyesGate, EYES_APPROVAL_REQUIRED};
use rightkit_contracts::types::content_brief::ReviewMode;
use rightkit_contracts::types::{ContentBrief, DeliveryEnvelope};
use rightkit_contracts::{
    check_typed, negotiate, parse, validate, validate_str, Contract, ContractError, Version,
};
use serde_json::{json, Value};

const BRIEF: &str = r#"{
  "contract": "content-brief", "contractVersion": "1.0.0", "briefId": "brief.consumer-1", "revision": 1,
  "briefType": "talking_head", "channel": { "venture": "orthic", "channel": "legion-notes", "account": "main" },
  "platforms": ["youtube_shorts"], "deliverables": ["video"], "hook": "Stop re-explaining your repo.",
  "stages": [ { "id": "script", "requestId": "req.script.1", "inputHash": "blake3:2222222222222222222222222222222222222222222222222222222222222222", "dependsOn": [] } ]
}"#;

const ENVELOPE: &str = r#"{
  "contract": "delivery-envelope", "contractVersion": "1.0.0", "envelopeId": "env.3", "briefId": "brief.text-12",
  "briefRevision": 1, "contentRevision": 2, "policyRevision": 1,
  "producer": { "engine": "postright", "engineVersion": "0.2.0" }, "platforms": ["x"],
  "payload": { "type": "text", "body": "Shipping the export preset flow today.", "locale": "en-US" }
}"#;

const CAROUSEL_GAP: &str = r#"{
  "contract": "delivery-envelope", "contractVersion": "1.0.0", "envelopeId": "env.2", "briefId": "brief.carousel-07",
  "briefRevision": 1, "contentRevision": 1, "policyRevision": 1,
  "producer": { "engine": "genright", "engineVersion": "0.9.0" }, "platforms": ["instagram_carousel"],
  "payload": { "type": "carousel", "slides": [
    { "index": 1, "still": { "hash": "blake3:2222222222222222222222222222222222222222222222222222222222222222", "mimeType": "image/png", "width": 1080, "height": 1350, "alt": "First" } },
    { "index": 3, "still": { "hash": "blake3:5555555555555555555555555555555555555555555555555555555555555555", "mimeType": "image/png", "width": 1080, "height": 1350, "alt": "Third" } }
  ] }
}"#;

fn main() {
    contract_table();
    embedded_schema_examples();
    version_parsing_and_negotiation();
    validation_order();
    typed_documents();
    schema_and_semantic_failures();
    eyes_gate_rules();
    println!("OK rightkit-contracts 0.1.7 (registry consumer, schema)");
}

fn contract_table() {
    let names: Vec<&str> = Contract::ALL.iter().map(|c| c.name()).collect();
    assert_eq!(
        names,
        [
            "content-brief",
            "delivery-envelope",
            "record-session",
            "record-event",
            "demo-plan",
            "daemon-api",
            "channel-memory"
        ],
        "the seven contracts in their fixed order"
    );
    for contract in Contract::ALL {
        assert_eq!(Contract::from_name(contract.name()), Some(contract));
        assert_eq!(
            contract.has_semantic_rules(),
            contract != Contract::ChannelMemory,
            "{}: only channel-memory leaves semantic rules to its own crate",
            contract.name()
        );
        let schema: Value = serde_json::from_str(contract.schema_source())
            .unwrap_or_else(|error| panic!("{} schema is not JSON: {error}", contract.name()));
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema",
            "{} declares Draft 2020-12",
            contract.name()
        );
        assert_eq!(
            schema["$id"].as_str(),
            Some(contract.schema_id()),
            "{} $id",
            contract.name()
        );
        assert_eq!(
            Version::parse(schema["version"].as_str().expect("schema declares version"))
                .expect("schema version is strict semver"),
            contract.supported_version(),
            "{} schema version equals supported_version",
            contract.name()
        );
    }
    assert_eq!(Contract::from_name("content_brief"), None);
    assert_eq!(Contract::from_name(""), None);
}

fn embedded_schema_examples() {
    let mut checked = 0;
    for contract in Contract::ALL {
        let schema: Value = serde_json::from_str(contract.schema_source()).expect("schema is JSON");
        let Some(examples) = schema["examples"].as_array() else {
            assert_eq!(
                contract,
                Contract::ChannelMemory,
                "{} must declare examples",
                contract.name()
            );
            continue;
        };
        assert!(!examples.is_empty(), "{} examples", contract.name());
        for example in examples {
            validate(contract, example)
                .unwrap_or_else(|error| panic!("{} example: {error}", contract.name()));
            check_typed(contract, &example.to_string())
                .unwrap_or_else(|error| panic!("{} example typed: {error}", contract.name()));
            checked += 1;
        }
    }
    assert!(
        checked >= 6,
        "at least one example per schema that has them"
    );
}

fn version_parsing_and_negotiation() {
    assert_eq!(Version::parse("1.2.3").unwrap(), Version::new(1, 2, 3));
    assert_eq!(Version::parse("0.0.0").unwrap(), Version::new(0, 0, 0));
    assert_eq!(Version::new(1, 2, 3).to_string(), "1.2.3");
    assert!(
        Version::new(1, 2, 0) < Version::new(1, 10, 0),
        "numeric order"
    );
    for text in [
        "1.2",
        "1.2.3.4",
        "1..3",
        "v1.2.3",
        "1.02.3",
        "1.2.-3",
        "",
        "1.2.3-rc1",
        " 1.2.3",
        "1.2.x",
        "+1.2.3",
    ] {
        assert!(
            matches!(
                Version::parse(text),
                Err(ContractError::InvalidVersion { .. })
            ),
            "accepted malformed version {text:?}"
        );
    }

    assert_eq!(
        negotiate(Contract::ContentBrief, "1.0.9").unwrap(),
        Version::new(1, 0, 9),
        "patch never gates acceptance"
    );
    assert_eq!(
        negotiate(Contract::ContentBrief, "1.2.0").unwrap(),
        Version::new(1, 2, 0),
        "equal minor is accepted"
    );
    let newer = negotiate(Contract::ContentBrief, "1.3.0").unwrap_err();
    assert!(
        matches!(newer, ContractError::NewerMinor { .. }),
        "{newer:?}"
    );
    assert!(newer.is_version_refusal());
    let major = negotiate(Contract::RecordEvent, "2.0.0").unwrap_err();
    assert!(
        matches!(major, ContractError::IncompatibleMajor { .. }),
        "{major:?}"
    );
    assert!(major.is_version_refusal());
    let older_major = negotiate(Contract::RecordEvent, "0.9.0").unwrap_err();
    assert!(matches!(
        older_major,
        ContractError::IncompatibleMajor { .. }
    ));
    assert!(matches!(
        negotiate(Contract::DemoPlan, "1.1.0"),
        Err(ContractError::NewerMinor { .. })
    ));
    let invalid = negotiate(Contract::DemoPlan, "one.two.three").unwrap_err();
    assert!(matches!(invalid, ContractError::InvalidVersion { .. }));
    assert!(invalid.is_version_refusal());
}

fn validation_order() {
    // The documents below are malformed on purpose: only the earliest stage may speak.
    let wrong = validate(
        Contract::DemoPlan,
        &json!({ "contract": "record-event", "contractVersion": "1.0.0" }),
    )
    .unwrap_err();
    match &wrong {
        ContractError::WrongContract { expected, found } => {
            assert_eq!(*expected, Contract::DemoPlan);
            assert_eq!(found, "record-event");
        }
        other => panic!("expected WrongContract, got {other:?}"),
    }
    assert!(!wrong.is_version_refusal());

    let missing = validate(Contract::DemoPlan, &json!({ "contract": "demo-plan" })).unwrap_err();
    assert!(
        matches!(missing, ContractError::MissingVersion { .. }),
        "{missing:?}"
    );
    assert!(missing.is_version_refusal());

    let major = validate(
        Contract::RecordSession,
        &json!({ "contract": "record-session", "contractVersion": "2.0.0" }),
    )
    .unwrap_err();
    assert!(
        matches!(major, ContractError::IncompatibleMajor { .. }),
        "an incompatible major is refused before schema checks: {major:?}"
    );

    let schema_stage = validate(
        Contract::RecordSession,
        &json!({ "contract": "record-session", "contractVersion": "1.0.0" }),
    )
    .unwrap_err();
    assert!(
        matches!(schema_stage, ContractError::Schema { .. }),
        "a compatible but empty document reaches the schema stage: {schema_stage:?}"
    );
}

fn typed_documents() {
    let document = validate_str(Contract::ContentBrief, BRIEF).expect("brief validates");
    assert_eq!(document["briefId"], "brief.consumer-1");

    let brief: ContentBrief = parse(Contract::ContentBrief, BRIEF).expect("typed brief");
    assert_eq!(brief.brief_id, "brief.consumer-1");
    assert_eq!(brief.revision, 1);
    assert_eq!(brief.supersedes, None);
    assert_eq!(brief.platforms, ["youtube_shorts"]);
    assert_eq!(brief.hook.as_deref(), Some("Stop re-explaining your repo."));
    assert_eq!(brief.channel.venture, "orthic");
    assert_eq!(brief.stages.len(), 1);
    assert_eq!(brief.stages[0].request_id, "req.script.1");

    let text = serde_json::to_string(&brief).expect("serialise brief");
    assert!(
        !text.contains("null"),
        "absent optional fields are omitted: {text}"
    );
    let back: ContentBrief = parse(Contract::ContentBrief, &text).expect("round trip");
    assert_eq!(back, brief, "typed brief survives a JSON round trip");
    check_typed(Contract::ContentBrief, &text).expect("round-tripped brief passes check_typed");

    let envelope: DeliveryEnvelope =
        parse(Contract::DeliveryEnvelope, ENVELOPE).expect("typed envelope");
    assert_eq!(envelope.envelope_id, "env.3");
    assert_eq!(envelope.brief_revision, 1);
    assert_eq!(envelope.content_revision, 2);
    assert_eq!(envelope.platforms, ["x"]);
    let text = serde_json::to_string(&envelope).expect("serialise envelope");
    let back: DeliveryEnvelope = parse(Contract::DeliveryEnvelope, &text).expect("round trip");
    assert_eq!(back, envelope);

    match parse::<ContentBrief>(Contract::ContentBrief, "{not json") {
        Err(ContractError::Parse { message }) => assert!(!message.is_empty()),
        other => panic!("expected Parse error, got {other:?}"),
    }
    // A valid envelope offered as a brief is a wrong-contract refusal, not a parse error.
    assert!(matches!(
        parse::<ContentBrief>(Contract::ContentBrief, ENVELOPE),
        Err(ContractError::WrongContract { .. })
    ));
}

fn schema_and_semantic_failures() {
    // Schema stage: an unknown field breaks the closed schema and the error names it.
    let mut broken: Value = serde_json::from_str(BRIEF).expect("brief json");
    broken["briefType"] = json!("not_a_brief_type");
    let error = validate(Contract::ContentBrief, &broken).unwrap_err();
    match &error {
        ContractError::Schema { contract, errors } => {
            assert_eq!(*contract, Contract::ContentBrief);
            assert!(
                errors.iter().any(|line| line.contains("briefType")),
                "schema errors name the failing path: {errors:?}"
            );
        }
        other => panic!("expected Schema error, got {other:?}"),
    }
    let shown = error.to_string();
    assert!(
        shown.contains("content-brief"),
        "Display names the contract: {shown}"
    );
    assert!(
        shown.contains("briefType"),
        "Display lists the error lines: {shown}"
    );

    // Semantic stage: valid schema, but carousel slide indexes must be contiguous.
    let error = check_typed(Contract::DeliveryEnvelope, CAROUSEL_GAP).unwrap_err();
    match &error {
        ContractError::Semantic { contract, rule } => {
            assert_eq!(*contract, Contract::DeliveryEnvelope);
            assert!(!rule.is_empty());
        }
        other => panic!("expected Semantic error, got {other:?}"),
    }
    assert!(!error.is_version_refusal());
    assert!(error.to_string().contains("delivery-envelope"));

    // A bad content hash is a schema error, not a semantic one.
    let mut bad_hash: Value = serde_json::from_str(CAROUSEL_GAP).expect("envelope json");
    bad_hash["payload"]["slides"][0]["still"]["hash"] = json!("sha256:abc");
    assert!(matches!(
        validate(Contract::DeliveryEnvelope, &bad_hash),
        Err(ContractError::Schema { .. })
    ));
}

fn eyes_gate_rules() {
    assert_eq!(EYES_APPROVAL_REQUIRED, "eyes_approval_required");
    // manual: only a person approves.
    assert_eq!(
        eyes_gate(ReviewMode::Manual, true, false),
        Ok(EyesGate::Approved)
    );
    assert_eq!(
        eyes_gate(ReviewMode::Manual, false, true),
        Err(EYES_APPROVAL_REQUIRED)
    );
    assert_eq!(
        eyes_gate(ReviewMode::Manual, false, false),
        Err(EYES_APPROVAL_REQUIRED)
    );
    // auto_when_clean: a clean verdict delivers; without one it fails closed like manual.
    assert_eq!(
        eyes_gate(ReviewMode::AutoWhenClean, false, true),
        Ok(EyesGate::AutoClean)
    );
    assert_eq!(
        eyes_gate(ReviewMode::AutoWhenClean, true, false),
        Ok(EyesGate::Approved)
    );
    assert_eq!(
        eyes_gate(ReviewMode::AutoWhenClean, false, false),
        Err(EYES_APPROVAL_REQUIRED)
    );
    // none: never gated.
    assert_eq!(
        eyes_gate(ReviewMode::None, false, false),
        Ok(EyesGate::NotRequired)
    );
    // Both approvals present: a clean verdict is reported first, and a manual brief is Approved.
    assert_eq!(
        eyes_gate(ReviewMode::Manual, true, true),
        Ok(EyesGate::Approved)
    );
    assert_eq!(
        eyes_gate(ReviewMode::AutoWhenClean, true, true),
        Ok(EyesGate::AutoClean)
    );
    assert_eq!(
        eyes_gate(ReviewMode::None, true, true),
        Ok(EyesGate::NotRequired)
    );
    assert_eq!(EyesGate::Approved.name(), "approved");
    assert_eq!(EyesGate::AutoClean.name(), "auto_clean");
    assert_eq!(EyesGate::NotRequired.name(), "not_required");
}
