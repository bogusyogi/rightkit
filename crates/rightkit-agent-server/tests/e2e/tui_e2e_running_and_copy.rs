//! TUI acceptance for running-panel chrome, fullscreen mode, and Markdown copy.

use crate::support::tui_harness as h;
use base64::Engine as _;
use h::*;

#[test]
fn running_panel_groups_kinds_and_routes_stop_actions() {
    use rightkit_terminal_ui::bots_panel::{
        LiveAgentProjection, LiveSubagentsIntent, LiveSubagentsPanel, RunningProjection,
    };
    use rightkit_terminal_ui::component::{Component, Event};
    use rightkit_terminal_ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rightkit_terminal_ui::nav::AgentId;

    let mut panel = LiveSubagentsPanel::default();
    panel.set_agents(vec![LiveAgentProjection {
        id: AgentId("agent-1".into()),
        name: "reviewer".into(),
        status: "running".into(),
        role: "reviewer".into(),
        model: "fixture-model".into(),
        elapsed: "7s".into(),
        activity: "reading files".into(),
    }]);
    panel.set_running(vec![
        RunningProjection::Shell {
            id: "bg-1".into(),
            command: "cargo run".into(),
            elapsed: "8s".into(),
            last_output: "listening".into(),
            port: Some(3000),
            status: "running".into(),
        },
        RunningProjection::Task {
            id: "task-1".into(),
            name: "Build feature".into(),
            step: "implement".into(),
            elapsed: "9s".into(),
            status: "running".into(),
        },
    ]);
    assert_eq!(
        panel.footer_summary().as_deref(),
        Some("1 agent · 1 shell · 1 task · ← / Ctrl+T")
    );
    assert_eq!(panel.rows(), 5);

    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    panel.handle(&Event::Focus(true));
    panel.handle(&key(KeyCode::Char('k')));
    panel.handle(&key(KeyCode::Char('y')));
    assert!(matches!(
        panel.drain_intents().as_slice(),
        [LiveSubagentsIntent::ConfirmStopAgent(_)]
    ));
    panel.handle(&key(KeyCode::Char('a')));
    assert!(
        panel.drain_intents().is_empty(),
        "attach on an agent row does nothing"
    );
    panel.handle(&key(KeyCode::Down));
    panel.handle(&key(KeyCode::Char('a')));
    assert!(
        matches!(panel.drain_intents().as_slice(), [LiveSubagentsIntent::AttachShell(id)] if id == "bg-1")
    );
    panel.handle(&key(KeyCode::Char('k')));
    assert!(
        matches!(panel.drain_intents().as_slice(), [LiveSubagentsIntent::StopShell(id)] if id == "bg-1")
    );
    panel.handle(&key(KeyCode::Down));
    panel.handle(&key(KeyCode::Char('k')));
    assert!(
        matches!(panel.drain_intents().as_slice(), [LiveSubagentsIntent::StopTask(id)] if id == "task-1")
    );
    panel.set_running(vec![RunningProjection::Task {
        id: "task-1".into(),
        name: "Build feature".into(),
        step: "implement".into(),
        elapsed: "9s".into(),
        status: "running".into(),
    }]);
    assert_eq!(
        panel.footer_summary().as_deref(),
        Some("1 agent · 1 task · ← / Ctrl+T")
    );
    panel.set_running(Vec::new());
    assert_eq!(
        panel.footer_summary().as_deref(),
        Some("1 agent · ← / Ctrl+T")
    );
}

#[test]
fn fullscreen_toggle_persists_and_returns_to_inline() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    let entered_before = tui.raw_output().matches("\x1b[?1049h").count();
    tui.submit("/tui fullscreen");
    let fullscreen = tui.wait_viewport("fullscreen chat", |view| {
        view.contains("Ask anything")
            && tui.raw_output().matches("\x1b[?1049h").count() > entered_before
    });
    assert!(fullscreen.contains("Ask anything"), "{fullscreen}");
    assert!(
        tui.raw_output().contains("\x1b[?1049h"),
        "alternate screen entered"
    );

    let left_before = tui.raw_output().matches("\x1b[?1049l").count();
    tui.key(ESC);
    let inline = tui.wait_viewport("inline restored", |view| {
        view.contains("Ask anything")
            && tui.raw_output().matches("\x1b[?1049l").count() > left_before
    });
    assert!(inline.contains("Ask anything"), "{inline}");
    let config =
        std::fs::read_to_string(crate::common::app_root(tui.home.path()).join("config.toml"))
            .expect("TUI setting persisted");
    assert!(config.contains("fullscreen = false"), "{config}");
}

#[test]
fn c_copies_latest_assistant_markdown_through_osc52() {
    let markdown = "Copied **Markdown** exactly.\n\n```rust\nfn main() {}\n```";
    let provider = ScriptedProvider::start(vec![Step::text(markdown)]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            env: &[("SSH_CLIENT", "127.0.0.1 12345 22")],
            ..Default::default()
        },
    );
    tui.ready();
    tui.submit("respond with code");
    tui.wait_viewport("assistant response", |view| {
        view.contains("fn main") && view.contains("copy · c")
    });
    tui.key(b"c");
    let expected = base64::engine::general_purpose::STANDARD.encode(markdown.as_bytes());
    let deadline = std::time::Instant::now() + WAIT;
    let raw = loop {
        let raw = tui.raw_output();
        if raw.contains(&expected) && raw.contains("Copied to clipboard (") {
            break raw;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "OSC 52 copy missing: {raw:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert!(raw.contains(&expected), "copy payload missing: {raw:?}");
    assert!(
        raw.contains("Copied to clipboard ("),
        "copy toast missing: {raw:?}"
    );
}

#[test]
fn up_navigation_selects_older_assistant_message_for_copy() {
    let first = "older answer markdown";
    let provider = ScriptedProvider::start(vec![Step::text(first), Step::text("newer answer")]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            env: &[("SSH_CLIENT", "127.0.0.1 12345 22")],
            ..Default::default()
        },
    );
    tui.ready();
    tui.submit("first question");
    tui.wait_viewport("first answer", |view| view.contains(first));
    tui.submit("second question");
    tui.wait_viewport("second answer", |view| view.contains("newer answer"));
    // CG-A05 retains plain Up for composer history; Ctrl+Up enters response selection.
    tui.key(b"\x1b[1;5A");
    tui.key(UP);
    tui.key(b"c");
    let expected = base64::engine::general_purpose::STANDARD.encode(first.as_bytes());
    let deadline = std::time::Instant::now() + WAIT;
    let raw = loop {
        let raw = tui.raw_output();
        if raw.contains(&expected) {
            break raw;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "older response copy missing: {raw:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert!(raw.contains(&expected), "older response was not copied");
    // Assert on the emulated screen: ratatui diffs cells, so the raw PTY bytes need not contain the full string.
    tui.wait_for_text("Assistant message 1 selected");
}

#[test]
fn fullscreen_drag_selection_copies_exact_text_and_shows_toast() {
    let provider = ScriptedProvider::start(vec![Step::text("select-this word exactly")]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            env: &[("SSH_CLIENT", "127.0.0.1 12345 22")],
            extra_config: "[tui]\nfullscreen = true\n",
            ..Default::default()
        },
    );
    tui.ready_loose();
    tui.submit("show selectable text");
    tui.wait_viewport("response visible", |view| view.contains("select-this"));
    let view = tui.viewport();
    let (row, column) = view
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("select-this").map(|column| (row + 1, column + 1)))
        .expect("response location");
    let end_column = column + "select-this".len();
    tui.send(
        format!("\x1b[<0;{column};{row}M\x1b[<32;{end_column};{row}M\x1b[<0;{end_column};{row}m")
            .as_bytes(),
    );
    let expected = base64::engine::general_purpose::STANDARD.encode(b"select-this");
    let deadline = std::time::Instant::now() + WAIT;
    let raw = loop {
        let raw = tui.raw_output();
        if raw.contains(&expected) && raw.contains("Copied to clipboard (") {
            break raw;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "selection copy missing: {raw:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert!(raw.contains(&expected), "selection did not copy exact text");
    assert!(
        raw.contains("Copied to clipboard ("),
        "selection toast missing"
    );
}

#[test]
fn fullscreen_message_copy_picker_copies_block_or_writes_file() {
    fn click_copy(tui: &Tui) {
        let view = tui.viewport();
        let (row, column) = view
            .lines()
            .enumerate()
            .find_map(|(row, line)| line.find("copy · c").map(|column| (row + 1, column + 1)))
            .expect("message copy affordance");
        let end = column + 8;
        tui.send(format!("\x1b[<0;{column};{row}M\x1b[<0;{end};{row}m").as_bytes());
        tui.wait_viewport_text("Copy picker:");
    }

    let markdown = "Before\n\n```rust\nfn first() {}\n```\n\n```text\nsecond block\n```";
    let provider = ScriptedProvider::start(vec![Step::text(markdown)]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            extra_config: "[tui]\nfullscreen = true\n",
            // Exercise the OSC52 backend asserted below, without touching the OS clipboard.
            env: &[("SSH_CLIENT", "127.0.0.1 12345 22")],
            ..Default::default()
        },
    );
    tui.ready_loose();
    tui.submit("show code blocks");
    tui.wait_viewport("response visible", |view| {
        view.contains("second block") && view.contains("copy · c")
    });

    click_copy(&tui);
    tui.key(b"2");
    let expected = base64::engine::general_purpose::STANDARD.encode(b"second block\n");
    let deadline = std::time::Instant::now() + WAIT;
    while !tui.raw_output().contains(&expected) {
        assert!(std::time::Instant::now() < deadline, "block copy missing");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    click_copy(&tui);
    tui.key(b"w1");
    let path = tui.workspace.path().join("code-block-1.rs");
    let deadline = std::time::Instant::now() + WAIT;
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "write-to-file action missing"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), "fn first() {}\n");
}
