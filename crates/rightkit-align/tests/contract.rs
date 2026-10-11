//! The `rightkit.alignment` v1 wire contract: round trip, schema identity, and
//! agreement between the serialised shape and `contracts/alignment.v1.schema.json`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use rightkit_align::{
    align, asr_words_from_seconds, AlignOptions, Alignment, AlignmentContract, DesignatedAnchor,
    ScriptSpec, CONTRACT, CONTRACT_VERSION,
};
use serde_json::Value;

/// "Click Export, open PostgreSQL now." with a misheard product name, so the
/// document has an accepted token, a rejected token, a clause break and a
/// coverage failure.
fn sample_alignment() -> Alignment {
    let asr = asr_words_from_seconds([
        ("click", 0.40f32, 0.72f32),
        ("export", 0.76, 1.18),
        ("open", 1.30, 1.56),
        ("postgres", 1.64, 2.40),
        ("now", 2.48, 2.80),
    ]);
    align(
        &ScriptSpec::from_text("step-1", "Click Export, open PostgreSQL now."),
        &asr,
        16_000,
        &[
            DesignatedAnchor {
                word_id: "w1".into(),
                target_id: "t-export".into(),
            },
            DesignatedAnchor {
                word_id: "w3".into(),
                target_id: "t-pg".into(),
            },
        ],
        &AlignOptions::default(),
    )
}

fn document() -> Value {
    serde_json::to_value(sample_alignment().to_contract()).expect("serialises")
}

fn key_set(value: &Value) -> BTreeSet<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

fn schema_set(schema: &Value, field: &str) -> BTreeSet<String> {
    schema[field]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|v| v.as_str().expect("string").to_string())
                .collect()
        })
        .unwrap_or_else(|| key_set(&schema[field]))
}

/// The instance has exactly the keys the schema requires and declares.
fn assert_matches(name: &str, schema: &Value, instance: &Value) {
    let instance_keys = key_set(instance);
    assert_eq!(
        schema_set(schema, "required"),
        instance_keys,
        "{name}: required keys differ from the serialised keys"
    );
    assert_eq!(
        schema_set(schema, "properties"),
        instance_keys,
        "{name}: declared properties differ from the serialised keys"
    );
}

fn schema() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/alignment.v1.schema.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("schema is JSON")
}

#[test]
fn the_document_round_trips_through_json_unchanged() {
    let contract = sample_alignment().to_contract();
    let json = serde_json::to_string(&contract).expect("serialises");
    let back: AlignmentContract = serde_json::from_str(&json).expect("parses");
    assert_eq!(back, contract);
}

#[test]
fn the_document_has_the_v1_shape_the_spec_names() {
    let value = document();
    assert_eq!(value["contract"], "rightkit.alignment");
    assert_eq!(value["contractVersion"], "1.0.0");
    assert_eq!(CONTRACT, value["contract"]);
    assert_eq!(CONTRACT_VERSION, value["contractVersion"]);
    assert_eq!(value["route"], "local");
    assert_eq!(value["timing_tolerance_ms"], 50);

    let accepted = &value["tokens"][0];
    assert_eq!(accepted["word_id"], "w0");
    assert_eq!(accepted["step_id"], "step-1");
    assert_eq!(accepted["target_id"], Value::Null);
    assert_eq!(accepted["outcome"]["status"], "accepted");
    assert_eq!(accepted["outcome"]["sample_rate"], 16_000);
    assert_eq!(accepted["outcome"]["start_sample"], 6_400);
    assert!(accepted["timing_confidence"].as_f64().unwrap() > 0.6);

    let designated = &value["tokens"][1];
    assert_eq!(designated["target_id"], "t-export");

    let rejected = &value["tokens"][3];
    assert_eq!(rejected["outcome"]["status"], "rejected");
    assert_eq!(rejected["outcome"]["reason"], "word_differs");
    assert_eq!(rejected["asr_text"], "postgres");

    assert_eq!(value["clauses"][0]["clause_id"], "c0");
    assert_eq!(value["clauses"][0]["last_word_id"], "w1");
    assert_eq!(value["clauses"][0]["end"]["sample_rate"], 16_000);

    let failure = &value["coverage"]["failures"][0];
    assert_eq!(failure["word_id"], "w3");
    assert_eq!(failure["target_id"], "t-pg");
    assert_eq!(failure["reason"], "word_differs");
}

#[test]
fn a_document_with_a_foreign_contract_or_version_is_refused_on_read() {
    let mut foreign = document();
    foreign["contract"] = Value::from("rightkit.transcript");
    let error = serde_json::from_value::<AlignmentContract>(foreign).unwrap_err();
    assert!(error.to_string().contains("rightkit.alignment"), "{error}");

    for version in ["2.0.0", "1.1.0", "1.0", "1.0.0-rc"] {
        let mut newer = document();
        newer["contractVersion"] = Value::from(version);
        assert!(
            serde_json::from_value::<AlignmentContract>(newer).is_err(),
            "contractVersion {version} must be refused"
        );
    }
}

#[test]
fn the_schema_file_names_this_version_and_matches_the_serialised_shape() {
    let schema = schema();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["$id"], "urn:rightkit:contract:alignment:v1");
    assert_eq!(schema["properties"]["contract"]["const"], CONTRACT);
    assert_eq!(
        schema["properties"]["contractVersion"]["$ref"],
        "#/$defs/contractVersion"
    );
    assert!(schema["$defs"]["contractVersion"]["pattern"]
        .as_str()
        .unwrap()
        .contains("1"));
    assert_eq!(schema["properties"]["route"]["$ref"], "#/$defs/route");
    assert_eq!(
        schema["$defs"]["route"]["enum"],
        serde_json::json!(["local"])
    );

    let value = document();
    let defs = &schema["$defs"];
    assert_matches("document", &schema, &value);
    assert_matches("token", &defs["token"], &value["tokens"][0]);
    assert_matches(
        "outcome accepted",
        &defs["outcome"]["oneOf"][0],
        &value["tokens"][0]["outcome"],
    );
    assert_matches(
        "outcome rejected",
        &defs["outcome"]["oneOf"][1],
        &value["tokens"][3]["outcome"],
    );
    assert_matches("clause", &defs["clause"], &value["clauses"][0]);
    assert_matches("span", &defs["span"], &value["clauses"][0]["start"]);
    assert_matches("coverage", &defs["coverage"], &value["coverage"]);
    assert_matches(
        "coverage failure",
        &defs["coverageFailure"],
        &value["coverage"]["failures"][0],
    );

    let reasons: BTreeSet<&str> = defs["rejectReason"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        BTreeSet::from(["no_spoken_form", "unaligned", "ambiguous", "word_differs"])
    );
}
