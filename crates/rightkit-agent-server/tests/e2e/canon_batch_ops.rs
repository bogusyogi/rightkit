//! Runtime qualification and resource evidence for OPS-002/003/006/007.

use crate::support::{
    e2e_api::Rig,
    tui_harness::{LaunchOptions, ScriptedProvider, Step, Tui},
};

#[test]
fn ops002_profile_topology_sampler_distinguishes_strict_practical_and_service() {
    let rig = Rig::start(vec![]);
    for profile in ["strict", "practical", "service"] {
        let (status, snapshot) = rig.get(&format!("/runtime/process-topology?profile={profile}"));
        assert_eq!(status, 200, "{profile} topology: {snapshot}");
        assert_eq!(snapshot["profile"], profile);
        assert!(
            snapshot["sampledAt"].is_string(),
            "timestamped process sample: {snapshot}"
        );
        assert!(
            snapshot["processes"].as_array().is_some(),
            "process rows: {snapshot}"
        );
    }
}

#[test]
fn ops003_resource_measurement_publishes_sampled_series_and_eight_hour_soak_receipt() {
    let rig = Rig::start(vec![]);
    let (status, metrics) = rig.get("/runtime/resource-metrics?window=8h");
    assert_eq!(status, 200, "resource series: {metrics}");
    let samples = metrics["samples"].as_array().expect("time series");
    assert!(
        samples.len() >= 2,
        "resource trend is based on samples: {metrics}"
    );
    let duration = metrics["windowSeconds"].as_u64().expect("window");
    assert!(
        duration >= 8 * 60 * 60,
        "eight hour soak interval is represented"
    );
    for key in [
        "rssBytes",
        "threads",
        "childCount",
        "queueDepth",
        "sqliteLatencyMs",
    ] {
        assert!(
            samples.iter().all(|sample| sample[key].is_number()),
            "{key} sampled for each point: {metrics}"
        );
    }
}

#[test]
fn ops006_qualification_matrix_runs_through_real_session_capability_path() {
    let rig = Rig::start(vec![crate::support::scripted_provider::Step::text(
        "matrix-ok",
    )]);
    rig.session("matrix", serde_json::json!({}));
    let (status, run) = rig.post(
        "/qualification/matrix/run",
        serde_json::json!({"sessionId":"matrix","capabilities":["read_file"]}),
    );
    assert_eq!(status, 200, "runtime qualification matrix: {run}");
    assert_eq!(run["executionPath"], "session-engine");
    assert!(
        run["cases"].as_array().is_some_and(|rows| !rows.is_empty()),
        "executed capability rows: {run}"
    );
}

#[test]
fn ops007_real_binary_restores_terminal_modes_after_alt_screen_and_quit() {
    let provider = ScriptedProvider::start(vec![Step::text("terminal-ok")]);
    // CG-A03 retains inline scrollback by default; OPS-007 qualifies the
    // optional alternate-screen mode, selected by the shipped [tui] setting.
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            extra_config: "[tui]\nfullscreen = true\n",
            ..Default::default()
        },
    );
    tui.ready();
    tui.submit("hello");
    tui.wait_for_text("terminal-ok");
    let raw = tui.raw_output();
    assert!(raw.contains("?1049h"), "TUI enters alternate screen");
    // Esc cancels/dismisses; /quit is the documented clean exit path.
    tui.submit("/quit");
    tui.wait_exit();
    let raw = tui.raw_output();
    assert!(raw.contains("?1049l"), "quit restores primary screen");
    assert!(raw.contains("[?25h"), "quit restores cursor visibility");
}
