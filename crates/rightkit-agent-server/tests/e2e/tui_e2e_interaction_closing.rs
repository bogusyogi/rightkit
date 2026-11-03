//! PTY journeys for the closing interaction lane: rewind, live subagent chat,
//! and question-card descriptions/decline semantics.

use crate::support::tui_harness as h;
use h::*;
use std::time::Duration;

fn ask_user_args() -> Value {
    json!({"questions": [{
        "question": "Which database should this use?",
        "header": "Database",
        "options": [
            {"label": "Postgres", "description": "Relational, battle tested"},
            {"label": "SQLite", "description": "Embedded, zero setup"}
        ]
    }]})
}

#[test]
fn esc_twice_rewinds_conversation_and_restores_prompt_to_composer() {
    let provider = ScriptedProvider::start(vec![
        Step::text("first turn answer"),
        Step::text("second turn answer"),
        Step::text("edited first prompt answer"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("FIRST-REWIND-PROMPT");
    tui.wait_for_text("first turn answer");
    tui.submit("SECOND-REWIND-PROMPT");
    tui.wait_for_text("second turn answer");

    tui.send(ESC);
    std::thread::sleep(Duration::from_millis(80));
    tui.send(ESC);
    tui.wait_viewport("rewind prompt picker", |view| {
        view.contains("Rewind") && view.contains("FIRST-REWIND-PROMPT")
    });
    // The picker preselects the most recent message (Claude Code style); step up to the first.
    tui.send(UP);
    std::thread::sleep(Duration::from_millis(80));
    tui.enter();
    tui.wait_viewport("rewind restore choices", |view| {
        view.contains("Restore conversation only") && view.contains("Restore files only")
    });
    tui.send(DOWN);
    tui.enter();
    tui.wait_viewport("restored composer prompt", |view| {
        view.contains("FIRST-REWIND-PROMPT") && view.contains("edit it")
    });
    tui.enter();
    tui.wait_for_text("edited first prompt answer");
    let request = provider.requests().last().cloned().unwrap();
    let users = user_texts(&request);
    assert!(
        users
            .iter()
            .any(|text| text.contains("FIRST-REWIND-PROMPT")),
        "restored prompt was not resubmitted: {users:?}"
    );
    assert!(
        !users
            .iter()
            .any(|text| text.contains("SECOND-REWIND-PROMPT")),
        "rewound conversation retained later prompt: {users:?}"
    );
}

#[test]
fn live_subagent_list_opens_child_chat_and_escape_returns_to_parent() {
    let provider = ScriptedProvider::start(vec![
        Step::tool(
            "spawn-live-child",
            "spawn_subagent",
            json!({"role":"planner","task":"LIVE-CHILD-TASK: inspect plan"}),
        ),
        // Keep the child live while navigating the Running panel.
        Step::text("child answer").held(),
        Step::text("parent answer").held(),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("delegate this task");
    // Spawning a sub-agent is gated in Ask mode; allow it once.
    tui.wait_viewport("spawn approval card", |view| {
        has_card(view) && view.contains("esc reject")
    });
    tui.enter();
    let view = tui.wait_viewport("live subagent list", |view| {
        // Running-panel change renamed this surface & its focus binding.
        view.contains("Running")
            && view.contains("planner")
            && (view.contains("running") || view.contains("done"))
    });
    assert!(
        view.contains("running") || view.contains("done"),
        "status missing from live agent row:\n{view}"
    );
    tui.key(b"\x14"); // Ctrl+T focuses Running work.
    tui.enter();
    let child_chat = tui.wait_viewport("child chat", |view| {
        view.contains("LIVE-CHILD-TASK") && view.contains("planner")
    });
    assert!(
        child_chat.contains("planner"),
        "role/model context missing in child chat:\n{child_chat}"
    );
    tui.type_slow("child draft must stay with child");
    tui.wait_viewport_text("child draft must stay with child");
    tui.key(ESC);
    // CG-A03 keeps settled prompts in native scrollback; S:I7 restores
    // per-view drafts. The parent's prompt need not remain in the viewport.
    tui.wait_for("parent chat restored", |screen| {
        screen.contains("Returned to parent chat.") && screen.contains("delegate this task")
    });
    tui.wait_viewport("parent composer restored", |view| {
        view.contains("Type to queue") && !view.contains("child draft must stay with child")
    });
    // spawn_subagent settles its tool result only after the child completes
    // (S:F10). Release that response before waiting for parent continuation.
    provider.release(1);
    tui.wait_requests(&provider, 3);
    provider.release(2);
    tui.wait_for_text("parent answer");
}

#[test]
fn question_card_keeps_descriptions_and_escape_declines_only_question() {
    let provider = ScriptedProvider::start(vec![
        Step::tool("closing-question", "ask_user", ask_user_args()),
        Step::text("continued after decline"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("choose a database");
    let card = tui.wait_viewport("question card", |view| {
        view.contains("Which database should this use?") && view.contains("Type my own answer")
    });
    for text in [
        "Relational, battle tested",
        "Embedded, zero setup",
        "esc decline",
    ] {
        assert!(
            card.contains(text),
            "question card missing {text:?}:\n{card}"
        );
    }
    tui.key(ESC);
    tui.wait_for_text("continued after decline");
    let screen = tui.screen_text();
    assert!(
        !screen.contains("Turn cancelled"),
        "declining question cancelled turn:\n{screen}"
    );
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "model received continuation after decline"
    );
    let answers = messages(&requests[1])
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        answers.contains("declined"),
        "model was not told question was declined: {answers}"
    );
}
