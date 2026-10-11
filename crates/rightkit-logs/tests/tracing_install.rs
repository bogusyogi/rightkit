#![cfg(feature = "tracing")]

use rightkit_logs::{install_tracing, TracingFileConfig};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "rightkit-logs-install-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create install fixture directory");
    path
}

/// One test per binary: `install_tracing` owns the process-wide subscriber.
#[test]
fn install_writes_daily_file_flushes_on_drop_and_rejects_reinstall() {
    let dir = temp_dir("global");

    // Validation runs before any subscriber is installed, so these failures leave the
    // process free for the real install below.
    let bad_prefix = install_tracing(TracingFileConfig::new(&dir, "bad/prefix"));
    assert!(bad_prefix.is_err(), "path separators must be rejected");
    let bad_filter = install_tracing(TracingFileConfig {
        env_filter: Some("info=[".to_string()),
        ..TracingFileConfig::new(&dir, "scraperight")
    });
    assert!(bad_filter.is_err(), "an invalid directive must be rejected");

    let guard = install_tracing(TracingFileConfig::new(&dir, "scraperight"))
        .expect("install global tracing");
    tracing::info!(detail = "installed", "install check");

    let reinstall = install_tracing(TracingFileConfig::new(&dir, "scraperight"));
    assert!(
        reinstall.is_err(),
        "a second global subscriber must be refused"
    );

    drop(guard);

    let names: Vec<String> = fs::read_dir(&dir)
        .expect("read fixture dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    let name = &names[0];
    assert!(
        name.starts_with("scraperight.")
            && name.ends_with(".jsonl")
            && name.len() == "scraperight.2026-10-11.jsonl".len(),
        "daily name shape: {name}"
    );

    let text = fs::read_to_string(dir.join(name)).expect("read daily file");
    let installed = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid JSON line"))
        .find(|value| value["detail"] == "installed")
        .expect("installed event flushed on guard drop");
    assert_eq!(installed["message"], "install check");

    fs::remove_dir_all(dir).expect("remove install fixture");
}
