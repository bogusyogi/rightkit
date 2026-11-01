//! PTY coverage for native usage, hook readiness, and durable event inspection.

use crate::support::tui_harness as h;
use h::*;

#[test]
fn usage_hooks_and_logs_show_canonical_runtime_data() {
    let provider = ScriptedProvider::start(vec![Step::text("p15 trace response")]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            extra_config: "\n[tui]\nstatusline = [\"model\", \"cost\"]\n",
            ..Default::default()
        },
    );
    tui.ready_loose();

    tui.submit("p15 durable trace prompt");
    tui.wait_for_text("p15 trace response");
    let footer = tui.wait_viewport("cost footer", |view| {
        view.contains("cost unavailable") || view.contains('$')
    });
    assert!(
        footer.contains("cost unavailable") || footer.contains('$'),
        "cost is shown only when priced:\n{footer}"
    );

    tui.submit("/usage");
    tui.wait_for_text("Prompt tokens:");
    tui.wait_for_text("Estimated cost:");

    tui.submit("/hooks");
    let hooks = tui.wait_for_text("configured");
    assert!(
        hooks.contains("hooks") || hooks.contains("Hook"),
        "redacted hook readiness is shown:\n{hooks}"
    );

    tui.submit("/logs");
    let logs = tui.wait_viewport("durable event trace", |view| {
        view.contains("Session event trace")
    });
    assert!(
        logs.contains("usageUpdated") || logs.contains("assistant"),
        "actual session events appear in trace:\n{logs}"
    );
    tui.send(ESC);
    tui.wait_viewport("trace closed", |view| view.contains("Ask anything"));
    assert_eq!(provider.request_count(), 1);
}
