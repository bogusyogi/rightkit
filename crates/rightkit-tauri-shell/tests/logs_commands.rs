#![cfg(feature = "logs")]
//! The renderer log commands' core (Orthic-Labs/rightkit#390): batch checks, the cap with its
//! `logs_dropped` marker, the recording gate, synchronous error records, and the scrubbed bundle.
//! Everything runs on `LogsRuntime`, which has no Tauri types; the command wrappers only add the
//! caller check and the `unsupported` answer.

use std::time::Duration;

use rightkit_logs::LOGS_DROPPED_EVENT;
use rightkit_tauri_shell::logs_commands::{
    prepare_batch, require_registered, sanitize_strings, valid_event_name, CollectOptions,
    LogsConfig, LogsRuntime, MAX_EVENT_BYTES, SUITE_KEY_FRAGMENTS,
};
use rightkit_tauri_shell::ShellError;
use serde_json::{json, Value};

fn event(n: i64, level: &str, name: &str) -> Value {
    json!({"schema_version": 1, "ts_ms": n, "level": level, "event": name})
}

fn runtime(dir: &std::path::Path, batch_cap: usize) -> LogsRuntime {
    let mut config = LogsConfig::new("DemoApp", "1.2.3", dir.join("logs").join("events.jsonl"));
    config.batch_cap = batch_cap;
    LogsRuntime::new(config).unwrap()
}

fn lines(rt: &LogsRuntime) -> Vec<Value> {
    match std::fs::read_to_string(rt.store().path()) {
        Ok(text) => text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn names(rt: &LogsRuntime) -> Vec<String> {
    lines(rt)
        .iter()
        .map(|v| v["event"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn bad_records_are_counted_not_stored() {
    let rt_dir = tempfile::tempdir().unwrap();
    let rt = runtime(rt_dir.path(), 500);
    let huge = json!({
        "schema_version": 1, "ts_ms": 1, "level": "info", "event": "big",
        "blob": "x".repeat(MAX_EVENT_BYTES + 1)
    });
    let report = rt.write(vec![
        event(1, "info", "ok_one"),
        json!("not an object"),
        json!({"schema_version": 1, "ts_ms": 2, "level": "loud", "event": "bad_level"}),
        json!({"schema_version": 1, "ts_ms": 3, "level": "info"}),
        event(4, "info", ""),
        // Only the shell writes the marker; the renderer cannot forge one.
        event(5, "warn", LOGS_DROPPED_EVENT),
        huge,
        event(6, "info", "ok_two"),
    ]);
    assert_eq!(report.received, 8);
    assert_eq!(report.invalid, 5);
    assert_eq!(report.oversize, 1);
    assert_eq!(report.queued, 2);
    assert!(rt.flush(Duration::from_secs(5)));
    assert_eq!(names(&rt), ["ok_one", "ok_two"]);
}

#[test]
fn event_names_are_bounded_plain_text() {
    assert!(valid_event_name("app_started"));
    assert!(!valid_event_name(""));
    assert!(!valid_event_name("a\nb"));
    assert!(!valid_event_name(&"n".repeat(129)));
    assert!(!valid_event_name(LOGS_DROPPED_EVENT));
}

#[test]
fn a_batch_over_the_cap_keeps_the_newest_and_leads_with_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path(), 10);
    let batch: Vec<Value> = (0..25)
        .map(|n| event(n, "info", &format!("e{n}")))
        .collect();
    let report = rt.write(batch);
    // Ten records fit: the marker plus the newest nine; sixteen were dropped.
    assert_eq!(report.capped, 16);
    assert_eq!(report.queued, 10);
    assert!(rt.flush(Duration::from_secs(5)));
    let written = lines(&rt);
    assert_eq!(written.len(), 10);
    assert_eq!(written[0]["event"], LOGS_DROPPED_EVENT);
    assert_eq!(written[0]["level"], "warn");
    assert_eq!(written[0]["dropped"], 16);
    assert_eq!(written[1]["event"], "e16");
    assert_eq!(written[9]["event"], "e24");
}

#[test]
fn a_batch_within_the_cap_has_no_marker() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path(), 10);
    let report = rt.write(
        (0..10)
            .map(|n| event(n, "info", &format!("e{n}")))
            .collect(),
    );
    assert_eq!(report.capped, 0);
    assert!(rt.flush(Duration::from_secs(5)));
    assert!(!names(&rt).contains(&LOGS_DROPPED_EVENT.to_owned()));
    assert_eq!(names(&rt).len(), 10);
}

#[test]
fn the_recording_gate_stores_nothing_and_counts_no_drop() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path(), 500);
    assert!(rt.recording());
    rt.set_recording(false);
    let report = rt.write(vec![
        event(1, "info", "muted_info"),
        event(2, "error", "muted_error"),
    ]);
    assert_eq!(report.muted, 2);
    assert_eq!(
        report.queued + report.errors_appended + report.queue_dropped,
        0
    );
    assert_eq!(rt.writer().dropped(), 0);
    assert!(rt.flush(Duration::from_secs(5)));
    assert!(
        lines(&rt).is_empty(),
        "nothing is stored while the gate is off"
    );
    rt.set_recording(true);
    rt.write(vec![event(3, "info", "heard")]);
    assert!(rt.flush(Duration::from_secs(5)));
    assert_eq!(names(&rt), ["heard"]);
}

#[test]
fn error_records_are_appended_synchronously_and_in_arrival_order() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path(), 500);
    let report = rt.write(vec![
        event(1, "info", "before"),
        event(2, "error", "boom"),
        event(3, "info", "after"),
    ]);
    assert_eq!(report.errors_appended, 1);
    assert_eq!(report.errors_failed, 0);
    // No flush: the error line is already on disk, and the record queued before it precedes it.
    let at_return = names(&rt);
    assert!(at_return.contains(&"boom".to_owned()), "{at_return:?}");
    let before = at_return.iter().position(|n| n == "before").unwrap();
    let boom = at_return.iter().position(|n| n == "boom").unwrap();
    assert!(before < boom);
    assert!(rt.flush(Duration::from_secs(5)));
    assert_eq!(names(&rt), ["before", "boom", "after"]);
}

#[test]
fn sensitive_keys_and_text_are_redacted_before_they_are_stored() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path(), 500);
    let mut e = event(1, "info", "navigated");
    e["note"] = json!("opened https://example.test/cb?access_token=SECRETVALUE&x=1");
    e["file_path"] = json!("/Users/ada/private/plan.docx");
    e["api_key"] = json!("sk-abcdefghijklmnopqrstuvwxyz");
    e["detail"] = json!({"url": "https://example.test/a?b=c", "count": 3});
    rt.write(vec![e]);
    assert!(rt.flush(Duration::from_secs(5)));
    let stored = lines(&rt).remove(0);
    let text = stored.to_string();
    assert!(!text.contains("SECRETVALUE"), "{text}");
    assert!(!text.contains("plan.docx"), "{text}");
    assert!(!text.contains("sk-abcdef"), "{text}");
    assert!(!text.contains("b=c"), "{text}");
    assert_eq!(stored["detail"]["count"], 3);
    assert!(stored["note"].as_str().unwrap().contains("example.test"));
}

#[test]
fn the_suite_redaction_fragments_are_the_default() {
    let config = LogsConfig::new("DemoApp", "1", "events.jsonl");
    for fragment in SUITE_KEY_FRAGMENTS {
        assert!(config.redaction.additional_key_fragments.contains(fragment));
    }
}

#[test]
fn prepare_batch_checks_before_the_cap() {
    let config = LogsConfig::new("DemoApp", "1", "events.jsonl");
    let prepared = prepare_batch(
        vec![event(1, "info", "a"), json!(7), event(2, "debug", "b")],
        &config.redaction,
    );
    assert_eq!(prepared.records.len(), 2);
    assert_eq!(prepared.invalid, 1);
    assert_eq!(prepared.oversize, 0);
}

#[test]
fn sanitize_strings_reaches_every_leaf() {
    let mut v = json!({"a": ["see https://h.test/p?token=ABC123"], "n": 5, "o": {"s": "mail ada@example.com"}});
    sanitize_strings(&mut v);
    let text = v.to_string();
    assert!(!text.contains("ABC123"), "{text}");
    assert!(!text.contains("ada@example.com"), "{text}");
    assert_eq!(v["n"], 5);
}

#[test]
fn the_support_bundle_is_bounded_scrubbed_and_carries_the_native_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path(), 500);
    let mut batch: Vec<Value> = (0..20)
        .map(|n| event(n, "info", &format!("e{n}")))
        .collect();
    batch[19]["note"] = json!("see https://example.test/x?code=ZZZ999");
    rt.write(batch);
    assert!(rt.flush(Duration::from_secs(5)));

    let bundle = rt.collect(None);
    assert_eq!(bundle.metadata.app_name, "DemoApp");
    assert_eq!(bundle.metadata.app_version, "1.2.3");
    assert_eq!(bundle.sources.len(), 1);
    assert_eq!(bundle.sources[0].name, "renderer");
    assert_eq!(bundle.sources[0].events.len(), 20);
    assert!(!serde_json::to_string(&bundle).unwrap().contains("ZZZ999"));

    // The renderer may ask for fewer events, never more than the native policy.
    let few = rt.collect(Some(CollectOptions {
        max_events: Some(5),
    }));
    assert_eq!(few.sources[0].events.len(), 5);
    let many = rt.collect(Some(CollectOptions {
        max_events: Some(usize::MAX),
    }));
    assert_eq!(many.sources[0].events.len(), 20);
}

#[test]
fn collect_options_carry_no_path_or_url() {
    let ok: CollectOptions = serde_json::from_str(r#"{"maxEvents":3}"#).unwrap();
    assert_eq!(ok.max_events, Some(3));
    assert_eq!(
        serde_json::from_str::<CollectOptions>("{}").unwrap(),
        CollectOptions::default()
    );
    for hostile in [
        r#"{"path":"C:\\x"}"#,
        r#"{"url":"https://example.test"}"#,
        r#"{"maxEvents":3,"logPath":"/etc/passwd"}"#,
    ] {
        assert!(
            serde_json::from_str::<CollectOptions>(hostile).is_err(),
            "{hostile}"
        );
    }
}

#[test]
fn clear_removes_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path(), 500);
    rt.write(vec![event(1, "info", "a"), event(2, "error", "b")]);
    assert!(rt.flush(Duration::from_secs(5)));
    assert!(rt.store().path().exists());
    rt.clear().unwrap();
    assert!(!rt.store().path().exists());
    assert!(lines(&rt).is_empty());
}

#[test]
fn a_command_without_registered_state_is_unsupported() {
    assert!(require_registered(true).is_ok());
    match require_registered(false) {
        Err(ShellError::Unsupported(message)) => assert!(message.contains("not configured")),
        other => panic!("expected unsupported, got {other:?}"),
    }
}
