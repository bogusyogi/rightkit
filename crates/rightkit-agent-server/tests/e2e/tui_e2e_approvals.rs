//! TUI end-to-end: approval cards and permission modes. Every decision is
//! checked twice: what the card/transcript shows AND what the model is told.

use crate::support::tui_harness as h;
use h::*;
use std::time::Duration;

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn tool_results(request: &Value) -> Vec<String> {
    messages(request)
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(content_text)
        .collect()
}

fn card_option_selected(view: &str, n: u8) -> bool {
    view.lines().any(|l| l.contains(&format!("› {n}  ")))
}

fn card_ready(view: &str) -> bool {
    // The title arrives before the remaining rows over ConPTY. Wait for the
    // final instruction row before examining content or choosing an action.
    has_card(view) && view.contains("esc reject")
}

// 1. The shell card names the command, the reason and every option; digits decide.
#[test]
fn shell_card_content_and_deny_digit_continues_the_turn_with_a_denial() {
    let provider = ScriptedProvider::start(vec![
        shell_call("call_deny", "echo deny-me-9911"),
        Step::text("ok, I will not run it"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("run something");
    let card = tui.wait_viewport("approval card", card_ready);
    for needle in [
        "Approve shell command",
        "echo deny-me-9911",
        "need approval in Ask mode",
        "1  Allow once",
        "2  Always allow: echo",
        "Installs allow rule: shell(echo)",
        "3  Allow for this session: echo deny-me-9911",
        "only this exact command",
        "4  Deny",
        "5  No, and tell",
        "esc reject",
    ] {
        assert!(card.contains(needle), "card missing {needle:?}:\n{card}");
    }
    assert!(
        card_option_selected(&card, 1),
        "Allow once is preselected:\n{card}"
    );

    tui.send(b"4");
    tui.wait_for_text("ok, I will not run it");
    let screen = tui.screen_text();
    assert!(
        screen.contains("rejected shell · echo deny-me-9911"),
        "{screen}"
    );
    assert!(!has_card(&tui.viewport()));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "Deny lets the turn continue");
    let results = tool_results(&requests[1]).join("\n");
    assert!(
        !results.contains("\ndeny-me-9911\n"),
        "a denied command must not produce output: {results}"
    );
    assert_eq!(
        tool_results(&requests[1]).len(),
        1,
        "the model is told about the denial"
    );
}

// 2. Arrow keys move the selection; Enter takes it.
#[test]
fn arrow_keys_move_the_card_selection_and_enter_decides() {
    let provider = ScriptedProvider::start(vec![
        shell_call("call_arrow", "echo arrow-choice"),
        Step::text("arrow flow done"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("go");
    tui.wait_viewport("card", card_ready);
    tui.send(DOWN);
    tui.wait_viewport("option 2 selected", |v| card_option_selected(v, 2));
    tui.send(DOWN);
    tui.wait_viewport("option 3 selected", |v| card_option_selected(v, 3));
    tui.send(UP);
    tui.wait_viewport("back on option 2", |v| card_option_selected(v, 2));
    tui.send(DOWN);
    tui.wait_viewport("option 3 selected again", |v| card_option_selected(v, 3));
    tui.enter(); // allow for this session
    tui.wait_for_text("arrow flow done");
    let screen = tui.screen_text();
    assert!(
        screen.contains("allowed shell · echo arrow-choice · this session"),
        "{screen}"
    );
}

// 3. A session grant covers exactly that command: repeating it runs without a
//    card, a different command asks again.
#[test]
fn session_grant_covers_only_the_exact_command() {
    let provider = ScriptedProvider::start(vec![
        shell_call("g1", "echo granted-one"),
        shell_call("g2", "echo granted-one"),
        shell_call("g3", "echo other-command"),
        Step::text("grant flow finished"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("run three commands");
    tui.wait_viewport("first card", card_ready);
    tui.send(b"3");
    // g2 is the same command: it must run with no card; g3 must ask again.
    tui.wait_viewport("card for the different command", |v| {
        card_ready(v) && v.contains("echo other-command")
    });
    // Judge the grant on the scrollback-inclusive screen: the open card's full-height surface
    // pushes earlier transcript rows above the visible viewport, so the viewport alone is not a
    // stable place to count them.
    let at_second_card = tui.screen_text();
    assert_eq!(
        at_second_card
            .matches("allowed shell · echo granted-one · this session")
            .count(),
        1,
        "only the first run needed (and recorded) a decision:
{at_second_card}"
    );
    tui.send(b"1");
    let screen = tui.wait_for_text("grant flow finished");
    assert!(
        screen.contains("allowed shell · echo other-command"),
        "{screen}"
    );
    assert!(
        !screen.contains("allowed shell · echo other-command · this session"),
        "{screen}"
    );
    // Scrollback-inclusive: committed transcript lines must survive in the
    // terminal's scrollback, not only while they are on the visible screen.
    assert_eq!(
        screen
            .matches("allowed shell · echo granted-one · this session")
            .count(),
        1,
        "the grant record survives in scrollback exactly once:
{screen}"
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    let last = tool_results(&requests[3]).join("\n");
    assert!(last.contains("other-command"), "{last}");
    assert_eq!(
        last.matches("granted-one").count(),
        2,
        "the granted command ran both times, only the first asked:
{last}"
    );
}

// 4. "No, and tell the model what to do instead" rejects, stops, hands the composer back.
#[test]
fn redirect_option_stops_the_turn_and_prompts_for_instructions() {
    let provider = ScriptedProvider::start(vec![
        shell_call("call_redirect", "echo never-runs"),
        Step::text("following the redirect"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("go");
    tui.wait_viewport("card", card_ready);
    tui.send(b"5");
    let view = tui.wait_viewport("redirect prompt", |v| {
        !has_card(v) && v.contains("what to do instead") && v.contains("Turn stopped")
    });
    assert!(view.contains("Turn stopped"), "{}", tui.screen_text());
    pause(1000);
    assert_eq!(
        provider.request_count(),
        1,
        "no further model call until the user redirects"
    );
    tui.submit("use a different approach");
    tui.wait_for_text("following the redirect");
    assert!(last_user_text(&provider.requests()[1]).contains("use a different approach"));
}

// 5. File changes show a diff preview in the card; allowing writes the edit row.
#[test]
fn file_change_card_previews_the_diff_and_allow_applies_it() {
    let provider = ScriptedProvider::start(vec![
        Step::tool(
            "call_w",
            "write_file",
            json!({"path": "notes.txt", "content": "one\ntwo\n"}),
        ),
        Step::text("file written"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("write the notes");
    let card = tui.wait_viewport("file card", card_ready);
    for needle in [
        "Approve file change",
        "notes.txt",
        "(+2 -0)",
        "+  1 one",
        "+  2 two",
        "1  Allow once",
        "2  Always allow: notes.txt",
        "3  Deny",
    ] {
        assert!(card.contains(needle), "card missing {needle:?}:\n{card}");
    }
    tui.enter();
    tui.wait_for_text("file written");
    let screen = tui.screen_text();
    assert!(
        screen.contains("✓ edit") && screen.contains("notes.txt") && screen.contains("+2 −0"),
        "{screen}"
    );
    assert!(
        provider.requests()[1]["messages"]
            .to_string()
            .contains("call_w"),
        "the model gets the write result"
    );
}

// 6. Esc on a file-change card rejects it and says so.
#[test]
fn esc_on_a_file_change_card_rejects_the_edit() {
    let provider = ScriptedProvider::start(vec![Step::tool(
        "call_wr",
        "write_file",
        json!({"path": "rejected.txt", "content": "nope\n"}),
    )]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("write it");
    tui.wait_viewport("file card", card_ready);
    tui.key(ESC);
    let screen = tui.wait_for("rejection recorded", |s| {
        s.contains("rejected edit · rejected.txt") && s.contains("Turn stopped")
    });
    assert!(!has_card(&tui.viewport()), "{screen}");
    assert!(!tui.workspace.path().join("rejected.txt").exists());
}

// 7. Yolo never asks; Auto/Accept-edits modes are reachable from the keyboard.
#[test]
fn yolo_runs_tools_without_a_card() {
    let provider = ScriptedProvider::start(vec![
        shell_call("y1", "echo yolo-ran-5521"),
        Step::text("yolo finished"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    let boot = tui.ready();
    assert!(boot.contains("Yolo"), "{boot}");
    tui.submit("do it");
    tui.wait_for_text("yolo finished");
    let screen = tui.screen_text();
    assert!(!screen.contains("Approve shell command"), "{screen}");
    assert!(
        screen.contains("✓ shell") && screen.contains("yolo-ran-5521"),
        "{screen}"
    );
}

// 8. Plan mode denies writes without prompting and tells the model why.
#[test]
fn plan_mode_denies_writes_and_tells_the_model() {
    let provider = ScriptedProvider::start(vec![
        Step::tool(
            "p1",
            "write_file",
            json!({"path": "plan.txt", "content": "x"}),
        ),
        Step::text("plan-mode answer"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.type_text("/plan\r");
    tui.wait_viewport("plan mode", |v| v.contains("Plan"));
    tui.submit("try to write");
    tui.wait_for_text("plan-mode answer");
    assert!(
        !tui.screen_text().contains("Approve file change"),
        "plan mode never prompts to write"
    );
    let results = tool_results(&provider.requests()[1]).join("\n");
    assert!(results.contains("denied in Plan mode"), "{results}");
}

// 9. Hostile command text cannot inject terminal controls through the card or the ledger.
#[test]
fn hostile_command_bytes_never_reach_the_terminal() {
    let provider = ScriptedProvider::start(vec![
        shell_call("h1", "Write-Output 'a'; # \u{1b}]0;PWNED-TITLE\u{7}"),
        Step::text("hostile done"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("go");
    tui.wait_viewport("card", has_card);
    tui.enter();
    tui.wait_for_text("hostile done");
    let raw = tui.raw_output();
    assert!(
        !raw.contains("\u{1b}]0;PWNED-TITLE"),
        "terminal control injected from command text"
    );
}
