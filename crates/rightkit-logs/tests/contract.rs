use rightkit_logs::{
    collect_diagnostics, prepare_diagnostic_events, BundleMetadata, CollectionPolicy, EventRecord,
    JsonlStore, Level, LogSource,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "rightkit-logs-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create telemetry fixture directory");
    path
}

#[test]
fn event_record_serializes_stable_flattened_json() {
    let event = EventRecord::new(1_725_000_000_000, Level::Info, "app_started")
        .with_field("message", json!("ready"))
        .with_field("cold_start", json!(true));

    let value = serde_json::to_value(event).expect("serialize event");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ts_ms"], 1_725_000_000_000_i64);
    assert_eq!(value["level"], "info");
    assert_eq!(value["event"], "app_started");
    assert_eq!(value["message"], "ready");
    assert_eq!(value["cold_start"], true);
    assert!(value.get("fields").is_none(), "fields must be flattened");
}

#[test]
fn event_fields_cannot_override_envelope_keys() {
    let event = EventRecord::new(42, Level::Info, "trusted_event")
        .with_field("schema_version", json!(999))
        .with_field("ts_ms", json!(0))
        .with_field("level", json!("error"))
        .with_field("event", json!("forged_event"));

    let value = serde_json::to_value(event).expect("serialize event");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ts_ms"], 42);
    assert_eq!(value["level"], "info");
    assert_eq!(value["event"], "trusted_event");
}

#[test]
fn concurrent_appends_produce_only_complete_json_lines() {
    let dir = temp_dir("concurrent");
    let path = dir.join("events.jsonl");
    let store = Arc::new(JsonlStore::new(&path, 1024 * 1024));
    let mut workers = Vec::new();

    for worker in 0..8 {
        let store = Arc::clone(&store);
        workers.push(thread::spawn(move || {
            for sequence in 0..25 {
                let event = EventRecord::new(sequence, Level::Info, "worker_event")
                    .with_field("worker", json!(worker))
                    .with_field("sequence", json!(sequence));
                store.append(&event).expect("append event");
            }
        }));
    }
    for worker in workers {
        worker.join().expect("worker must not panic");
    }

    let text = fs::read_to_string(&path).expect("read ledger");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 200);
    for line in lines {
        let value: serde_json::Value = serde_json::from_str(line).expect("valid JSONL row");
        assert_eq!(value["event"], "worker_event");
    }

    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

#[test]
fn append_rotates_to_one_predecessor_at_size_limit() {
    let dir = temp_dir("rotation");
    let path = dir.join("events.jsonl");
    fs::write(&path, vec![b'x'; 128]).expect("seed full current log");
    let store = JsonlStore::new(&path, 128);

    store
        .append(&EventRecord::new(7, Level::Warn, "after_rotation"))
        .expect("append after rotation");

    let predecessor = dir.join("events.jsonl.1");
    assert_eq!(fs::metadata(&predecessor).expect("predecessor").len(), 128);
    let current = fs::read_to_string(&path).expect("read new current log");
    let value: serde_json::Value = serde_json::from_str(current.trim()).expect("current JSON");
    assert_eq!(value["event"], "after_rotation");
    assert_eq!(value["level"], "warn");

    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

#[test]
fn best_effort_failure_never_panics_and_clear_removes_both_files() {
    let dir = temp_dir("failure-clear");
    let blocker = dir.join("not-a-directory");
    fs::write(&blocker, b"file").expect("seed path blocker");
    let bad_store = JsonlStore::new(blocker.join("events.jsonl"), 128);
    bad_store.append_best_effort(&EventRecord::new(1, Level::Error, "write_failed"));

    let path = dir.join("events.jsonl");
    let predecessor = dir.join("events.jsonl.1");
    fs::write(&path, b"current\n").expect("seed current");
    fs::write(&predecessor, b"previous\n").expect("seed predecessor");
    let store = JsonlStore::new(&path, 128);
    store.clear().expect("clear log pair");

    assert!(!path.exists());
    assert!(!predecessor.exists());
    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

fn write_jsonl(path: &std::path::Path, values: &[serde_json::Value]) {
    let mut text = values
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    fs::write(path, text).expect("write JSONL fixture");
}

fn metadata() -> BundleMetadata {
    BundleMetadata {
        app_name: "FixtureRight".to_string(),
        app_version: "1.2.3".to_string(),
        build: "release".to_string(),
        os: "windows".to_string(),
        settings: json!({ "privacy_mode": false }),
    }
}

#[test]
fn collection_keeps_recent_window_from_most_recent_error() {
    let dir = temp_dir("error-anchor");
    let path = dir.join("events.jsonl");
    let mut events = Vec::new();
    for index in 0..10 {
        events.push(json!({
            "schema_version": 1,
            "ts_ms": index,
            "level": if index == 5 || index == 8 { "error" } else { "info" },
            "event": format!("event_{index}")
        }));
    }
    write_jsonl(&path, &events);
    let policy = CollectionPolicy {
        max_events: 5,
        ..CollectionPolicy::default()
    };

    let bundle = collect_diagnostics(&[LogSource::new("app", &path)], metadata(), &policy);

    let source = &bundle.sources[0];
    let names: Vec<&str> = source
        .events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert_eq!(names, vec!["event_8", "event_9"]);
    assert!(source.stats.error_anchor_found);
    assert_eq!(source.stats.parsed, 10);
    assert_eq!(source.stats.dropped, 8);

    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

#[test]
fn collection_reads_predecessor_and_recognizes_legacy_failure_names() {
    let dir = temp_dir("rotated-legacy");
    let path = dir.join("events.jsonl");
    let predecessor = dir.join("events.jsonl.1");
    write_jsonl(
        &predecessor,
        &[
            json!({ "ts_ms": 1, "event": "before" }),
            json!({ "ts_ms": 2, "event": "engine_failed" }),
        ],
    );
    write_jsonl(
        &path,
        &[
            json!({ "ts_ms": 3, "event": "recovering" }),
            json!({ "ts_ms": 4, "event": "ready" }),
        ],
    );

    let bundle = collect_diagnostics(
        &[LogSource::new("app", &path)],
        metadata(),
        &CollectionPolicy::default(),
    );

    let source = &bundle.sources[0];
    let names: Vec<&str> = source
        .events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert_eq!(names, vec!["engine_failed", "recovering", "ready"]);
    assert!(source.stats.rotated_read);
    assert!(source.stats.error_anchor_found);

    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

#[test]
fn collection_applies_event_limit_and_error_anchor_across_all_sources() {
    let dir = temp_dir("global-window");
    let app_path = dir.join("events.jsonl");
    let engine_path = dir.join("engine-events.jsonl");
    write_jsonl(
        &app_path,
        &[
            json!({ "ts_ms": 1, "level": "info", "event": "app_1" }),
            json!({ "ts_ms": 8, "level": "error", "event": "app_error" }),
            json!({ "ts_ms": 10, "level": "info", "event": "app_10" }),
        ],
    );
    write_jsonl(
        &engine_path,
        &[
            json!({ "ts_ms": 2, "level": "info", "event": "engine_2" }),
            json!({ "ts_ms": 3, "level": "info", "event": "engine_3" }),
            json!({ "ts_ms": 4, "level": "info", "event": "engine_4" }),
            json!({ "ts_ms": 5, "level": "info", "event": "engine_5" }),
            json!({ "ts_ms": 6, "level": "info", "event": "engine_6" }),
            json!({ "ts_ms": 7, "level": "info", "event": "engine_7" }),
            json!({ "ts_ms": 9, "level": "info", "event": "engine_9" }),
        ],
    );
    let policy = CollectionPolicy {
        max_events: 5,
        ..CollectionPolicy::default()
    };

    let bundle = collect_diagnostics(
        &[
            LogSource::new("app", &app_path),
            LogSource::new("engine", &engine_path),
        ],
        metadata(),
        &policy,
    );

    let mut retained: Vec<(i64, &str)> = bundle
        .sources
        .iter()
        .flat_map(|source| &source.events)
        .map(|event| {
            (
                event["ts_ms"].as_i64().expect("timestamp"),
                event["event"].as_str().expect("event name"),
            )
        })
        .collect();
    retained.sort_by_key(|(timestamp, _)| *timestamp);
    assert_eq!(
        retained,
        vec![(8, "app_error"), (9, "engine_9"), (10, "app_10")]
    );
    assert!(bundle.sources[0].stats.error_anchor_found);
    assert_eq!(
        bundle
            .sources
            .iter()
            .map(|source| source.events.len())
            .sum::<usize>(),
        3
    );

    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

#[test]
fn collection_tolerates_corruption_partial_tail_and_missing_sources() {
    let dir = temp_dir("corruption");
    let path = dir.join("events.jsonl");
    let mut bytes = vec![b'x'; 512];
    bytes.extend_from_slice(b"\nnot-json\n[]\n");
    bytes.extend_from_slice(br#"{"ts_ms":9,"level":"info","event":"valid_after_corruption"}"#);
    bytes.push(b'\n');
    fs::write(&path, bytes).expect("write corrupt fixture");
    let policy = CollectionPolicy {
        max_scan_bytes_per_file: 128,
        ..CollectionPolicy::default()
    };
    let missing = dir.join("missing.jsonl");

    let bundle = collect_diagnostics(
        &[
            LogSource::new("app", &path),
            LogSource::new("missing", &missing),
        ],
        metadata(),
        &policy,
    );

    assert_eq!(bundle.sources.len(), 2);
    assert_eq!(bundle.sources[0].events.len(), 1);
    assert_eq!(
        bundle.sources[0].events[0]["event"],
        "valid_after_corruption"
    );
    assert!(bundle.sources[0].stats.malformed >= 2);
    assert!(!bundle.sources[1].warnings.is_empty());
    assert!(bundle.sources[1].events.is_empty());

    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

#[test]
fn collection_recursively_redacts_defaults_and_enforces_bundle_bytes() {
    let dir = temp_dir("redaction-bound");
    let path = dir.join("events.jsonl");
    write_jsonl(
        &path,
        &[
            json!({
                "ts_ms": 1,
                "level": "info",
                "event": "sensitive",
                "message": "private message",
                "transcript": "private transcript",
                "nested": {
                    "api_token": "private token",
                    "custom_private": "private custom",
                    "safe_code": "E42"
                },
                "detail": "z".repeat(2_000)
            }),
            json!({
                "ts_ms": 2,
                "level": "info",
                "event": "small",
                "message": "another private message",
                "safe_code": "E43"
            }),
        ],
    );
    let mut metadata = metadata();
    metadata.settings = json!({
        "privacy_mode": false,
        "password": "private password",
        "custom_private": "private setting",
        "safe_toggle": true
    });
    let mut policy = CollectionPolicy {
        max_bundle_bytes: 900,
        ..CollectionPolicy::default()
    };
    policy
        .redaction
        .additional_exact_keys
        .insert("custom_private".to_string());

    let bundle = collect_diagnostics(&[LogSource::new("app", &path)], metadata, &policy);
    let serialized = serde_json::to_string(&bundle).expect("serialize diagnostics bundle");

    for forbidden in [
        "private message",
        "private transcript",
        "private token",
        "private custom",
        "private password",
        "private setting",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }
    assert!(serialized.contains("[redacted:diagnostics]"));
    assert!(serialized.contains("safe_toggle"));
    assert!(serialized.len() <= policy.max_bundle_bytes);
    assert!(bundle.sources[0].stats.dropped >= 1);

    fs::remove_dir_all(dir).expect("remove telemetry fixture");
}

#[test]
fn in_memory_events_use_the_same_recent_error_and_redaction_policy() {
    let events = vec![
        json!({"kind": "started", "message": "private before"}),
        json!({"kind": "provider_failed", "message": "private failure"}),
        json!({"kind": "recovered", "message": "private recovery"}),
    ];

    let prepared = prepare_diagnostic_events(events, &CollectionPolicy::default());

    assert_eq!(prepared.events.len(), 2);
    assert_eq!(prepared.events[0]["kind"], "provider_failed");
    assert_eq!(prepared.events[1]["kind"], "recovered");
    assert_eq!(prepared.events[0]["message"], "[redacted:diagnostics]");
    assert!(prepared.stats.error_anchor_found);
    assert_eq!(prepared.stats.dropped, 1);
}

#[test]
fn tracing_rfc3339_timestamps_order_sources_and_anchor_globally() {
    let dir = temp_dir("tracing-timestamps");
    let shell = dir.join("shell.jsonl");
    let engine = dir.join("engine.jsonl");
    write_jsonl(
        &shell,
        &[
            json!({"timestamp":"2026-07-12T10:00:00.000Z","level":"info","target":"shell"}),
            json!({"timestamp":"2026-07-12T10:00:03.000Z","level":"info","target":"shell"}),
        ],
    );
    write_jsonl(
        &engine,
        &[
            json!({"timestamp":"2026-07-12T10:00:01.000Z","level":"error","target":"engine"}),
            json!({"timestamp":"2026-07-12T10:00:02.000Z","level":"info","target":"engine"}),
        ],
    );

    let bundle = collect_diagnostics(
        &[
            LogSource::new("shell", &shell),
            LogSource::new("engine", &engine),
        ],
        metadata(),
        &CollectionPolicy::default(),
    );

    assert_eq!(bundle.sources[0].events.len(), 1);
    assert_eq!(bundle.sources[0].events[0]["target"], "shell");
    assert_eq!(bundle.sources[1].events.len(), 2);
    assert!(bundle.sources[1].stats.error_anchor_found);

    fs::remove_dir_all(dir).expect("remove logging fixture");
}
