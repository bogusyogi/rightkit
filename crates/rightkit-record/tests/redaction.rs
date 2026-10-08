mod common;

use std::fs;

use common::{blank_event, make_writable, recorder, request, Scratch};
use rightkit_contracts::types::record_event::{EventKind, Redacted, Target};
use rightkit_contracts::types::Rect;
use rightkit_record::fake::{frame, FakeBackend};
use rightkit_record::{FieldMode, HelperEvent, RedactionPolicy, VerifyOptions};
use serde_json::Value;

const T0: u64 = 1_000_000;
const LABEL: &str = "Export to Desktop";
const TYPED: &str = "hunter2-typed";

fn telemetry_event(host_ns: u64) -> rightkit_contracts::types::record_event::RecordEvent {
    let mut event = blank_event(EventKind::Input, host_ns);
    event.input_kind = Some(rightkit_contracts::types::record_event::InputKind::Click);
    event.phase = Some(rightkit_contracts::types::record_event::Phase::Down);
    event.effect_id = Some("effect-1".into());
    event.admitted = Some(true);
    event.step_id = Some("step-1".into());
    event.target = Some(Target {
        target_id: Some("btn-export".into()),
        identity: Some(Redacted::Full {
            value: "com.right.cutright/export".into(),
        }),
        role: "button".into(),
        label: Some(Redacted::Full {
            value: LABEL.into(),
        }),
        rect_pt: Rect {
            x: 10.0,
            y: 10.0,
            width: 80.0,
            height: 24.0,
        },
    });
    event.text = Some(Redacted::Full {
        value: TYPED.into(),
    });
    event
}

fn run_with(policy: RedactionPolicy, name: &str, scratch: &Scratch) -> std::path::PathBuf {
    let mut backend = FakeBackend::macos();
    backend.push(vec![
        HelperEvent::Ready(frame(0, 5_000, 0)),
        HelperEvent::Frame(frame(1, 20_000, 16_666_667)),
    ]);
    let out = scratch.bundle(name);
    let mut req = request(out.clone());
    req.redaction = policy;
    let mut rec = recorder(backend, T0);
    rec.record_start(req).unwrap();
    rec.append_event(telemetry_event(12_000)).unwrap();
    assert_eq!(
        rec.record_stop().unwrap().status,
        rightkit_contracts::types::record_session::BundleStatus::Complete
    );
    out
}

fn persisted_event(out: &std::path::Path) -> Value {
    let text = fs::read_to_string(out.join("events.ndjson")).unwrap();
    let line = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["kind"] == "input")
        .expect("input event persisted");
    line
}

#[test]
fn strict_policy_hashes_labels_and_omits_typed_text() {
    let scratch = Scratch::new("strict");
    let out = run_with(RedactionPolicy::strict(), "strict", &scratch);
    let events = fs::read_to_string(out.join("events.ndjson")).unwrap();
    assert!(!events.contains(LABEL), "plaintext label leaked");
    assert!(!events.contains(TYPED), "typed text leaked");
    assert!(
        !events.contains("com.right.cutright/export"),
        "identity leaked"
    );

    let event = persisted_event(&out);
    assert_eq!(event["target"]["label"]["mode"], "hash");
    assert!(event["target"]["label"]["digest"]
        .as_str()
        .unwrap()
        .starts_with("blake3:"));
    assert_eq!(event["target"]["label"]["saltId"], "label:target.label");
    assert_eq!(event["target"]["identity"]["mode"], "hash");
    assert_eq!(event["text"]["mode"], "omit");
    assert_eq!(event["redactionPolicy"]["policyId"], "redaction.rk3");

    let report = rightkit_record::verify::bundle_verify(&out, &VerifyOptions::default()).unwrap();
    assert!(report.ok(), "{:?}", report.problems);
}

#[test]
fn a_hash_field_is_never_stored_as_full() {
    let scratch = Scratch::new("hash-only");
    let out = run_with(RedactionPolicy::strict(), "hash-only", &scratch);
    let event = persisted_event(&out);
    assert_ne!(event["target"]["label"]["mode"], "full");
    assert!(event["target"]["label"].get("value").is_none());
}

#[test]
fn full_policy_keeps_the_value_and_omit_still_drops_text() {
    let scratch = Scratch::new("full");
    let policy = RedactionPolicy::new("redaction.test", "1.0.0")
        .with_field("target.label", FieldMode::Full)
        .unwrap()
        .with_field("text", FieldMode::Omit)
        .unwrap();
    let out = run_with(policy, "full", &scratch);
    let event = persisted_event(&out);
    assert_eq!(event["target"]["label"]["mode"], "full");
    assert_eq!(event["target"]["label"]["value"], LABEL);
    assert_eq!(event["text"]["mode"], "omit");
}

#[test]
fn an_omit_field_with_a_destination_fails_verification() {
    let scratch = Scratch::new("destination");
    let out = run_with(RedactionPolicy::strict(), "destination", &scratch);
    // Write a plaintext value into the omitted text field, then re-seal the manifest
    // hash for events.ndjson so only the redaction rule can catch it.
    make_writable(&out);
    let text = fs::read_to_string(out.join("events.ndjson")).unwrap();
    let tampered = text.replace(
        r#""text":{"mode":"omit"}"#,
        r#""text":{"mode":"full","value":"leaked"}"#,
    );
    assert_ne!(tampered, text, "the omit marker must be present to tamper");
    fs::write(out.join("events.ndjson"), tampered).unwrap();
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(out.join("hashes.json")).unwrap()).unwrap();
    let (hash, bytes) = rightkit_record::hash::file_hash(&out.join("events.ndjson")).unwrap();
    manifest["files"]["events.ndjson"] = serde_json::json!({ "blake3": hash, "bytes": bytes });
    fs::write(
        out.join("hashes.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = rightkit_record::verify::bundle_verify(&out, &VerifyOptions::default()).unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("omit field text has a destination")),
        "{:?}",
        report.problems
    );
}

#[test]
fn a_known_secret_must_not_appear_in_any_persisted_text() {
    let scratch = Scratch::new("secret");
    let out = run_with(RedactionPolicy::strict(), "secret", &scratch);
    // The scenario's `say` text is persisted in scenario.json.
    let clean = rightkit_record::verify::bundle_verify(
        &out,
        &VerifyOptions {
            forbidden_plaintext: vec!["not-in-this-bundle-xyz".into()],
        },
    )
    .unwrap();
    assert!(clean.ok(), "{:?}", clean.problems);
    let flagged = rightkit_record::verify::bundle_verify(
        &out,
        &VerifyOptions {
            forbidden_plaintext: vec!["save the project".into()],
        },
    )
    .unwrap();
    assert!(flagged
        .problems
        .iter()
        .any(|p| p.contains("forbidden plaintext appears in scenario.json")));
}

#[test]
fn caller_frame_events_are_refused_so_pixels_cannot_be_forged() {
    let scratch = Scratch::new("forge");
    let mut rec = recorder(FakeBackend::macos(), T0);
    rec.record_start(request(scratch.bundle("forge"))).unwrap();
    let err = rec
        .append_event(blank_event(EventKind::FrameDropped, 1))
        .unwrap_err();
    assert_eq!(err.code, rightkit_record::Code::ArgInvalid);
}
