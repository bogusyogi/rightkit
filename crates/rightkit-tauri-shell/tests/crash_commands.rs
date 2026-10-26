#![cfg(feature = "crash")]
//! The standalone renderer crash sink (`crash_commands`). `CrashSink` and `crash_route` have no
//! Tauri types, so they run without a webview; the command wrapper only adds the caller check and
//! the `unsupported` answer.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_crash::{CrashConfig, WindowErrorGate, WindowErrorReport};
use rightkit_tauri_shell::crash_commands::{crash_route, CrashRoute, CrashSink};

type Captured = Arc<Mutex<Vec<serde_json::Value>>>;

/// A configuration that hands each report to a capture instead of writing a file.
fn capturing_config() -> (CrashConfig, Captured) {
    let out: Captured = Arc::default();
    let sink_out = Arc::clone(&out);
    let config = CrashConfig::new("DemoApp", "1.2.3", std::env::temp_dir())
        .write_report_file(false)
        .with_sink(move |report| sink_out.lock().unwrap().push(report));
    (config, out)
}

fn report(kind: &str, message: &str) -> WindowErrorReport {
    WindowErrorReport {
        kind: kind.into(),
        message: message.into(),
        stack: "at main (app://index.js:1:1)".into(),
        source: Some("app://index.js:1:1".into()),
    }
}

#[test]
fn the_sink_records_one_window_report_for_a_renderer_error() {
    let (config, out) = capturing_config();
    let sink = CrashSink::new(config, WindowErrorGate::default());

    sink.record(report("error", "boom"));

    let got = out.lock().unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["kind"], "window_error");
    assert_eq!(got[0]["process_kind"], "window");
    assert_eq!(got[0]["message"], "boom");
}

#[test]
fn an_unhandled_rejection_is_recorded_as_a_window_rejection() {
    let (config, out) = capturing_config();
    let sink = CrashSink::new(config, WindowErrorGate::default());

    sink.record(report("unhandledrejection", "nope"));

    assert_eq!(out.lock().unwrap()[0]["kind"], "window_rejection");
}

#[test]
fn a_burst_beyond_the_gate_limit_is_rate_limited() {
    let (config, out) = capturing_config();
    // Two per minute, ten for the process: the third report of a burst is refused.
    let gate = WindowErrorGate::new(Duration::from_secs(60), 2, 10);
    let sink = CrashSink::new(config, gate);

    for n in 0..6 {
        sink.record(report("error", &format!("burst {n}")));
    }

    let got = out.lock().unwrap();
    assert_eq!(got.len(), 2, "only the gate's limit is recorded: {got:?}");
    assert_eq!(got[0]["message"], "burst 0");
    assert_eq!(got[1]["message"], "burst 1");
}

#[test]
fn with_gate_replaces_the_rate_limit() {
    let (config, out) = capturing_config();
    let sink = CrashSink::new(config, WindowErrorGate::default()).with_gate(WindowErrorGate::new(
        Duration::from_secs(60),
        1,
        1,
    ));

    sink.record(report("error", "first"));
    sink.record(report("error", "second"));

    assert_eq!(out.lock().unwrap().len(), 1);
}

#[test]
fn a_host_service_wins_then_a_sink_then_nothing_is_unsupported() {
    assert_eq!(crash_route(true, true), CrashRoute::Host);
    assert_eq!(crash_route(true, false), CrashRoute::Host);
    assert_eq!(crash_route(false, true), CrashRoute::Sink);
    assert_eq!(crash_route(false, false), CrashRoute::Unsupported);
}

/// The host service keeps its public items: an app that names `CrashHostService` still compiles,
/// and its `record` goes through the same sink path.
#[cfg(feature = "host-services")]
#[test]
fn the_host_crash_service_still_records_through_the_shared_sink() {
    use rightkit_tauri_shell::host::CrashHostService;

    let (config, out) = capturing_config();
    let service = CrashHostService::new(config).with_gate(WindowErrorGate::new(
        Duration::from_secs(60),
        1,
        1,
    ));

    service.record(report("error", "one"));
    service.record(report("error", "two"));

    let got = out.lock().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["message"], "one");
}
