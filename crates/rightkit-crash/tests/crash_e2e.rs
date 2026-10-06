//! End-to-end: a real panic in a real child process; reports travel through the
//! real consented outbox to a real HTTP server on loopback.

use rightkit_crash::{
    build_support_bundle, enqueue_pending, install_panic_hook, list_pending, CrashConfig,
    CrashReport, NativeCapture, SupportBundleOptions, CRASH_EVENT_TYPE,
};
use rightkit_logs::testing::MockServer;
use rightkit_logs::{
    CollectionPolicy, FlushState, HttpTransport, LogSource, OutboxConfig, Telemetry, UploadConfig,
};
use serde_json::json;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CHILD_ENV: &str = "RK_CRASH_E2E_CHILD_DIR";
const NATIVE_ENV: &str = "RK_CRASH_E2E_NATIVE";
const DICTATED: &str = "send the quarterly numbers to the board";
const EMAIL: &str = "ada.lovelace@example.org";
const USER_PATH: &str = "/Users/adalovelace/Documents/private/notes.txt";
const CREDENTIAL: &str = "sk-live-4f9a8b7c6d5e4f3a2b1c";

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let path = std::env::temp_dir().join(format!("rightkit-crash-e2e-{label}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Executes inside a separate process: installs the hook and panics for real.
#[test]
fn child_process_panics() {
    let Ok(dir) = std::env::var(CHILD_ENV) else { return };
    let mut config = CrashConfig::new("ExampleApp", "9.9.9", PathBuf::from(dir).join("crash"));
    config.build = Some("e2e-build".into());
    if std::env::var(NATIVE_ENV).is_ok() {
        config.native_capture = NativeCapture::Auto;
    }
    install_panic_hook(config);
    let worker = std::thread::Builder::new()
        .name("export-worker".into())
        .spawn(|| {
            panic!(
                "slice failed on `{DICTATED}` for {EMAIL} reading {USER_PATH} with {CREDENTIAL} from 10.1.2.3"
            );
        })
        .unwrap();
    let _ = worker.join();
    // The hook ran on the panicking thread; make the process fail like a real crash.
    std::process::exit(101);
}

fn crash_child(dir: &Path, native: bool) -> std::process::ExitStatus {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "child_process_panics", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, dir)
        .env("RUST_BACKTRACE", "0")
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    if native {
        command.env(NATIVE_ENV, "1");
    }
    command.status().expect("spawn child")
}

fn assert_clean(text: &str, context: &str) {
    for forbidden in [DICTATED, EMAIL, "adalovelace", CREDENTIAL, "10.1.2.3"] {
        assert!(!text.contains(forbidden), "{context} leaked {forbidden:?}:\n{text}");
    }
}

fn read_report(path: &Path) -> CrashReport {
    serde_json::from_slice(&std::fs::read(path).unwrap()).expect("crash report is valid JSON")
}

#[test]
fn real_panic_in_child_process_writes_a_redacted_report() {
    let dir = temp_dir("panic");
    let status = crash_child(&dir, false);
    assert!(!status.success(), "child must have crashed");

    let reports = list_pending(&dir.join("crash"));
    assert_eq!(reports.len(), 1, "exactly one report: {reports:?}");
    let raw = std::fs::read_to_string(&reports[0]).unwrap();
    assert_clean(&raw, "crash report file");

    let report = read_report(&reports[0]);
    assert_eq!(report.kind, "panic");
    assert_eq!(report.app_name, "ExampleApp");
    assert_eq!(report.app_version, "9.9.9");
    assert_eq!(report.build.as_deref(), Some("e2e-build"));
    assert_eq!(report.thread, "export-worker");
    assert!(report.location.as_deref().unwrap().contains("crash_e2e.rs"));
    // Shape of the message survives so the report stays diagnosable.
    assert!(report.message.starts_with("slice failed on `<redacted:"), "{}", report.message);
    assert!(report.message.contains("[redacted:email]"));
    assert!(report.message.contains("/Users/<user>/Documents/private/notes.txt"));
    assert!(report.message.contains("[redacted:credential]"));
    assert!(report.message.contains("[redacted:ip]"));
    assert!(report.backtrace.contains("child_process_panics"), "backtrace should name the crashing frame");
    assert!(report.native_artifacts.is_empty());
    // No partial files left behind by the atomic write.
    let strays = std::fs::read_dir(dir.join("crash")).unwrap().flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".partial")).count();
    assert_eq!(strays, 0);
}

#[cfg(target_os = "macos")]
#[test]
fn native_sample_is_captured_locally_and_referenced_by_the_report() {
    let dir = temp_dir("native");
    assert!(!crash_child(&dir, true).success());
    let crash_dir = dir.join("crash");
    let report = read_report(&list_pending(&crash_dir)[0]);
    assert_eq!(report.native_artifacts.len(), 1, "{report:?}");
    let sample = std::fs::read_to_string(crash_dir.join(&report.native_artifacts[0])).unwrap();
    assert!(sample.contains("Call graph"), "real `sample` output expected");
    assert_clean(&sample, "native sample");
}

#[test]
fn crash_report_is_held_without_consent_then_uploaded_after_restart_with_consent() {
    let dir = temp_dir("upload");
    let server = MockServer::start();
    assert!(!crash_child(&dir, false).success());
    let crash_dir = dir.join("crash");

    let open = || {
        let mut config = UploadConfig::new(&server.url, "ExampleApp", "9.9.9");
        config.base_backoff_secs = 1;
        Telemetry::open(&dir, config, Box::new(HttpTransport::new("crash-e2e")), OutboxConfig::default()).unwrap()
    };

    // Next launch, user has not consented: report stays local, nothing is sent.
    let telemetry = open();
    let summary = enqueue_pending(&crash_dir, &telemetry.outbox);
    assert_eq!((summary.queued, summary.held_no_consent), (0, 1), "{summary:?}");
    assert_eq!(telemetry.uploader.flush_now().state, FlushState::NoConsent);
    assert_eq!(server.count(), 0);
    assert_eq!(list_pending(&crash_dir).len(), 1, "report kept on disk");

    // User opts in; report is queued once and moved to submitted/.
    telemetry.consent.grant("2026-10").unwrap();
    let summary = enqueue_pending(&crash_dir, &telemetry.outbox);
    assert_eq!(summary.queued, 1, "{summary:?}");
    assert!(list_pending(&crash_dir).is_empty());
    assert_eq!(list_pending(&crash_dir.join("submitted")).len(), 1);
    assert_eq!(enqueue_pending(&crash_dir, &telemetry.outbox).queued, 0, "never queued twice");

    // App restarts before the upload happens: the outbox persisted it.
    drop(telemetry);
    let telemetry = open();
    assert_eq!(telemetry.outbox.len(), 1);
    assert_eq!(server.count(), 0);
    let report = telemetry.uploader.flush();
    assert_eq!(report.state, FlushState::Drained, "{report:?}");
    assert_eq!(server.count(), 1);

    let body = server.bodies().remove(0);
    let event = &body["events"][0];
    assert_eq!(event["type"], CRASH_EVENT_TYPE);
    assert_eq!(event["payload"]["kind"], "panic");
    assert_eq!(event["payload"]["thread"], "export-worker");
    assert_clean(&body.to_string(), "uploaded body");
    assert!(telemetry.outbox.is_empty());
}

#[test]
fn support_bundle_is_a_redacted_zip_and_native_dumps_need_explicit_opt_in() {
    let dir = temp_dir("bundle");
    assert!(!crash_child(&dir, cfg!(target_os = "macos")).success());
    let crash_dir = dir.join("crash");

    let log_path = dir.join("events.jsonl");
    let log_line = json!({
        "schema_version": 1, "ts_ms": 1, "level": "error", "event": "export_failed",
        "message": DICTATED, "detail": format!("{EMAIL} {USER_PATH}"), "transcript": DICTATED,
    });
    std::fs::write(&log_path, format!("{log_line}\n")).unwrap();

    let build = |include_native: bool, name: &str| {
        let options = SupportBundleOptions {
            app_name: "ExampleApp".into(),
            app_version: "9.9.9".into(),
            build: "e2e".into(),
            crash_dir: crash_dir.clone(),
            log_sources: vec![LogSource::new("app", &log_path)],
            settings: json!({ "dictionary_entries": 12, "api_key": CREDENTIAL }),
            policy: CollectionPolicy::default(),
            include_native_dumps: include_native,
            destination: dir.join(name),
        };
        build_support_bundle(&options).expect("bundle")
    };

    let plain = build(false, "plain.zip");
    assert_eq!((plain.crash_reports, plain.native_artifacts, plain.log_events), (1, 0, 1));
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&plain.destination).unwrap()).unwrap();
    let mut all_text = String::new();
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        names.push(entry.name().to_string());
        if entry.is_file() {
            let mut text = String::new();
            entry.read_to_string(&mut text).unwrap();
            all_text.push_str(&text);
        }
    }
    assert!(names.iter().any(|n| n.ends_with("summary.json")));
    assert!(names.iter().any(|n| n.ends_with("diagnostics.json")));
    assert!(names.iter().any(|n| n.contains("crash/crash_panic_")));
    assert!(!names.iter().any(|n| n.contains("sample_") || n.contains("dump_")));
    assert_clean(&all_text, "support bundle");
    assert!(all_text.contains("export_failed"), "bundle keeps the event name");
    assert!(all_text.contains("dictionary_entries"), "content-free settings survive");

    #[cfg(target_os = "macos")]
    {
        let with_native = build(true, "native.zip");
        assert_eq!(with_native.native_artifacts, 1);
    }
}
