#![cfg(feature = "tracing")]

use rightkit_logs::{
    collect_tracing_diagnostics, tracing_layer, tracing_layer_checked, tracing_log_sources,
    BundleMetadata, CollectionPolicy, TracingFileConfig,
};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "rightkit-logs-tracing-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create tracing fixture directory");
    path
}

fn daily_names(dir: &Path, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read fixture dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&format!("{prefix}.")) && name.ends_with(".jsonl"))
        .collect();
    names.sort();
    names
}

fn read_json_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("read daily file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSON line"))
        .collect()
}

fn only_daily_file(dir: &Path, prefix: &str) -> PathBuf {
    let names = daily_names(dir, prefix);
    assert_eq!(names.len(), 1, "expected one daily file, got {names:?}");
    dir.join(&names[0])
}

#[test]
fn json_lines_put_event_fields_at_the_top_level() {
    let dir = temp_dir("json-shape");
    let config = TracingFileConfig::new(&dir, "scraperight");
    let (layer, guard) = tracing_layer::<Registry>(config);
    let subscriber = Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(detail = "ready", cold_start = true, "app started");
    });
    drop(guard);

    let lines = read_json_lines(&only_daily_file(&dir, "scraperight"));
    assert_eq!(lines.len(), 1);
    let value = &lines[0];
    assert_eq!(value["message"], "app started");
    assert_eq!(value["detail"], "ready");
    assert_eq!(value["cold_start"], true);
    assert_eq!(value["level"], "INFO");
    assert!(value["timestamp"].is_string());
    assert!(value["target"].is_string());
    assert!(
        value.get("fields").is_none(),
        "flatten_event must remove `fields`"
    );

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn flatten_event_false_nests_fields_under_fields() {
    let dir = temp_dir("json-nested");
    let config = TracingFileConfig {
        flatten_event: false,
        ..TracingFileConfig::new(&dir, "scraperight")
    };
    let (layer, guard) = tracing_layer::<Registry>(config);
    let subscriber = Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(detail = "ready", "nested");
    });
    drop(guard);

    let lines = read_json_lines(&only_daily_file(&dir, "scraperight"));
    assert_eq!(lines[0]["fields"]["detail"], "ready");
    assert!(lines[0].get("detail").is_none());

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn guard_drop_flushes_every_accepted_line() {
    let dir = temp_dir("guard-flush");
    let config = TracingFileConfig::new(&dir, "scraperight");
    let (layer, guard) = tracing_layer::<Registry>(config);
    let subscriber = Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        for sequence in 0..200 {
            tracing::info!(sequence, "queued");
        }
    });
    assert_eq!(
        guard.dropped_events(),
        0,
        "queue capacity must hold the burst"
    );
    drop(guard);

    let lines = read_json_lines(&only_daily_file(&dir, "scraperight"));
    assert_eq!(lines.len(), 200);
    assert_eq!(lines[199]["sequence"], 199);

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn env_filter_directive_limits_the_file_layer() {
    let dir = temp_dir("filter");
    let config = TracingFileConfig {
        env_filter: Some("warn".to_string()),
        ..TracingFileConfig::new(&dir, "scraperight")
    };
    let (layer, guard) = tracing_layer::<Registry>(config);
    let subscriber = Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(detail = "quiet", "info line");
        tracing::warn!(detail = "loud", "warn line");
    });
    drop(guard);

    let lines = read_json_lines(&only_daily_file(&dir, "scraperight"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["detail"], "loud");

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn layer_creation_prunes_to_the_newest_max_files_and_leaves_other_files() {
    let dir = temp_dir("retention");
    for day in [
        "2020-01-01",
        "2020-01-02",
        "2020-01-03",
        "2020-01-04",
        "2020-01-05",
    ] {
        fs::write(dir.join(format!("scraperight.{day}.jsonl")), b"{}\n").expect("seed");
    }
    fs::write(dir.join("scraperight-engine.2020-01-01.jsonl"), b"{}\n").expect("seed engine");
    fs::write(dir.join("scraperight.current.jsonl"), b"{}\n").expect("seed current");

    let config = TracingFileConfig::new(&dir, "scraperight");
    let (layer, guard) = tracing_layer::<Registry>(config);
    drop(layer);
    drop(guard);

    let daily: Vec<String> = daily_names(&dir, "scraperight")
        .into_iter()
        .filter(|name| name.len() == "scraperight.2020-01-01.jsonl".len())
        .collect();
    // Today's file is created at layer construction, so the newest two seeded days survive.
    assert_eq!(daily.len(), 3, "{daily:?}");
    assert!(daily.contains(&"scraperight.2020-01-04.jsonl".to_string()));
    assert!(daily.contains(&"scraperight.2020-01-05.jsonl".to_string()));
    assert!(dir.join("scraperight-engine.2020-01-01.jsonl").exists());
    assert!(dir.join("scraperight.current.jsonl").exists());

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn collect_reads_the_same_daily_files_the_writer_keeps() {
    let dir = temp_dir("collect");
    fs::write(
        dir.join("scraperight.2020-01-01.jsonl"),
        "{\"ts_ms\":1000,\"level\":\"info\",\"event\":\"old_event\",\"detail\":\"from-old-file\"}\n",
    )
    .expect("seed old daily file");
    fs::write(
        dir.join("scraperight-engine.2020-01-01.jsonl"),
        "{\"ts_ms\":1,\"level\":\"info\",\"event\":\"other_family\",\"detail\":\"must-not-appear\"}\n",
    )
    .expect("seed other family");

    let config = TracingFileConfig::new(&dir, "scraperight");
    let (layer, guard) = tracing_layer::<Registry>(config.clone());
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(detail = "from-new-file", "new line");
    });
    drop(guard);

    let bundle =
        collect_tracing_diagnostics(&config, "app", metadata(), &CollectionPolicy::default());
    assert_eq!(bundle.sources.len(), 2, "{:?}", bundle.sources);
    assert_eq!(bundle.sources[0].name, "app-0");
    assert_eq!(bundle.sources[0].file_name, "scraperight.2020-01-01.jsonl");
    assert_eq!(bundle.sources[0].events[0]["detail"], "from-old-file");
    assert_eq!(bundle.sources[1].name, "app-1");
    assert_eq!(bundle.sources[1].events[0]["detail"], "from-new-file");
    let serialized = serde_json::to_string(&bundle).expect("serialize bundle");
    assert!(!serialized.contains("must-not-appear"));

    let newest_only = TracingFileConfig {
        max_files: 1,
        ..config
    };
    let bundle = collect_tracing_diagnostics(
        &newest_only,
        "app",
        metadata(),
        &CollectionPolicy::default(),
    );
    assert_eq!(bundle.sources.len(), 1);
    assert_eq!(bundle.sources[0].events[0]["detail"], "from-new-file");

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn collect_without_daily_files_reports_the_source_as_missing() {
    let dir = temp_dir("collect-missing");
    let config = TracingFileConfig::new(dir.join("absent"), "scraperight");

    let sources = tracing_log_sources(&config, "app");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "app");

    let bundle =
        collect_tracing_diagnostics(&config, "app", metadata(), &CollectionPolicy::default());
    assert_eq!(
        bundle.sources[0].warnings,
        vec!["current log missing".to_string()]
    );

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn checked_layer_returns_an_error_for_an_unusable_directory() {
    let base = temp_dir("checked-bad");
    let blocker = base.join("not-a-dir");
    fs::write(&blocker, b"file in the way").expect("write blocker file");
    let config = TracingFileConfig::new(blocker.join("logs"), "scraperight");
    assert!(tracing_layer_checked::<Registry>(config).is_err());
    fs::remove_dir_all(base).expect("remove tracing fixture");
}

#[test]
fn checked_layer_writes_like_tracing_layer_on_a_usable_directory() {
    let dir = temp_dir("checked-ok");
    let config = TracingFileConfig::new(&dir, "scraperight");
    let (layer, guard) = tracing_layer_checked::<Registry>(config).expect("checked layer opens");
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(detail = "checked", "started");
    });
    drop(guard);
    let lines = read_json_lines(&only_daily_file(&dir, "scraperight"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["detail"], "checked");
    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn default_queue_is_128k_lines_and_every_event_is_written_or_counted() {
    assert_eq!(TracingFileConfig::new("unused", "p").queue_lines, 128_000);

    let dir = temp_dir("queue-bound");
    let config = TracingFileConfig {
        queue_lines: 1,
        ..TracingFileConfig::new(&dir, "scraperight")
    };
    let (layer, guard) = tracing_layer::<Registry>(config);
    let subscriber = Registry::default().with(layer);
    const EVENTS: u64 = 500;
    tracing::subscriber::with_default(subscriber, || {
        for sequence in 0..EVENTS {
            tracing::info!(sequence, "burst");
        }
    });
    let dropped = guard.dropped_events();
    drop(guard);
    let written = read_json_lines(&only_daily_file(&dir, "scraperight")).len() as u64;
    assert_eq!(
        written + dropped,
        EVENTS,
        "each event is written or counted as dropped"
    );
    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

#[test]
fn guard_clear_truncates_the_active_file_and_removes_only_its_own_family() {
    let dir = temp_dir("clear-barrier");
    fs::write(dir.join("scraperight.2020-01-01.jsonl"), b"{}\n").expect("seed old daily");
    fs::write(dir.join("scraperight-engine.2020-01-01.jsonl"), b"{}\n").expect("seed other family");
    fs::write(dir.join("notes.txt"), b"keep\n").expect("seed unrelated");

    let config = TracingFileConfig::new(&dir, "scraperight");
    let (layer, guard) = tracing_layer::<Registry>(config);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(detail = "before-1", "first");
        tracing::info!(detail = "before-2", "second");
        guard.clear().expect("writer-owned clear acknowledges");
        tracing::info!(detail = "after-1", "third");
    });
    drop(guard);

    let lines = read_json_lines(&only_daily_file(&dir, "scraperight"));
    assert_eq!(
        lines.len(),
        1,
        "only the post-clear line survives: {lines:?}"
    );
    assert_eq!(
        lines[0]["detail"], "after-1",
        "write after clear is visible"
    );
    assert!(
        !dir.join("scraperight.2020-01-01.jsonl").exists(),
        "old family member removed"
    );
    assert!(
        dir.join("scraperight-engine.2020-01-01.jsonl").exists(),
        "other family retained"
    );
    assert!(dir.join("notes.txt").exists(), "unrelated file retained");

    fs::remove_dir_all(dir).expect("remove tracing fixture");
}

fn metadata() -> BundleMetadata {
    BundleMetadata {
        app_name: "ScrapeRight".to_string(),
        app_version: "0.0.0".to_string(),
        build: "test".to_string(),
        os: "test".to_string(),
        settings: serde_json::json!({}),
    }
}
