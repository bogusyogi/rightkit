//! PTY journeys for project navigation, TUI settings, and per-chat goals.

use crate::support::tui_harness as h;
use h::*;

#[test]
fn ctrl_b_opens_project_sidebar_and_escape_returns_to_composer() {
    // The home folder is never itself a project (D14: the picker lists projects that have
    // chats), so seed one real chat in a workspace first, then relaunch from the home folder
    // against the same data.
    let seed_provider = ScriptedProvider::start(vec![Step::text("seeded-chat")]);
    let mut seed = Tui::launch(&seed_provider, &[]);
    seed.ready_loose();
    seed.submit("seed a project chat");
    seed.wait_for_text("seeded-chat");
    std::thread::sleep(std::time::Duration::from_millis(500));
    seed.kill();

    let provider = ScriptedProvider::start(vec![Step::text("sidebar-ready")]);
    // Launch Home opens only when launched in the user's home folder.
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            cwd_home: true,
            reuse: Some((
                seed.home.path().to_path_buf(),
                seed.workspace.path().to_path_buf(),
            )),
            ..Default::default()
        },
    );
    tui.ready_home();

    // The picker opens before the context has a session list; it is refreshed
    // once readiness settles, so wait for the seeded row rather than the picker frame.
    let home = tui.wait_viewport("launch project picker lists the seeded project", |view| {
        view.contains("choose a project or chat")
            && view.contains("this folder")
            && view.contains("Project  ")
    });
    assert!(
        home.contains("Project  "),
        "the seeded project is listed and preselected:\n{home}"
    );
    tui.enter();
    tui.wait_viewport("launch picker closed", |view| view.contains("Ask anything"));

    tui.send(b"\x02");
    let sidebar = tui.wait_viewport("project sidebar", |view| {
        view.contains("Projects") && view.contains("Enter switch")
    });
    assert!(
        sidebar.contains("this folder") || sidebar.contains("Project"),
        "current project is listed:\n{sidebar}"
    );

    tui.key(ESC);
    let composer = tui.wait_viewport("sidebar closed", |view| {
        view.contains("Ask anything") && !view.contains("Enter switch")
    });
    assert!(
        composer.contains(MODEL),
        "route remains visible after closing sidebar:\n{composer}"
    );
    tui.submit("after sidebar");
    tui.wait_for_text("sidebar-ready");
    assert_eq!(provider.request_count(), 1);
}

#[test]
fn config_screen_changes_and_persists_theme() {
    let provider = ScriptedProvider::start(vec![Step::text("settings-ready")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready_loose();

    tui.submit("/config");
    tui.wait_viewport("TUI settings", |view| {
        view.contains("Settings · [tui]") && view.contains("theme: auto")
    });
    // build_config now includes Running/fullscreen/copy settings before theme.
    // Select by label so adding settings never changes this persistence proof.
    for _ in 0..16 {
        let before = tui.viewport();
        if before.lines().any(|line| line.contains("› theme: auto")) {
            break;
        }
        let selected = before
            .lines()
            .find(|line| line.contains('›'))
            .expect("selected settings row")
            .to_string();
        tui.key(DOWN);
        tui.wait_viewport("settings selection moved", |view| {
            view.lines()
                .any(|line| line.contains('›') && line != selected)
        });
    }
    tui.wait_viewport_text("› theme: auto");
    tui.enter();
    tui.wait_for_text("theme updated");

    let config =
        std::fs::read_to_string(crate::common::app_root(tui.home.path()).join("config.toml"))
            .expect("saved user config");
    assert!(
        config.contains("theme = \"dark\""),
        "[tui].theme persisted:\n{config}"
    );
    let view = tui.wait_viewport("updated settings", |view| view.contains("theme: dark"));
    assert!(
        view.contains("Settings · [tui]"),
        "settings remain open after update:\n{view}"
    );
}

#[test]
fn goal_commands_update_active_chat_goal_and_footer() {
    // Keep execution in flight while commands operate on an active goal.
    // Otherwise GoalState::MAX_NO_PROGRESS legitimately blocks this text-only
    // script before the test can edit it. Retain every lifecycle assertion.
    let provider = ScriptedProvider::start(vec![Step::text(
        "goal work response\nHolding this response while goal controls are exercised.",
    )
    .held()]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready_loose();

    tui.submit("/goal set NAVIGATION-GOAL-ONE");
    tui.wait_for_text("goal work response");
    tui.wait_viewport("goal shown in footer", |view| {
        view.contains("goal (active): NAVIGATION-GOAL-ONE")
    });

    tui.submit("/goal edit NAVIGATION-GOAL-TWO");
    tui.wait_viewport("edited goal", |view| {
        view.contains("goal (active): NAVIGATION-GOAL-TWO")
    });
    tui.submit("/goal pause");
    tui.wait_viewport("paused goal", |view| {
        view.contains("goal (paused): NAVIGATION-GOAL-TWO")
    });
    provider.release(0);
    tui.wait_viewport("paused turn settled", |view| {
        view.contains("Ask anything") && !view.contains("esc cancel")
    });
    tui.submit("/goal resume");
    tui.wait_viewport("resumed goal", |view| {
        view.contains("goal (active): NAVIGATION-GOAL-TWO")
    });
    tui.submit("/goal clear");
    tui.submit("/goal");
    tui.wait_for_text("No goal is set for this chat");
}

#[test]
fn copy_uses_clipboard_and_export_selects_html_format() {
    let provider = ScriptedProvider::start(vec![Step::text("export journey reply")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready_loose();
    tui.submit("clipboard seed");
    tui.wait_for_text("export journey reply");

    tui.submit("/copy clipboard payload");
    // Running/fullscreen/copy change unified /copy feedback in tui_host::clipboard_copy_notice.
    tui.wait_for_text("Copied to clipboard (17 characters, 1 lines)");
    tui.submit("/export html");
    tui.wait_for_text("Exported session");
    let screen = tui.screen_text();
    assert!(screen.contains("(html)"), "{screen}");
    assert_eq!(provider.request_count(), 1);
}

#[test]
fn session_menu_actions_target_selected_past_chat() {
    let provider = ScriptedProvider::start(vec![
        Step::text("past chat response"),
        Step::text("new chat response"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready_loose();
    tui.submit("past chat seed");
    tui.wait_for_text("past chat response");
    tui.submit("/new");
    tui.wait_for_text("Started a new session");

    let open_past = |tui: &mut Tui, title: &str| {
        tui.submit("/resume");
        let mut view = tui.wait_viewport("session picker", |view| view.contains("sessions"));
        for _ in 0..8 {
            if view
                .lines()
                .any(|line| line.contains(&format!("> {title}")))
            {
                break;
            }
            let selected = view
                .lines()
                .find(|line| line.contains("> "))
                .expect("selected session row")
                .to_string();
            tui.key(DOWN);
            view = tui.wait_viewport("session selection moved", |view| {
                view.lines()
                    .any(|line| line.contains("> ") && line != selected)
            });
        }
        assert!(
            view.lines()
                .any(|line| line.contains(&format!("> {title}"))),
            "past chat is selected:\n{view}"
        );
        tui.key(b"m");
        tui.wait_viewport("session actions", |view| view.contains("Session actions"));
    };

    open_past(&mut tui, "past chat seed");
    tui.enter();
    tui.wait_viewport_text("Type new session title and press Enter");
    tui.type_slow("Renamed past chat");
    tui.wait_viewport_text("Renamed past chat");
    tui.enter();
    tui.wait_for_text("Renamed session");

    open_past(&mut tui, "Renamed past chat");
    tui.key(DOWN);
    tui.enter();
    tui.key(ESC);
    tui.wait_for_text("Cloned session");

    open_past(&mut tui, "Renamed past chat");
    tui.key(DOWN);
    tui.key(DOWN);
    tui.enter();
    tui.key(ESC);
    tui.wait_for_text("past chat seed");

    open_past(&mut tui, "Renamed past chat");
    for _ in 0..4 {
        tui.key(DOWN);
    }
    tui.enter();
    tui.wait_viewport("delete confirmation", |view| {
        view.contains("Confirm delete")
    });
    tui.enter();
    tui.key(ESC);
    tui.wait_for_text("Deleted session");
}

#[test]
fn resume_picker_search_finds_and_switches_to_prior_chat() {
    let provider = ScriptedProvider::start(vec![
        Step::text("searchable replay"),
        Step::text("new chat"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready_loose();
    tui.submit("unique searchable session");
    tui.wait_for_text("searchable replay");
    tui.submit("/new");
    tui.wait_for_text("Started a new session");

    tui.submit("/resume");
    tui.wait_viewport("resume picker", |view| view.contains("sessions"));
    tui.key(b"/");
    tui.type_text("unique searchable session");
    tui.enter();
    tui.wait_viewport("filtered session row", |view| {
        view.contains("unique searchable session")
    });
    tui.enter();
    tui.wait_for_text("Switched to session");
    // CG-A01: /new and /resume are local commands. Prove the resumed
    // conversation by submitting the second provider request explicitly.
    tui.submit("continue the searchable chat");
    tui.wait_for_text("new chat");
    assert_eq!(provider.request_count(), 2);
    let users = user_texts(&provider.requests()[1]);
    assert!(
        users
            .iter()
            .any(|text| text.contains("unique searchable session")),
        "{users:?}"
    );
    assert!(
        users
            .last()
            .unwrap()
            .contains("continue the searchable chat"),
        "{users:?}"
    );
}

// A021: `[tui.keymap]` in config.toml rebinds the sidebar key; the old key stops working.
#[test]
fn config_toml_tui_keymap_override_rebinds_a_key() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            extra_config: "\n[tui.keymap]\nsidebar = \"f3\"\n",
            ..Default::default()
        },
    );
    tui.ready();

    tui.send(b"\x02"); // Ctrl+B: the shipped default, now unbound
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let unbound = tui.viewport();
    assert!(
        !unbound.contains("Enter switch"),
        "the default key must not open the sidebar once it is rebound:\n{unbound}"
    );

    tui.send(b"\x1bOR"); // F3
    let sidebar = tui.wait_viewport("sidebar on the rebound key", |view| {
        view.contains("Projects") && view.contains("Enter switch")
    });
    assert!(sidebar.contains("Projects"), "{sidebar}");
    tui.key(ESC);
    tui.wait_viewport("sidebar closed", |view| {
        view.contains("Ask anything") && !view.contains("Enter switch")
    });
}

// A088: `/worktree` adds, lists and removes a real git worktree through the host.
#[test]
fn worktree_command_adds_lists_and_removes_a_real_worktree() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    let target = tui.home.path().join("wt-feature-x");
    let target_arg = target.to_string_lossy().replace('\\', "/");

    let worktrees = |tui: &Tui| -> String {
        let out = rightkit_child_process::command_std("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(tui.workspace.path())
            .output()
            .expect("git worktree list");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let wait_git = |tui: &Tui, what: &str, pred: &dyn Fn(&str) -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let listing = worktrees(tui);
            if pred(&listing) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}:\n{listing}\n{}",
                tui.screen_text()
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };

    // The long typed line must clear the paste-burst window before Enter, or Enter is swallowed.
    // Type the command token with a human pause before its arguments: one fast burst
    // "/worktree add ..." has been observed to reach the dispatcher as "/worktree
    // orktree add ..." (slash popup completion racing the paste-burst buffer).
    tui.type_slow("/worktree");
    tui.type_slow(&format!(" add feature-x HEAD {target_arg}"));
    tui.wait_viewport_text("wt-feature-x");
    tui.key(b"\r");
    wait_git(&tui, "git to list the new worktree", &|l| {
        l.contains("wt-feature-x")
    });
    assert!(
        target.join("README.md").exists(),
        "the worktree is checked out"
    );

    tui.submit("/worktree");
    tui.wait_for("worktree listing in the transcript", |screen| {
        screen.matches("wt-feature-x").count() >= 1 && screen.contains("feature-x")
    });

    tui.type_slow("/worktree");
    tui.type_slow(&format!(" remove {target_arg}"));
    tui.wait_viewport_text("wt-feature-x");
    tui.key(b"\r");
    wait_git(&tui, "git to drop the removed worktree", &|l| {
        !l.contains("wt-feature-x")
    });
    assert_eq!(provider.request_count(), 0, "/worktree is a local command");
}
