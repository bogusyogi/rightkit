//! `init_diagnostics` end to end: one call installs the crash hook and the daily JSONL tracing
//! logs under one data root, and a real panic lands in both. One test per binary: the panic hook
//! and the `tracing` subscriber are process-wide.
#![cfg(feature = "diagnostics")]

use rightkit_crash::{init_diagnostics, DiagnosticsConfig, ProcessKind};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CREDENTIAL: &str = "sk-live-4f9a8b7c6d5e4f3a2b1c";

static APP_SINK_CALLS: AtomicUsize = AtomicUsize::new(0);

fn temp_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "rightkit-crash-diagnostics-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
}

fn log_lines(dir: &Path) -> Vec<Value> {
    files_in(dir)
        .iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn one_call_wires_crash_reports_and_daily_logs_and_a_panic_reaches_both() {
    let root = temp_root();
    let mut config = DiagnosticsConfig::new(ProcessKind::Engine, "Example App", "9.9.9", &root);
    // A sink the app already had keeps running alongside the log event.
    config.crash = config
        .crash
        .with_sink(|_| {
            APP_SINK_CALLS.fetch_add(1, Ordering::SeqCst);
        })
        .redact_values(vec![CREDENTIAL.into()]);
    let diagnostics = init_diagnostics(config);
    assert!(
        diagnostics.log_error().is_none(),
        "log writer should start: {:?}",
        diagnostics.log_error()
    );
    assert!(diagnostics.tracing().is_some());
    assert_eq!(diagnostics.crash_dir(), root.join("crashes"));
    assert_eq!(diagnostics.log_dir(), root.join("logs"));

    tracing::info!(target: "example", step = "started", "engine ready");
    let worker = std::thread::Builder::new()
        .name("export-worker".into())
        .spawn(|| panic!("export failed with {CREDENTIAL}"))
        .unwrap();
    assert!(worker.join().is_err());

    // A second install keeps the first subscriber and reports why its own logs are off.
    let second = init_diagnostics(DiagnosticsConfig::new(
        ProcessKind::Helper,
        "Example App",
        "9.9.9",
        root.join("second"),
    ));
    assert!(second.log_error().is_some());
    assert!(second.tracing().is_none());
    drop(second);
    drop(diagnostics); // flushes every accepted line

    let reports = files_in(&root.join("crashes"));
    assert_eq!(reports.len(), 1, "one crash report: {reports:?}");
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(&reports[0]).unwrap()).unwrap();
    assert_eq!(report["kind"], "panic");
    assert_eq!(report["process_kind"], "engine");
    assert_eq!(report["thread"], "export-worker");
    assert!(!report.to_string().contains(CREDENTIAL));
    assert_eq!(APP_SINK_CALLS.load(Ordering::SeqCst), 1);

    let log_files = files_in(&root.join("logs"));
    assert_eq!(log_files.len(), 1, "one daily file: {log_files:?}");
    let name = log_files[0].file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("example-app-engine.") && name.ends_with(".jsonl"),
        "{name}"
    );
    let lines = log_lines(&root.join("logs"));
    assert!(lines
        .iter()
        .any(|l| l["message"] == "engine ready" && l["step"] == "started"));
    let crash = lines
        .iter()
        .find(|l| l["message"] == "crash report recorded")
        .unwrap_or_else(|| panic!("crash event in the log: {lines:?}"));
    assert_eq!(crash["level"], "ERROR");
    assert_eq!(crash["target"], "rightkit_crash");
    assert_eq!(crash["kind"], "panic");
    assert_eq!(crash["process_kind"], "engine");
    assert_eq!(crash["crash_message"], "export failed with [redacted]");
    let logged = std::fs::read_to_string(&log_files[0]).unwrap();
    assert!(!logged.contains(CREDENTIAL), "{logged}");

    let _ = std::fs::remove_dir_all(&root);
}
