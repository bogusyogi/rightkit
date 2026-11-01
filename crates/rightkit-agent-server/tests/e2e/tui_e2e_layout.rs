//! TUI end-to-end: screen layout, footer/status, scrollback, small terminals,
//! session commands and desktop notifications.

use crate::support::tui_harness as h;
use h::*;
use std::time::Duration;

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

// 1. Startup: wordmark + ready line, then rule / composer / rule / footer pinned to the bottom.
#[test]
fn startup_layout_is_wordmark_ready_line_composer_and_footer() {
    let provider = ScriptedProvider::start(vec![]);
    let tui = Tui::launch(&provider, &[]);
    let view = tui.ready();
    let rows: Vec<&str> = view.lines().collect();
    let non_blank: Vec<(usize, &str)> = rows
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        // The untrusted-workspace notice (STATUS D2) is transcript, not layout.
        .filter(|(_, l)| !l.contains("Trust this folder?") && !l.contains("Run /trust"))
        .collect();
    let texts: Vec<&str> = non_blank.iter().map(|(_, l)| *l).collect();
    // Notices between the ready line and the composer rule (e.g. the visible
    // "Context is off (...)" degradation) are transcript, not layout.
    let rule = texts
        .iter()
        .position(|l| l.starts_with('─'))
        .unwrap_or_else(|| panic!("composer rule missing: {view}"));
    let texts: Vec<&str> = texts[..2.min(rule)]
        .iter()
        .chain(texts[rule..].iter())
        .copied()
        .collect();
    assert!(texts[0].contains("CodeRight"), "{view}");
    assert!(
        texts[1].contains("ready") && texts[1].contains(MODEL),
        "{view}"
    );
    assert!(texts[2].starts_with('─'), "{view}");
    assert!(
        texts[3].starts_with('▌') && texts[3].contains("Ask anything"),
        "{view}"
    );
    assert!(texts[4].starts_with('─'), "{view}");
    let footer = texts[5];
    let dir = tui
        .workspace
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        footer.contains(&dir)
            && footer.contains("e2e-branch")
            && footer.contains("Ask")
            && footer.contains(MODEL),
        "{footer}"
    );
    assert_eq!(
        non_blank.last().unwrap().0,
        rows.len() - 1,
        "footer sits on the last terminal row:\n{view}"
    );
    assert_eq!(texts.len(), 6, "nothing else on a fresh screen:\n{view}");
    let (row, _) = tui.cursor();
    let composer_row = non_blank
        .iter()
        .find(|(_, l)| l.starts_with('▌'))
        .map(|(row, _)| *row)
        .expect("composer row");
    assert_eq!(row as usize, composer_row, "the cursor is in the composer");
}

// 2. A narrow terminal clips the cwd but keeps branch, mode and never overflows.
#[test]
fn narrow_footer_keeps_mode_and_fits_the_width() {
    let provider = ScriptedProvider::start(vec![]);
    let tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            cols: 56,
            ..Default::default()
        },
    );
    let view = tui.wait_viewport("footer", |v| {
        v.contains("e2e-branch") && v.contains("Ask") && !v.contains("Starting context")
    });
    for row in view.lines() {
        assert!(
            row.chars().count() <= 56,
            "row wider than the terminal: {row:?}"
        );
    }
    let footer = view.lines().last().unwrap();
    assert!(
        footer.contains("e2e-branch") && footer.contains("Ask"),
        "{footer}"
    );
}

// 3. A very short terminal still works end to end.
#[test]
fn short_terminal_still_takes_a_prompt_and_shows_the_answer() {
    let provider = ScriptedProvider::start(vec![Step::text("short-screen-answer")]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            rows: 10,
            ..Default::default()
        },
    );
    tui.ready_loose();
    tui.submit("hi in a tiny terminal");
    let view = tui.wait_viewport("answer visible and composer idle", |v| {
        v.contains("short-screen-answer") && v.contains("Ask anything")
    });
    assert!(
        view.lines().last().unwrap().contains("e2e-branch"),
        "footer stays on the last row:\n{view}"
    );
    assert!(
        view.contains("Ask anything"),
        "composer ready again:\n{view}"
    );
}

// 4. Working indicator while a turn runs, gone when it ends; ctx gauge appears.
#[test]
fn working_row_shows_during_a_turn_and_clears_after() {
    let provider = ScriptedProvider::start(vec![Step::text("first half second half").held()]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("work for a bit");
    tui.wait_requests(&provider, 1);
    let busy = tui.wait_viewport("working row", |v| v.contains("esc cancel"));
    assert!(
        busy.contains("Thinking") || busy.contains("Working"),
        "{busy}"
    );
    provider.release(0);
    let done = tui.wait_viewport("settled", |v| {
        !v.contains("esc cancel") && v.contains("% ctx")
    });
    assert!(!done.contains("Working"), "{done}");
}

// 5. A failed turn is held on the status row and cleared by the next prompt.
#[test]
fn failure_is_held_until_the_next_prompt() {
    let empty = Step {
        prompt_tokens: 100,
        completion_tokens: 0,
        ..Step::default()
    };
    let provider = ScriptedProvider::start(vec![
        empty.clone(),
        empty.clone(),
        empty,
        Step::text("recovered answer"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("this one fails");
    let failed = tui.wait_viewport("held failure", |v| v.contains("esc dismiss"));
    assert!(failed.contains("✗"), "{failed}");
    tui.submit("try again");
    tui.wait_for_text("recovered answer");
    let after = tui.wait_viewport("failure cleared", |v| {
        !v.contains("esc dismiss") && v.contains("Ask anything")
    });
    assert!(after.contains("Ask anything"), "{after}");
}

// 6. Many turns scroll into the terminal scrollback, in order, nothing duplicated.
//    (Four turns: a background self-review request fires after the fifth.)
#[test]
fn long_conversation_scrolls_into_scrollback_in_order() {
    let body = |i: usize| {
        (0..9)
            .map(|n| format!("reply-{i:02}-line-{n}"))
            .collect::<Vec<_>>()
            .join(
                "

",
            )
    };
    let script: Vec<Step> = (1..=4).map(|i| Step::text(&body(i))).collect();
    let provider = ScriptedProvider::start(script);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    for i in 1..=4 {
        tui.submit(&format!("question-number-{i:02}"));
        tui.wait_for_text(&format!("reply-{i:02}-line-8"));
        tui.wait_viewport("idle", |v| v.contains("Ask anything"));
    }
    let screen = tui.screen_text();
    let mut last = 0usize;
    for i in 1..=4 {
        let mut tags = vec![format!("question-number-{i:02}")];
        tags.extend((0..9).map(|n| format!("reply-{i:02}-line-{n}")));
        for tag in tags {
            assert_eq!(
                screen.matches(&tag).count(),
                1,
                "{tag} appears exactly once:
{screen}\n--- raw PTY ---\n{:?}",
                tui.raw_output()
            );
            let at = screen.find(&tag).unwrap();
            assert!(at > last, "{tag} out of order");
            last = at;
        }
    }
    assert!(
        screen.lines().count() > 36,
        "the conversation outgrew one screen"
    );
    let view = tui.viewport();
    assert!(
        view.lines().last().unwrap().contains("e2e-branch"),
        "footer still pinned after scrolling:
{view}"
    );
}

// 6b. Internal event identifiers never appear as transcript text.
#[test]
fn no_internal_event_names_leak_into_the_transcript() {
    let script: Vec<Step> = (1..=8).map(|i| Step::text(&format!("reply-{i}"))).collect();
    let provider = ScriptedProvider::start(script);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    for i in 1..=6 {
        tui.submit(&format!("q{i}"));
        tui.wait_viewport("idle", |v| v.contains("Ask anything"));
    }
    pause(1500);
    let screen = tui.screen_text();
    assert!(!screen.contains("[selfReviewProposalReceived]"), "{screen}");
}

// 7. /clear is /new: it starts a new session (the old one stays in history).
#[test]
fn clear_command_starts_a_new_session() {
    let provider = ScriptedProvider::start(vec![
        Step::text("answer-before-clear"),
        Step::text("answer-after-clear"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("remember the word zebra");
    tui.wait_for_text("answer-before-clear");
    tui.wait_viewport("idle", |v| !v.contains("esc cancel"));
    tui.submit("/clear");
    tui.wait_for_text("Started a new session");
    tui.submit("what word was it");
    tui.wait_for_text("answer-after-clear");
    let second = &provider.requests()[1];
    assert!(
        !user_texts(second).iter().any(|t| t.contains("zebra")),
        "a new session starts with fresh model context: {:?}",
        user_texts(second)
    );
}

// 8. A finished background turn raises an OSC 9 notification plus a titled window.
#[test]
fn finished_turn_notifies_via_osc9_when_unfocused() {
    let provider = ScriptedProvider::start(vec![Step::text("slow turn complete").held()]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            env: &[("RIGHTKIT_NOTIFY", "osc9")],
            ..Default::default()
        },
    );
    tui.ready();
    tui.send(b"\x1b[O"); // terminal reports focus lost
    tui.submit("take your time");
    tui.wait_requests(&provider, 1);
    // Windows only notifies for turns that ran at least 10s.
    pause(if cfg!(windows) { 11_000 } else { 1_500 });
    provider.release(0);
    tui.wait_for_text("slow turn complete");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let raw = tui.raw_output();
        if raw.contains("\u{1b}]9;") {
            assert!(
                raw.contains("CodeRight ·"),
                "titled window / notification text missing"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no OSC 9 notification in the terminal stream\n{}",
            tui.screen_text()
        );
        pause(100);
    }
}

// 9. No notification while the terminal has focus.
#[test]
fn focused_terminal_gets_no_notification() {
    let provider = ScriptedProvider::start(vec![Step::text("quiet turn complete")]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            env: &[("RIGHTKIT_NOTIFY", "osc9")],
            ..Default::default()
        },
    );
    tui.ready();
    tui.send(b"\x1b[I"); // focus gained
    tui.submit("quick one");
    tui.wait_for_text("quiet turn complete");
    pause(1500);
    assert!(
        !tui.raw_output().contains("\u{1b}]9;"),
        "a focused terminal must not be notified"
    );
}

// ---------------------------------------------------------------------------
// D2: first launch in an untrusted folder asks "Trust this folder? (yes/no)".
// ---------------------------------------------------------------------------
fn trust_store_text(tui: &Tui) -> String {
    std::fs::read_to_string(crate::common::app_root(tui.home.path()).join("workspace-trust.json"))
        .unwrap_or_default()
}

fn launch_untrusted(
    provider: &ScriptedProvider,
    reuse: Option<(std::path::PathBuf, std::path::PathBuf)>,
) -> Tui {
    Tui::launch_with(
        provider,
        LaunchOptions {
            untrusted: true,
            reuse,
            ..Default::default()
        },
    )
}

#[test]
fn untrusted_folder_asks_for_trust_and_yes_persists_the_decision() {
    let provider = ScriptedProvider::start(vec![Step::text("trusted turn answer")]);
    let mut first = launch_untrusted(&provider, None);
    let card = first.wait_viewport("trust card", |v| v.contains("Trust this folder? (yes/no)"));
    let dir = first
        .workspace
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        card.contains(&dir),
        "the card names the folder it asks about:\n{card}"
    );
    assert!(
        !trust_store_text(&first)
            .to_lowercase()
            .contains(&dir.to_lowercase()),
        "nothing is trusted before the user answers"
    );

    first.key(b"y");
    first.wait_for_text("Trusted workspace:");
    first.wait_viewport("composer back after the decision", |v| {
        !v.contains("Trust this folder?") && v.contains("Ask anything")
    });
    let store = trust_store_text(&first);
    assert!(
        store.contains("\"trusted\"") && store.to_lowercase().contains(&dir.to_lowercase()),
        "the yes decision is persisted for this folder:\n{store}"
    );
    first.submit("work in the trusted folder");
    first.wait_for_text("trusted turn answer");
    pause(500);
    first.kill();

    // A later launch in the same folder does not ask again.
    let provider2 = ScriptedProvider::start(vec![]);
    let second = launch_untrusted(
        &provider2,
        Some((
            first.home.path().to_path_buf(),
            first.workspace.path().to_path_buf(),
        )),
    );
    second.ready_loose();
    pause(1500);
    let view = second.viewport();
    assert!(
        !view.contains("Trust this folder?"),
        "a trusted folder must not ask again:\n{view}"
    );
}

#[test]
fn untrusted_folder_no_stays_untrusted_and_the_next_launch_asks_again() {
    let provider = ScriptedProvider::start(vec![]);
    let mut first = launch_untrusted(&provider, None);
    first.wait_viewport("trust card", |v| v.contains("Trust this folder? (yes/no)"));
    first.key(b"n");
    first.wait_for_text("Workspace left untrusted");
    first.wait_viewport("composer back after the decision", |v| {
        !v.contains("Trust this folder?") && v.contains("Ask anything")
    });
    assert!(
        !trust_store_text(&first).contains("\"trusted\""),
        "declining must not record trust:\n{}",
        trust_store_text(&first)
    );
    pause(300);
    first.kill();

    let provider2 = ScriptedProvider::start(vec![]);
    let second = launch_untrusted(
        &provider2,
        Some((
            first.home.path().to_path_buf(),
            first.workspace.path().to_path_buf(),
        )),
    );
    second.wait_viewport("trust card again", |v| {
        v.contains("Trust this folder? (yes/no)")
    });
}

// ---------------------------------------------------------------------------
// Slash aliases: /new = /clear = /cls start a new session; /quit = /exit = /q quit.
// ---------------------------------------------------------------------------
#[test]
fn new_clear_and_cls_aliases_each_start_a_new_session() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    for (index, alias) in ["/new", "/cls", "/clear"].iter().enumerate() {
        tui.submit(alias);
        tui.wait_for(&format!("{alias} starts a new session"), |screen| {
            screen.matches("Started a new session").count() > index
        });
    }
    assert_eq!(provider.request_count(), 0, "aliases are local commands");
}

fn quit_alias_exits_cleanly(alias: &str) {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit(alias);
    tui.wait_exit();
}

#[test]
fn quit_exit_and_q_aliases_each_leave_the_tui() {
    for alias in ["/quit", "/exit", "/q"] {
        quit_alias_exits_cleanly(alias);
    }
}

// ---------------------------------------------------------------------------
// A015: the terminal is handed back in a usable state after every clean exit.
// A killed process cannot run any handler (TerminateProcess), so only the
// cooperative exits are asserted.
// ---------------------------------------------------------------------------
fn assert_terminal_restored(raw: &str, how: &str) {
    let last = |needle: &str| raw.rfind(needle);
    if let Some(on) = last("\u{1b}[?2004h") {
        assert!(
            last("\u{1b}[?2004l") > Some(on),
            "{how}: bracketed paste left enabled\n--- raw PTY ---\n{raw:?}"
        );
    }
    if let Some(on) = last("\u{1b}[?1049h") {
        assert!(
            last("\u{1b}[?1049l") > Some(on),
            "{how}: alternate screen left active\n--- raw PTY ---\n{raw:?}"
        );
    }
    if let Some(hidden) = last("\u{1b}[?25l") {
        assert!(
            last("\u{1b}[?25h") > Some(hidden),
            "{how}: cursor left hidden\n--- raw PTY ---\n{raw:?}"
        );
    }
    if last("\u{1b}[?1004h").is_some() {
        // CG-A04 owns application focus modes. ConPTY retains host focus
        // reporting & rewrites an application reset as 1004l + 1004h:
        // microsoft/terminal adaptDispatch.cpp, FOCUS_EVENT_MODE (GH#12799).
        // Only normalize that exact adjacent transport-owned re-enable.
        // A standalone enable still fails, as does a missing exit reset.
        let focus = if cfg!(windows) {
            raw.replace("\u{1b}[?1004l\u{1b}[?1004h", "\u{1b}[?1004l")
        } else {
            raw.to_string()
        };
        assert!(
            focus.rfind("\u{1b}[?1004l") > focus.rfind("\u{1b}[?1004h")
                && last("\u{1b}[?1004l") > raw.find("\u{1b}[?2004l"),
            "{how}: focus reporting left enabled\n--- raw PTY ---\n{raw:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn terminal_restoration_requires_exit_reset_despite_conpty_focus_reenable() {
    let startup_and_restoration = concat!(
        "\u{1b}[?1004l\u{1b}[?1004h",
        "\u{1b}[?2004h\u{1b}[?25l",
        "\u{1b}[?2004l\u{1b}[?25h",
    );
    let reset = format!("{startup_and_restoration}\u{1b}[?1004l\u{1b}[?1004h");
    assert_terminal_restored(&reset, "ConPTY exit reset");
    assert!(
        std::panic::catch_unwind(|| {
            assert_terminal_restored(startup_and_restoration, "missing exit reset");
        })
        .is_err(),
        "startup reset must not satisfy exit restoration"
    );
    let reenabled = format!("{reset}\u{1b}[?1004h");
    assert!(
        std::panic::catch_unwind(|| {
            assert_terminal_restored(&reenabled, "standalone focus enable");
        })
        .is_err(),
        "standalone focus enable must still fail"
    );
}

#[test]
fn terminal_state_is_restored_after_quit_and_ctrl_c_quit() {
    // /quit
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    assert!(
        tui.raw_output().contains("\u{1b}[?2004h"),
        "the TUI enables bracketed paste while running"
    );
    tui.submit("/quit");
    tui.wait_exit();
    pause(300);
    assert_terminal_restored(&tui.raw_output(), "/quit");

    // Idle double Ctrl+C.
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.key(CTRL_C);
    tui.key(CTRL_C);
    tui.wait_exit();
    pause(300);
    assert_terminal_restored(&tui.raw_output(), "double Ctrl+C");
}

// ---------------------------------------------------------------------------
// A029: Ctrl+L wipes the visible screen only; the session and its context stay.
// ---------------------------------------------------------------------------
#[test]
fn ctrl_l_clears_visible_transcript_but_keeps_session() {
    let provider = ScriptedProvider::start(vec![
        Step::text("first-ctrl-l-answer"),
        Step::text("second-ctrl-l-answer"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("first ctrl l prompt");
    tui.wait_for_text("first-ctrl-l-answer");
    tui.wait_viewport("idle", |v| {
        v.contains("Ask anything") && !v.contains("esc cancel")
    });

    tui.key(b"\x0c"); // Ctrl+L
    let cleared = tui.wait_viewport("visible screen cleared, live footprint repainted", |v| {
        !v.contains("first-ctrl-l-answer") && v.contains("Ask anything") && v.contains("e2e-branch")
    });
    assert!(
        !cleared.contains("first ctrl l prompt"),
        "the echoed prompt left the visible screen too:\n{cleared}"
    );

    tui.submit("second ctrl l prompt");
    tui.wait_for_text("second-ctrl-l-answer");
    let second = &provider.requests()[1];
    assert!(
        user_texts(second)
            .iter()
            .any(|text| text.contains("first ctrl l prompt")),
        "Ctrl+L must not drop the model's context: {:?}",
        user_texts(second)
    );
}

// ---------------------------------------------------------------------------
// A035: NO_COLOR removes every colour from the rendered cells; the colour
// baseline proves the check is not vacuous; --plain and --reduced-motion still
// run a full turn.
// ---------------------------------------------------------------------------
fn coloured_cells(tui: &Tui) -> usize {
    let parser = tui.parser.lock().unwrap();
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let mut count = 0;
    for row in 0..rows {
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                if cell.fgcolor() != vt100::Color::Default
                    || cell.bgcolor() != vt100::Color::Default
                {
                    count += 1;
                }
            }
        }
    }
    count
}

#[test]
fn plain_and_reduced_motion_flags_remove_animation_and_color() {
    // Baseline: the default render is coloured.
    let provider = ScriptedProvider::start(vec![Step::text("coloured-baseline-answer")]);
    let mut baseline = Tui::launch(&provider, &[]);
    baseline.ready();
    baseline.submit("baseline");
    baseline.wait_for_text("coloured-baseline-answer");
    baseline.wait_viewport("idle", |v| v.contains("Ask anything"));
    assert!(
        coloured_cells(&baseline) > 0,
        "the default render must carry colour:\n{}",
        baseline.viewport()
    );

    // NO_COLOR + --reduced-motion: no coloured cell anywhere on the screen.
    let provider = ScriptedProvider::start(vec![Step::text("monochrome-answer")]);
    let mut mono = Tui::launch_with(
        &provider,
        LaunchOptions {
            args: &["--reduced-motion"],
            env: &[("NO_COLOR", "1")],
            ..Default::default()
        },
    );
    mono.ready();
    mono.submit("monochrome");
    mono.wait_for_text("monochrome-answer");
    mono.wait_viewport("idle", |v| v.contains("Ask anything"));
    assert_eq!(
        coloured_cells(&mono),
        0,
        "NO_COLOR must leave no coloured cell:\n{}",
        mono.viewport()
    );

    // --plain: the linear layout still takes a prompt and shows the answer once.
    let provider = ScriptedProvider::start(vec![Step::text("plain-mode-answer")]);
    let mut plain = Tui::launch(&provider, &["--plain"]);
    plain.ready_loose();
    plain.submit("plain prompt");
    let screen = plain.wait_for_text("plain-mode-answer");
    assert_eq!(
        screen.matches("plain-mode-answer").count(),
        1,
        "append-only output shows the answer exactly once:\n{screen}"
    );
}
