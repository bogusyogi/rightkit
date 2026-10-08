mod common;

use std::fs;

use common::{blank_event, make_writable, recorder, request, Scratch, TEST_KEY};
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

fn recorded_key_id(out: &std::path::Path) -> String {
    let scenario: Value =
        serde_json::from_str(&fs::read_to_string(out.join("scenario.json")).unwrap()).unwrap();
    assert_eq!(scenario["redaction"]["hashAlgorithm"], "hmac-sha256");
    scenario["redaction"]["keyId"].as_str().unwrap().to_string()
}

fn reseal_events(out: &std::path::Path) {
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(out.join("hashes.json")).unwrap()).unwrap();
    let (hash, bytes) = rightkit_record::hash::file_hash(&out.join("events.ndjson")).unwrap();
    manifest["files"]["events.ndjson"] = serde_json::json!({ "blake3": hash, "bytes": bytes });
    fs::write(
        out.join("hashes.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
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
        .starts_with("hmac-sha256:"));
    let key_id = recorded_key_id(&out);
    assert_eq!(
        event["target"]["label"]["saltId"],
        format!("hmac-sha256:{key_id}")
    );
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

fn label_digest(out: &std::path::Path) -> String {
    persisted_event(out)["target"]["label"]["digest"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn the_digest_is_hmac_sha256_under_the_installation_key_and_the_key_stays_out_of_the_bundle() {
    let scratch = Scratch::new("hmac");
    let out = run_with(RedactionPolicy::strict(), "hmac", &scratch);
    let message = format!("target.label\0{LABEL}");
    let expected = format!(
        "hmac-sha256:{}",
        rightkit_record::hash::hex_lower(&rightkit_record::hash::hmac_sha256(
            &TEST_KEY,
            message.as_bytes()
        ))
    );
    assert_eq!(label_digest(&out), expected);

    // The bundle records the key id and never the key.
    let key_hex = rightkit_record::hash::hex_lower(&TEST_KEY);
    for entry in fs::read_dir(&out).unwrap().flatten() {
        let bytes = fs::read(entry.path()).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(&key_hex), "{:?} holds the key", entry.path());
    }
    let report = rightkit_record::verify::bundle_verify(&out, &VerifyOptions::default()).unwrap();
    assert!(report.ok(), "{:?}", report.problems);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn equal_values_hash_equal_within_an_installation_and_differ_across_keys() {
    let scratch = Scratch::new("equality");
    let first = run_with(RedactionPolicy::strict(), "one", &scratch);
    let second = run_with(RedactionPolicy::strict(), "two", &scratch);
    assert_eq!(label_digest(&first), label_digest(&second));
    assert_eq!(recorded_key_id(&first), recorded_key_id(&second));

    let mut backend = FakeBackend::macos();
    backend.push(vec![
        HelperEvent::Ready(frame(0, 5_000, 0)),
        HelperEvent::Frame(frame(1, 20_000, 16_666_667)),
    ]);
    let other = scratch.bundle("other-key");
    let mut req = request(other.clone());
    req.key_source =
        rightkit_record::KeyHandle::new(rightkit_record::FixedKeySource::new([0x33u8; 32]));
    let mut rec = recorder(backend, T0);
    rec.record_start(req).unwrap();
    rec.append_event(telemetry_event(12_000)).unwrap();
    rec.record_stop().unwrap();
    assert_ne!(label_digest(&first), label_digest(&other));
    assert_ne!(recorded_key_id(&first), recorded_key_id(&other));
}

#[test]
fn a_take_whose_key_is_unavailable_is_refused_before_anything_is_written() {
    #[derive(Debug)]
    struct NoKey;
    impl rightkit_record::KeySource for NoKey {
        fn key(&self) -> rightkit_record::Result<rightkit_record::HashKey> {
            Err(rightkit_record::RecordError::new(
                rightkit_record::Code::PermissionDenied,
                "credential store locked",
            ))
        }
    }
    let scratch = Scratch::new("nokey");
    let out = scratch.bundle("nokey");
    let mut req = request(out.clone());
    req.key_source = rightkit_record::KeyHandle::new(NoKey);
    let mut rec = recorder(FakeBackend::macos(), T0);
    let err = rec.record_start(req).unwrap_err();
    assert_eq!(err.code, rightkit_record::Code::PermissionDenied);
    assert!(!out.exists(), "nothing may be created");

    // A policy that hashes nothing never asks for the key.
    let omit_all = RedactionPolicy::new("p", "1");
    let out2 = scratch.bundle("nokey-omit");
    let mut req = request(out2);
    req.redaction = omit_all;
    req.key_source = rightkit_record::KeyHandle::new(NoKey);
    let mut rec = recorder(FakeBackend::macos(), T0);
    rec.record_start(req).unwrap();
}

#[test]
fn verification_requires_the_recorded_key_id_to_match_the_hashes() {
    let scratch = Scratch::new("keyid");
    let out = run_with(RedactionPolicy::strict(), "keyid", &scratch);
    make_writable(&out);
    let path = out.join("scenario.json");
    let scenario = fs::read_to_string(&path).unwrap();
    let key_id = recorded_key_id(&out);
    // Removing the key id leaves keyed hashes with nothing to name their key.
    let stripped = scenario.replace(&format!("\"keyId\": \"{key_id}\""), "\"keyNote\": \"x\"");
    assert_ne!(stripped, scenario, "the key id must be present to remove");
    fs::write(&path, stripped).unwrap();
    let report = rightkit_record::verify::bundle_verify(&out, &VerifyOptions::default()).unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("records no key id")),
        "{:?}",
        report.problems
    );
}

#[test]
fn an_old_label_salted_bundle_still_reads_and_is_marked_weak() {
    let scratch = Scratch::new("legacy");
    let out = run_with(RedactionPolicy::strict(), "legacy", &scratch);
    make_writable(&out);
    // Rewrite the events as the label-salted format wrote them, and drop the key id.
    let key_id = recorded_key_id(&out);
    let events = fs::read_to_string(out.join("events.ndjson")).unwrap();
    let old = events
        .replace(&format!("hmac-sha256:{key_id}"), "label:target.label")
        .replace("\"digest\":\"hmac-sha256:", "\"digest\":\"blake3:");
    assert_ne!(old, events);
    fs::write(out.join("events.ndjson"), old).unwrap();
    reseal_events(&out);
    let scenario_path = out.join("scenario.json");
    let mut scenario: Value =
        serde_json::from_str(&fs::read_to_string(&scenario_path).unwrap()).unwrap();
    scenario["redaction"]
        .as_object_mut()
        .unwrap()
        .remove("keyId");
    scenario["redaction"]
        .as_object_mut()
        .unwrap()
        .remove("hashAlgorithm");
    fs::write(
        &scenario_path,
        serde_json::to_string_pretty(&scenario).unwrap(),
    )
    .unwrap();
    // The scenario hash in the manifest is now stale; only the redaction checks matter here.
    let report = rightkit_record::verify::bundle_verify(&out, &VerifyOptions::default()).unwrap();
    assert!(
        !report
            .problems
            .iter()
            .any(|p| p.contains("hash") && p.contains("salt")),
        "{:?}",
        report.problems
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("weak redaction")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn a_digest_prefix_that_disagrees_with_the_salt_algorithm_is_a_problem() {
    let scratch = Scratch::new("prefix-mismatch");
    let out = run_with(RedactionPolicy::strict(), "prefix-mismatch", &scratch);
    make_writable(&out);
    // The keyed bytes with the old false `blake3:` label, under an hmac-sha256 saltId.
    let events = fs::read_to_string(out.join("events.ndjson")).unwrap();
    let relabeled = events.replace("\"digest\":\"hmac-sha256:", "\"digest\":\"blake3:");
    assert_ne!(relabeled, events);
    fs::write(out.join("events.ndjson"), relabeled).unwrap();
    reseal_events(&out);
    let report = rightkit_record::verify::bundle_verify(&out, &VerifyOptions::default()).unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("digest prefix does not match")),
        "{:?}",
        report.problems
    );
}
