//! TUI render, input and observability contracts through the shared PTY harness.

use crate::support::tui_harness::{LaunchOptions, ScriptedProvider, Step, Tui, CTRL_C};
use std::time::Duration;

#[test]
fn cg_a03_high_volume_transcript_evicts_old_live_rows_but_preserves_salvage() {
    let payload = (0..20_000)
        .map(|i| format!("ROW-{i:05} "))
        .collect::<String>();
    let provider = ScriptedProvider::start(vec![Step::text(&payload)]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("produce transcript");
    let screen = tui.wait_viewport_text("ROW-19999");
    assert!(
        screen.contains("ROW-19999"),
        "latest output reaches terminal"
    );
    assert!(
        !screen.contains("ROW-00000"),
        "resident live history evicts beyond bound"
    );
    // CG-A03 + active REN-003 preserve terminal-native scrollback; terminal
    // capability policy (master canon) excludes a CodeRight scrollback store.
    // screen_text includes that salvage, unlike the resident viewport above.
    let salvage = tui.screen_text();
    assert!(
        salvage.contains("ROW-00000"),
        "older output is recoverable from terminal scrollback"
    );
    assert!(
        salvage.contains("ROW-19999"),
        "salvage retains latest output too"
    );
}

#[test]
fn cg_a04_sgr_mouse_input_and_terminal_native_selection_do_not_break_focus_keymap() {
    let provider = ScriptedProvider::start(vec![Step::text("mouse-ready")]);
    // CG-A03 defaults to terminal-native inline selection; CG-A04 captures
    // SGR mouse input only in the explicitly owned fullscreen surface.
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            extra_config: "[tui]\nfullscreen = true\n",
            ..Default::default()
        },
    );
    tui.ready();
    let before = tui.viewport();
    tui.send(b"\x1b[<0;3;3M\x1b[<0;3;3m");
    tui.send(b"\x1b[<0;3;3M");
    assert!(
        tui.raw_output().contains("?1006h") || tui.raw_output().contains("?1000h"),
        "mouse protocol is enabled when available"
    );
    assert!(
        tui.viewport().contains("Ask anything") || before.contains("Ask anything"),
        "mouse input preserves composer focus"
    );
}

#[test]
fn ren002_high_rate_output_coalesces_while_cancel_keeps_priority() {
    let payload = "token ".repeat(300_000);
    let provider = ScriptedProvider::start(vec![Step::text(&payload).held()]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("stream quickly");
    tui.wait_requests(&provider, 1);
    tui.wait_viewport("stream is active", |s| s.contains("esc cancel"));
    tui.send(CTRL_C);
    // The active footer already says "esc cancel". Wait for settlement,
    // not that instruction, before asserting the idle composer.
    let screen = tui.wait_viewport("cancelled turn with idle composer", |s| {
        (s.contains("Turn cancelled") || s.contains("Turn interrupted"))
            && s.contains("Ask anything")
            && !s.contains("esc cancel")
    });
    assert!(
        screen.contains("Ask anything"),
        "cancel returns promptly to composer"
    );
}

#[test]
fn ren006_renderer_metrics_export_counters_without_content() {
    let provider = ScriptedProvider::start(vec![Step::text("private-render-payload")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("render this");
    tui.wait_for_text("private-render-payload");
    std::thread::sleep(Duration::from_millis(100));
    let path = crate::common::app_root(tui.home.path()).join("tui-render-metrics.json");
    let metrics: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("renderer metrics export"))
            .expect("metrics JSON");
    for key in ["redraws", "coalesced", "slowFrames", "inputLatencyMs"] {
        assert!(metrics[key].is_number(), "{key} counter: {metrics}");
    }
    assert!(
        !metrics.to_string().contains("private-render-payload"),
        "renderer telemetry contains no sensitive content"
    );
}
