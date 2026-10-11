//! Worker lifecycle events, typed error kinds, the canary deadline, and the host health hook.
//!
//! Shell-backed fake workers stand in for the real child, so the event order and the
//! kind mapping are exercised without an OS OCR backend. The native-only tests run only where
//! a backend exists and skip elsewhere.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_ocr::worker::{self, MAX_INPUT_BYTES, WORKER_DEADLINE};
use rightkit_ocr::{
    BackendPreference, NativeHealthHook, Ocr, OcrBackend, OcrError, OcrErrorKind, OcrOptions,
    WorkerCommand, WorkerEvent, WorkerEvents,
};

/// Collects events in order. Implements `WorkerEvents` directly, not via a closure.
#[derive(Default)]
struct Recorder(Mutex<Vec<WorkerEvent>>);

impl Recorder {
    fn events(&self) -> Vec<WorkerEvent> {
        self.0.lock().unwrap().clone()
    }
}

impl WorkerEvents for Recorder {
    fn on_event(&self, event: WorkerEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[cfg(unix)]
fn sh(script: &str) -> WorkerCommand {
    WorkerCommand::new("/bin/sh", ["-c", script])
}

fn missing_executable() -> WorkerCommand {
    WorkerCommand::new("/nonexistent/rightkit-ocr-no-such-worker", ["--probe"])
}

#[cfg(unix)]
#[test]
fn successful_run_reports_spawn_wait_exit_and_response_in_order() {
    let body = r#"{"ok":true,"lines":[]}"#;
    let cmd = sh(&format!("cat >/dev/null; printf '%s\\n' '{body}'"));
    let rec = Recorder::default();
    let run = cmd.run_observed(b"x", &rec).expect("successful run");
    assert!(run.lines.is_empty());

    let pid = run.record.pid.expect("pid recorded");
    let events = rec.events();
    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(events[0], WorkerEvent::Spawned { pid });
    assert_eq!(events[1], WorkerEvent::Waiting);
    assert_eq!(
        events[2],
        WorkerEvent::Exited {
            code: Some(0),
            success: true
        }
    );
    let expected_bytes = body.len() + 1; // the trailing newline
    assert!(
        matches!(events[3], WorkerEvent::Responded { bytes, .. } if bytes == expected_bytes),
        "{events:?}"
    );
    assert_eq!(run.record.response_bytes, expected_bytes);
    assert_eq!(run.record.exit_code, Some(0));
    assert!(run.record.exit_success);
    assert!(!run.record.timed_out);
}

#[cfg(unix)]
#[test]
fn closure_observer_receives_the_same_events() {
    let cmd = sh(r#"cat >/dev/null; printf '%s\n' '{"ok":true,"lines":[]}'"#);
    let seen = Mutex::new(Vec::new());
    let record_event = |event: WorkerEvent| seen.lock().unwrap().push(event);
    let run = cmd.run_observed(b"x", &record_event).expect("run");
    let seen = seen.into_inner().unwrap();
    assert_eq!(
        seen.first(),
        Some(&WorkerEvent::Spawned {
            pid: run.record.pid.unwrap()
        })
    );
    assert_eq!(seen.len(), 4, "{seen:?}");
}

#[cfg(unix)]
#[test]
fn native_unavailable_child_error_keeps_text_and_maps_kind() {
    let cmd = sh(
        r#"cat >/dev/null; printf '%s\n' '{"ok":false,"error":"macos-vision-unavailable: simulated"}'"#,
    );
    let rec = Recorder::default();
    let failure = cmd.run_observed(b"x", &rec).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::MacosVisionUnavailable);
    assert_eq!(failure.to_string(), "macos-vision-unavailable: simulated");
    assert_eq!(
        cmd.run(b"x").unwrap_err(),
        "macos-vision-unavailable: simulated",
        "run() keeps the legacy string"
    );
    assert!(rec.events().contains(&WorkerEvent::Exited {
        code: Some(0),
        success: true
    }));
}

#[cfg(unix)]
#[test]
fn plain_child_error_stays_a_runtime_failure() {
    let cmd = sh(r#"cat >/dev/null; printf '%s\n' '{"ok":false,"error":"empty image"}'"#);
    let failure = cmd.run_observed(b"x", &Recorder::default()).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::Runtime);
    assert_eq!(failure.message, "empty image");
}

#[cfg(unix)]
#[test]
fn parent_deadline_emits_timed_out_before_exit_and_kills_the_child() {
    let cmd = sh("exec sleep 30").with_deadline(Duration::from_millis(300));
    let rec = Recorder::default();
    let started = Instant::now();
    let failure = cmd.run_observed(b"x", &rec).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "child was not killed"
    );

    assert_eq!(failure.kind, OcrErrorKind::WorkerTimeout);
    assert_eq!(failure.message, "OCR worker exceeded its deadline");
    assert!(failure.record.timed_out);
    let pid = failure.record.pid.expect("spawned child has a pid");
    assert_eq!(
        rec.events(),
        vec![
            WorkerEvent::Spawned { pid },
            WorkerEvent::Waiting,
            WorkerEvent::TimedOut,
            WorkerEvent::Exited {
                code: None,
                success: false
            },
            WorkerEvent::Responded {
                bytes: 0,
                elapsed: failure.record.elapsed
            },
        ]
    );
}

#[cfg(unix)]
#[test]
fn child_watchdog_exit_is_a_timeout_reported_after_exit() {
    let cmd = sh(&format!(
        "cat >/dev/null; exit {}",
        worker::SELF_DEADLINE_EXIT
    ));
    let rec = Recorder::default();
    let failure = cmd.run_observed(b"x", &rec).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::WorkerTimeout);
    assert_eq!(failure.message, "OCR worker exceeded its deadline");
    assert!(failure.record.timed_out);
    assert_eq!(failure.record.exit_code, Some(worker::SELF_DEADLINE_EXIT));

    let events = rec.events();
    let exited = events
        .iter()
        .position(|e| matches!(e, WorkerEvent::Exited { .. }))
        .expect("exited");
    let timed_out = events
        .iter()
        .position(|e| *e == WorkerEvent::TimedOut)
        .expect("timed out");
    assert!(
        exited < timed_out,
        "child watchdog timeout follows Exited: {events:?}"
    );
}

#[test]
fn spawn_failure_emits_no_child_events_and_keeps_the_legacy_message() {
    let cmd = missing_executable();
    let rec = Recorder::default();
    let failure = cmd.run_observed(b"x", &rec).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::WorkerFailed);
    assert_eq!(failure.message, "OCR worker process could not start");
    assert_eq!(failure.record.pid, None);
    assert!(rec.events().is_empty(), "{:?}", rec.events());
    assert_eq!(
        cmd.run(b"x").unwrap_err(),
        "OCR worker process could not start"
    );
}

#[test]
fn oversized_request_is_rejected_before_any_spawn() {
    let cmd = missing_executable();
    let rec = Recorder::default();
    let bytes = vec![0u8; MAX_INPUT_BYTES + 1];
    let failure = cmd.run_observed(&bytes, &rec).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::RequestTooLarge);
    assert_eq!(failure.record.pid, None);
    assert!(rec.events().is_empty());
}

#[cfg(unix)]
#[test]
fn malformed_child_output_is_a_protocol_failure_with_its_size_recorded() {
    let cmd = sh("cat >/dev/null; echo not-json");
    let failure = cmd.run_observed(b"x", &Recorder::default()).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::WorkerProtocol);
    assert_eq!(failure.message, "OCR worker returned malformed JSON");
    assert_eq!(failure.record.response_bytes, "not-json\n".len());
    assert_eq!(
        cmd.run(b"x").unwrap_err(),
        "OCR worker returned malformed JSON"
    );
}

#[cfg(unix)]
#[test]
fn flooding_child_output_is_an_output_exceeded_failure() {
    let cmd = sh("cat >/dev/null; head -c 9000000 /dev/zero");
    let failure = cmd.run_observed(b"x", &Recorder::default()).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::WorkerOutputExceeded);
    assert!(failure.message.contains("limit"), "{}", failure.message);
}

#[cfg(unix)]
#[test]
fn canary_deadline_is_separate_from_the_request_deadline() {
    assert_eq!(
        WorkerCommand::new("/bin/sh", ["-c", "true"]).canary_deadline,
        WORKER_DEADLINE,
        "default canary deadline keeps the existing bound"
    );
    let cmd = sh("cat >/dev/null; exec sleep 30")
        .with_deadline(Duration::from_secs(60))
        .with_canary_deadline(Duration::from_millis(300));
    let canary = cmd.for_canary();
    assert_eq!(canary.deadline, Duration::from_millis(300));
    assert_eq!(
        cmd.deadline,
        Duration::from_secs(60),
        "request deadline untouched"
    );

    let started = Instant::now();
    let failure = canary.run_observed(b"x", &Recorder::default()).unwrap_err();
    assert_eq!(failure.kind, OcrErrorKind::WorkerTimeout);
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// The router's canary goes through the canary deadline, not the request's. The fake worker
/// answers the request with an empty result and hangs on the canary-sized input, so a blank
/// page only passes if the canary bound applies. Runs only where Vision is the backend.
#[cfg(unix)]
#[test]
fn router_canary_uses_the_canary_deadline_for_blank_pages() {
    if rightkit_ocr::native_backend() != Some(OcrBackend::Vision) {
        return;
    }
    let canary_len = rightkit_ocr::canary_png().len();
    let script = format!(
        "n=$(wc -c | tr -d ' '); if [ \"$n\" -eq {canary_len} ]; then exec sleep 30; fi; \
         printf '%s\\n' '{{\"ok\":true,\"lines\":[]}}'"
    );
    let cmd = sh(&script).with_canary_deadline(Duration::from_millis(300));
    let blank = image::RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255]));
    let mut ocr = Ocr::new(
        OcrOptions::new(BackendPreference::Native)
            .with_isolation(rightkit_ocr::Isolation::Worker(cmd)),
    );
    let started = Instant::now();
    let err = ocr.recognize(&blank).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "canary ignored its deadline"
    );
    assert!(
        matches!(err, OcrError::Runtime(ref m) if m == "OCR worker exceeded its deadline"),
        "{err:?}"
    );
}

#[test]
fn every_ocr_error_variant_maps_to_a_kind() {
    let path = PathBuf::from("/artifacts/det.onnx");
    let cases = [
        (
            OcrError::UnresolvedProvenance {
                labels: vec!["PP-OCRv5".into(), "PP-OCRv6".into()],
            },
            OcrErrorKind::Provenance,
        ),
        (OcrError::InvalidProvenance, OcrErrorKind::Provenance),
        (
            OcrError::UnsafeArtifactPath { path: path.clone() },
            OcrErrorKind::Artifact,
        ),
        (
            OcrError::MissingArtifact { path: path.clone() },
            OcrErrorKind::Artifact,
        ),
        (
            OcrError::DigestMismatch {
                path,
                expected: "a".into(),
                actual: "b".into(),
            },
            OcrErrorKind::Artifact,
        ),
        (OcrError::Runtime("boom".into()), OcrErrorKind::Runtime),
        (
            OcrError::BackendUnavailable("macos-vision-unavailable: x".into()),
            OcrErrorKind::MacosVisionUnavailable,
        ),
        (
            OcrError::BackendUnavailable("windows-ocr-unavailable: x".into()),
            OcrErrorKind::WindowsOcrUnavailable,
        ),
        (
            OcrError::BackendUnavailable("ocr-unsupported-platform: x".into()),
            OcrErrorKind::UnsupportedPlatform,
        ),
        (
            OcrError::BackendUnavailable("ONNX fallback not compiled in".into()),
            OcrErrorKind::BackendUnavailable,
        ),
        (
            OcrError::NoFallbackConfigured("macos-vision-unavailable: x".into()),
            OcrErrorKind::NoFallbackConfigured,
        ),
    ];
    for (err, kind) in cases {
        assert_eq!(err.kind(), kind, "{err:?}");
    }
}

#[test]
fn legacy_prefixes_still_classify_and_display_keeps_them() {
    use OcrErrorKind::*;
    assert_eq!(
        OcrErrorKind::from_message("macos-vision-unavailable: x"),
        Some(MacosVisionUnavailable)
    );
    assert_eq!(
        OcrErrorKind::from_message("windows-ocr-unavailable: x"),
        Some(WindowsOcrUnavailable)
    );
    assert_eq!(
        OcrErrorKind::from_message("ocr-unsupported-platform: x"),
        Some(UnsupportedPlatform)
    );
    assert_eq!(OcrErrorKind::from_message("empty image"), None);

    for message in [
        "macos-vision-unavailable: x",
        "windows-ocr-unavailable: x",
        "ocr-unsupported-platform: x",
        "empty image",
    ] {
        assert_eq!(
            rightkit_ocr::is_backend_unavailable(message),
            OcrErrorKind::from_message(message).is_some_and(|k| k.is_backend_unavailable()),
            "{message}"
        );
    }
    assert!(!BackendUnavailable.is_backend_unavailable());
    assert_eq!(WorkerTimeout.as_str(), "worker_timeout");
    assert_eq!(MacosVisionUnavailable.as_str(), "macos_vision_unavailable");

    let text = OcrError::BackendUnavailable("macos-vision-unavailable: x".into()).to_string();
    assert!(text.contains("macos-vision-unavailable: x"), "{text}");
}

#[derive(Debug)]
struct Hook {
    cached: Option<bool>,
    recorded: Mutex<Vec<bool>>,
    reads: AtomicUsize,
}

impl Hook {
    fn new(cached: Option<bool>) -> Arc<Self> {
        Arc::new(Self {
            cached,
            recorded: Mutex::new(Vec::new()),
            reads: AtomicUsize::new(0),
        })
    }
}

impl NativeHealthHook for Hook {
    fn cached_verdict(&self) -> Option<bool> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.cached
    }
    fn record_verdict(&self, healthy: bool) {
        self.recorded.lock().unwrap().push(healthy);
    }
}

/// A cached verdict answers without spawning, so this needs no OCR asset.
#[test]
fn health_hook_short_circuits_the_probe_when_a_verdict_is_cached() {
    if rightkit_ocr::native_backend().is_none() {
        return;
    }
    // A cached verdict answers without spawning: the worker path does not exist.
    let cached = Hook::new(Some(true));
    let ocr = Ocr::new(
        OcrOptions::new(BackendPreference::Native)
            .with_isolation(rightkit_ocr::Isolation::Worker(missing_executable())),
    )
    .with_health_hook(cached.clone());
    assert_eq!(ocr.probe_native(), Ok(true));
    assert_eq!(cached.reads.load(Ordering::SeqCst), 1);
    assert!(cached.recorded.lock().unwrap().is_empty());
}

/// Without a cached verdict the real canary runs once and its result is recorded. This needs the
/// native OCR canary to read text on this host.
#[test]
fn health_hook_records_a_fresh_canary_verdict() {
    if rightkit_ocr::native_backend().is_none() || !common::native_ready_or_skip() {
        return;
    }
    let fresh = Hook::new(None);
    let ocr = Ocr::new(OcrOptions::new(BackendPreference::Native).with_isolation(
        rightkit_ocr::Isolation::Worker(WorkerCommand::new(
            env!("CARGO_BIN_EXE_rightkit-ocr"),
            [worker::DEFAULT_WORKER_ARG],
        )),
    ))
    .with_health_hook(fresh.clone());
    assert_eq!(ocr.probe_native(), Ok(true));
    assert_eq!(*fresh.recorded.lock().unwrap(), vec![true]);
}
