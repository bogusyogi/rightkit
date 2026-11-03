//! TUI end-to-end: composer behaviour as a user experiences it — slash
//! commands, multi-line and pasted input, editing keys, history, mode keys,
//! `!` shell, external editor. Real binary in a PTY; the scripted provider
//! records what the MODEL receives.

use crate::support::tui_harness as h;
use h::*;
use std::time::Duration;

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// The text the composer currently holds (rows between the two rules above
/// the footer, prompt marker stripped).
fn composer_text(view: &str) -> String {
    let lines: Vec<&str> = view.lines().collect();
    let rules: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with('─'))
        .map(|(i, _)| i)
        .collect();
    let (start, end) = match rules.as_slice() {
        [.., a, b] => (*a + 1, *b),
        _ => return String::new(),
    };
    lines[start..end]
        .iter()
        .map(|l| {
            l.strip_prefix('▌')
                .unwrap_or(l)
                .trim_start_matches(' ')
                .trim_end()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// 1. Slash popup: opens on `/`, filters by prefix, Esc closes it but keeps the
//    text, `/help` is a local command that never reaches the model.
#[test]
fn slash_popup_filters_closes_on_esc_and_help_stays_local() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();

    tui.type_slow("/");
    let all = tui.wait_viewport_text("/new");
    for cmd in ["/new", "/reset", "/undo"] {
        assert!(all.contains(cmd), "popup lists {cmd}:\n{all}");
    }
    tui.type_slow("he");
    let narrowed = tui.wait_viewport("popup narrowed to /help", |v| {
        v.contains("> /help") && !v.contains("/clear")
    });
    assert!(narrowed.contains("Show available commands"), "{narrowed}");

    tui.key(ESC);
    let closed = tui.wait_viewport("popup closed", |v| {
        !v.contains("> /help") && composer_text(v) == "/he"
    });
    assert_eq!(
        composer_text(&closed),
        "/he",
        "Esc closes the popup, never the text:\n{closed}"
    );

    tui.key(BACKSPACE);
    tui.key(BACKSPACE);
    tui.key(BACKSPACE);
    tui.submit("/help");
    let help = tui.wait_for_text("/quit");
    assert!(
        help.contains("Show available commands") && help.contains("Quit CodeRight"),
        "{help}"
    );
    pause(500);
    assert_eq!(
        provider.request_count(),
        0,
        "a slash command is local and never sent to the model"
    );
}

// 2. Words that merely look like commands are ordinary prompts.
#[test]
fn bare_command_words_and_unknown_slashes_are_sent_as_prompts() {
    let provider = ScriptedProvider::start(vec![Step::text("reply-a"), Step::text("reply-b")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("help");
    tui.wait_for_text("reply-a");
    tui.submit("/definitely-not-a-command");
    tui.wait_for_text("reply-b");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(last_user_text(&requests[0]).contains("help"));
    assert!(last_user_text(&requests[1]).contains("/definitely-not-a-command"));
}

// 3. Typing "/pla" and Enter in ONE terminal write still runs /plan.
#[test]
fn slash_prefix_and_enter_in_one_write_activates_plan_mode() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.type_text("/pla\r");
    let view = tui.wait_viewport("plan mode in footer", |v| {
        v.contains("Plan") && v.contains("e2e-branch")
    });
    assert_eq!(
        composer_text(&view).trim(),
        "Ask anything · @ to attach files · / for commands",
        "{view}"
    );
    assert_eq!(provider.request_count(), 0);
}

// 4. Shift+Tab cycles the permission mode shown in the footer without touching the draft.
#[test]
fn shift_tab_cycles_permission_modes_and_keeps_the_draft() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.type_slow("draft stays");
    let mut seen = Vec::new();
    for expected in ["Accept edits", "Auto", "Yolo", "Plan", "Ask"] {
        tui.key(BACKTAB);
        let v = tui.wait_viewport("permission mode rendered", |v| {
            v.lines().last().unwrap_or_default().contains(expected)
        });
        let footer = v.lines().last().unwrap_or_default().to_string();
        for mode in ["Accept edits", "Auto", "Yolo", "Plan", "Ask"] {
            if footer.contains(mode) {
                seen.push(mode);
            }
        }
        assert_eq!(
            composer_text(&v),
            "draft stays",
            "mode cycling must not touch the draft:\n{v}"
        );
    }
    assert_eq!(
        seen,
        ["Accept edits", "Auto", "Yolo", "Plan", "Ask"],
        "full cycle returns to Ask"
    );
}

// 5. Multi-line input: Alt+Enter and a bracketed paste both keep newlines and
//    do not submit; the model receives the exact text.
#[test]
fn multiline_alt_enter_and_bracketed_paste_reach_the_model_intact() {
    let provider = ScriptedProvider::start(vec![Step::text("got it")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.type_slow("first line");
    tui.key(ALT_ENTER);
    tui.type_slow("second line");
    let view = tui.wait_viewport("second line rendered", |v| {
        composer_text(v) == "first line\nsecond line"
    });
    assert_eq!(composer_text(&view), "first line\nsecond line", "{view}");
    assert_eq!(provider.request_count(), 0, "Alt+Enter must not submit");

    tui.key(ALT_ENTER);
    tui.paste("pasted a\npasted b\npasted c");
    let view = tui.wait_viewport("complete bracketed paste rendered", |v| {
        composer_text(v) == "first line\nsecond line\npasted a\npasted b\npasted c"
    });
    assert_eq!(
        composer_text(&view),
        "first line\nsecond line\npasted a\npasted b\npasted c",
        "paste newlines are content, not submits:\n{view}"
    );
    assert_eq!(provider.request_count(), 0);

    tui.enter();
    tui.wait_for_text("got it");
    let sent = last_user_text(&provider.requests()[0]);
    assert!(
        sent.contains("first line\nsecond line\npasted a\npasted b\npasted c"),
        "model must receive the exact multi-line text: {sent:?}"
    );
}

// 6. Readline-style editing: movement, word/line kills and yank.
#[test]
fn editing_keys_move_kill_and_yank() {
    let provider = ScriptedProvider::start(vec![Step::text("edited ok")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.type_slow("alpha beta gamma");
    tui.wait_viewport_text("alpha beta gamma");
    tui.key(CTRL_W); // kill previous word
    let killed = tui.wait_viewport("word killed", |v| composer_text(v) == "alpha beta");
    assert_eq!(composer_text(&killed), "alpha beta");
    tui.key(CTRL_Y); // yank it back
    tui.wait_viewport_text("alpha beta gamma");
    tui.key(CTRL_A);
    tui.type_slow(">> ");
    tui.key(CTRL_E);
    tui.type_slow(" <<");
    let edited = tui.wait_viewport("suffix rendered", |v| {
        composer_text(v) == ">> alpha beta gamma <<"
    });
    assert_eq!(composer_text(&edited), ">> alpha beta gamma <<");
    tui.key(LEFT); // between the two '<'
    tui.key(LEFT); // between the space and "<<"
    tui.key(BACKSPACE); // removes the space before "<<"
    tui.key(CTRL_K); // kill to end of line
                     // Wait for the settled frame, not the first frame that lost "<<": the
                     // repaint of a kill is not atomic on the terminal stream.
    let killed = tui.wait_viewport("kill to eol", |v| composer_text(v) == ">> alpha beta gamma");
    assert_eq!(composer_text(&killed), ">> alpha beta gamma");
    tui.key(CTRL_U);
    assert_eq!(
        composer_text(&tui.wait_viewport("kill to bol", |v| !v.contains("alpha"))).trim(),
        "Ask anything · @ to attach files · / for commands"
    );
    tui.key(CTRL_Y);
    tui.wait_viewport_text(">> alpha beta gamma");
    tui.enter();
    tui.wait_for_text("edited ok");
    assert!(last_user_text(&provider.requests()[0]).contains(">> alpha beta gamma"));
}

// 7. History: Up/Down recall, Ctrl+R reverse search.
#[test]
fn history_up_down_and_ctrl_r_search() {
    let provider =
        ScriptedProvider::start(vec![Step::text("r1"), Step::text("r2"), Step::text("r3")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    for (q, r) in [("question alpha", "r1"), ("question bravo", "r2")] {
        tui.submit(q);
        tui.wait_for_text(r);
        tui.wait_viewport("idle", |v| !v.contains("esc cancel"));
    }
    tui.key(UP);
    tui.wait_viewport("newest recalled", |v| composer_text(v) == "question bravo");
    tui.key(UP);
    tui.wait_viewport("older recalled", |v| composer_text(v) == "question alpha");
    tui.key(DOWN);
    tui.wait_viewport("back to newer", |v| composer_text(v) == "question bravo");
    tui.key(DOWN);
    let empty = tui.wait_viewport("back to empty draft", |v| v.contains("Ask anything"));
    assert!(empty.contains("Ask anything"), "{empty}");

    tui.key(CTRL_R);
    tui.type_slow("alph");
    let search = tui.wait_viewport("reverse search shows match", |v| {
        v.contains("reverse-i-search")
    });
    assert!(search.contains("question alpha"), "{search}");
    tui.enter(); // accept the match into the composer
    tui.wait_viewport("match accepted", |v| {
        composer_text(v).starts_with("question alpha") && !v.contains("reverse-i-search")
    });
    tui.enter();
    tui.wait_for_text("r3");
    assert!(last_user_text(&provider.requests()[2]).contains("question alpha"));
}

// 8. History survives a restart (same home), Up on an empty composer recalls it.
#[test]
fn history_persists_across_relaunch() {
    let provider = ScriptedProvider::start(vec![Step::text("persisted reply")]);
    let mut first = Tui::launch(&provider, &[]);
    first.ready();
    first.submit("remember this prompt");
    first.wait_for_text("persisted reply");
    pause(500);
    first.kill();

    let provider2 = ScriptedProvider::start(vec![]);
    let mut second = Tui::launch_with(
        &provider2,
        LaunchOptions {
            reuse: Some((
                first.home.path().to_path_buf(),
                first.workspace.path().to_path_buf(),
            )),
            ..Default::default()
        },
    );
    second.ready();
    second.key(UP);
    let view = second.wait_viewport("history recalled after restart", |v| {
        composer_text(v) == "remember this prompt"
    });
    assert!(
        !view.contains("persisted reply"),
        "a new run starts with a clean transcript:\n{view}"
    );
}

// 9. `!cmd` runs locally, shows its output, and the NEXT model request carries it.
#[test]
fn bang_command_runs_locally_and_feeds_the_next_request() {
    let provider = ScriptedProvider::start(vec![Step::text("saw the shell output")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("!echo bang-marker-4242");
    let screen = tui.wait_for("shell output in transcript", |s| {
        s.contains("$ echo bang-marker-4242") && s.contains("[exit 0]")
    });
    assert!(
        screen.lines().any(|l| l.trim() == "bang-marker-4242"),
        "{screen}"
    );
    assert_eq!(
        provider.request_count(),
        0,
        "a ! command alone does not call the model"
    );

    tui.submit("what did it print");
    tui.wait_for_text("saw the shell output");
    let sent = last_user_text(&provider.requests()[0]);
    assert!(
        sent.contains("<user_shell_command>") && sent.contains("bang-marker-4242"),
        "{sent}"
    );
    assert!(sent.contains("what did it print"), "{sent}");
}

// 10. Ctrl+C on a running turn cancels it and keeps the typed draft.
#[test]
fn ctrl_c_cancels_the_running_turn_and_keeps_the_draft() {
    let provider =
        ScriptedProvider::start(vec![
            Step::text("streaming-part-one streaming-part-two").held()
        ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("long running question");
    tui.wait_requests(&provider, 1);
    tui.wait_viewport("turn in flight", |v| v.contains("esc cancel"));
    tui.type_slow("unsent draft");
    tui.key(CTRL_C);
    // Settled frame: the working row and its reserved composer row are gone and the
    // draft is the only composer content (no trailing blank row).
    let view = tui.wait_viewport("turn cancelled", |v| {
        !v.contains("esc cancel") && composer_text(v) == "unsent draft"
    });
    assert_eq!(
        composer_text(&view),
        "unsent draft",
        "draft survives cancel:\n{view}"
    );
    provider.release(0);
}

// 11. Ctrl+G opens the external editor; its result replaces the composer text.
#[test]
fn ctrl_g_external_editor_replaces_the_draft() {
    let provider = ScriptedProvider::start(vec![Step::text("editor text received")]);
    let dir = tempfile::TempDir::new().unwrap();
    let (script, editor_env) = if cfg!(windows) {
        let p = dir.path().join("edit.cmd");
        std::fs::write(
            &p,
            "@echo off\r\n> %1 echo text from the external editor\r\n",
        )
        .unwrap();
        (p.clone(), p.to_string_lossy().to_string())
    } else {
        let p = dir.path().join("edit.sh");
        std::fs::write(
            &p,
            "#!/bin/sh\nprintf 'text from the external editor' > \"$1\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (p.clone(), p.to_string_lossy().to_string())
    };
    assert!(script.exists());
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            env: &[("VISUAL", &editor_env), ("EDITOR", &editor_env)],
            ..Default::default()
        },
    );
    tui.ready();
    tui.type_slow("will be replaced");
    tui.key(b"\x07"); // Ctrl+G
    let view = tui.wait_viewport("editor text in composer", |v| {
        v.contains("text from the external editor")
    });
    assert!(!view.contains("will be replaced"), "{view}");
    tui.enter();
    tui.wait_for_text("editor text received");
    assert!(last_user_text(&provider.requests()[0]).contains("text from the external editor"));
}

// 12. `?` is literal input; F1 toggles the key help.
#[test]
fn question_mark_is_literal_and_f1_toggles_key_help() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.key(F1);
    let help = tui.wait_viewport("key help footer", |v| v.contains("Shift+Tab mode"));
    assert!(
        help.contains("Enter queue/send") && help.contains("Esc cancel"),
        "{help}"
    );
    tui.type_slow("?");
    let view = tui.wait_viewport("? typed literally, help hidden", |v| {
        composer_text(v) == "?"
    });
    assert!(
        !view.contains("Shift+Tab mode"),
        "typing dismisses the help:\n{view}"
    );
}

// One command, several names: the popup labels the alias being typed with the
// canonical action in brackets, and every name runs the same action.
#[test]
fn popup_shows_alias_with_canonical_action_and_clear_starts_a_new_session() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();

    tui.type_slow("/e");
    let exit = tui.wait_viewport("exit row with (quit)", |v| {
        v.contains("exit") && v.contains("(quit)")
    });
    assert!(exit.contains("/exit"), "{exit}");
    tui.key(ESC);
    tui.key(BACKSPACE);
    tui.key(BACKSPACE);

    tui.type_slow("/cl");
    let clear = tui.wait_viewport("clear row with (new)", |v| {
        v.contains("clear") && v.contains("(new)")
    });
    assert!(clear.contains("/clear"), "{clear}");
    tui.key(ESC);
    for _ in 0..3 {
        tui.key(BACKSPACE);
    }

    tui.submit("/clear");
    tui.wait_for_text("Started a new session");
    assert_eq!(provider.request_count(), 0, "/clear is local");
}

// A020: `/vim` turns modal editing on (starting in Normal mode) and off again.
#[test]
fn vim_toggle_enables_modal_editing() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();

    tui.submit("/vim on");
    tui.wait_for_text("vim updated");
    tui.wait_viewport("composer idle after /vim on", |v| {
        v.contains("Ask anything")
    });

    // Normal mode: `i` enters Insert, typing inserts, Esc returns to Normal.
    tui.key(b"i");
    tui.type_slow("abc");
    tui.wait_viewport("typed in insert mode", |v| composer_text(v) == "abc");
    tui.key(ESC);
    tui.key(b"0"); // line start
    tui.key(b"x"); // delete the char under the cursor
    tui.wait_viewport("x deleted the first char in normal mode", |v| {
        composer_text(v) == "bc"
    });
    // `l` moves right, `h` moves left: x after `l` removes the second char.
    tui.key(b"l");
    tui.key(b"x");
    tui.wait_viewport("l then x deleted the second char", |v| {
        composer_text(v) == "b"
    });

    // Back to insert mode, clear the draft and turn the mode off again.
    tui.key(b"i");
    tui.key(CTRL_E);
    tui.key(CTRL_U);
    tui.wait_viewport("draft cleared", |v| v.contains("Ask anything"));
    tui.type_slow("/vim off");
    tui.enter();
    tui.wait_for("second vim confirmation", |s| {
        s.matches("vim updated").count() >= 2
    });

    // Off: Esc with text does nothing and letters are plain text (no Normal mode).
    tui.type_slow("hello");
    tui.key(ESC);
    tui.type_slow("x");
    tui.wait_viewport("plain editing after vim off", |v| {
        composer_text(v) == "hellox"
    });
    assert_eq!(provider.request_count(), 0);
}

// A032: the composer's undo/redo ledger is the Vim transaction history
// (`u` / Ctrl+R). Ctrl+Z is process suspend on Unix, never composer undo.
#[test]
fn vim_undo_and_redo_restore_composer_text() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("/vim on");
    tui.wait_for_text("vim updated");

    tui.key(b"i");
    tui.type_slow("abc");
    tui.key(ESC);
    tui.key(b"0");
    tui.key(b"x");
    tui.wait_viewport("edit applied", |v| composer_text(v) == "bc");
    tui.key(b"u");
    tui.wait_viewport("undo restores the deleted char", |v| {
        composer_text(v) == "abc"
    });
    tui.key(CTRL_R);
    tui.wait_viewport("redo re-applies the delete", |v| composer_text(v) == "bc");
}

// A017: a pasted image file path is attached to the next turn as an image.
#[test]
fn image_file_path_paste_attaches_image_to_next_turn() {
    let provider = ScriptedProvider::start(vec![Step::text("image received")]);
    let dir = tempfile::TempDir::new().unwrap();
    let image = dir.path().join("pasted-shot.png");
    let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
    std::fs::write(&image, png).unwrap();

    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.paste(&image.to_string_lossy());
    tui.wait_viewport("pasted path in the composer", |v| {
        v.contains("pasted-shot.png")
    });
    tui.type_slow(" what is in this picture");
    tui.enter();
    tui.wait_for_text("image received");

    let requests = provider.requests();
    let sent = requests[0].to_string();
    assert!(
        sent.contains("image/png"),
        "the pasted image path must reach the provider as an image attachment: {sent}"
    );
    assert!(sent.contains("what is in this picture"), "{sent}");
}
