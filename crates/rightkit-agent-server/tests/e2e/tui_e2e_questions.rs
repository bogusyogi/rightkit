//! TUI end-to-end: agent questions (`ask_user`) render as a card like the
//! approval card, and Esc declines the question without killing the turn.

use crate::support::tui_harness as h;
use h::*;

fn tool_results(request: &Value) -> Vec<String> {
    messages(request)
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(content_text)
        .collect()
}

fn question_args() -> Value {
    json!({"questions": [{
        "question": "Which database should the service use?",
        "header": "Database",
        "options": [
            {"label": "Postgres", "description": "Relational, battle tested"},
            {"label": "SQLite", "description": "Embedded, zero setup"}
        ]
    }]})
}

fn has_question_card(view: &str) -> bool {
    view.contains("Which database should the service use?") && view.contains("Type my own answer")
}

fn row_selected(view: &str, n: u8) -> bool {
    view.lines().any(|l| l.contains(&format!("› {n}  ")))
}

// 1. The card lists numbered options WITH their descriptions plus a free-text row.
#[test]
fn question_card_shows_numbered_options_with_descriptions_and_other_row() {
    let provider = ScriptedProvider::start(vec![
        Step::tool("q1", "ask_user", question_args()),
        Step::text("db chosen"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("set up storage");
    let card = tui.wait_viewport("question card", has_question_card);
    for needle in [
        "1  Postgres",
        "Relational, battle tested",
        "2  SQLite",
        "Embedded, zero setup",
        "3  Type my own answer",
        "esc decline",
    ] {
        assert!(card.contains(needle), "card missing {needle:?}:\n{card}");
    }
    assert!(
        row_selected(&card, 1),
        "first option is preselected:\n{card}"
    );
}

// 2. Arrow keys move the highlight; Enter confirms; the model receives the label.
#[test]
fn arrow_then_enter_answers_with_the_option_label() {
    let provider = ScriptedProvider::start(vec![
        Step::tool("q2", "ask_user", question_args()),
        Step::text("sqlite it is"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("set up storage");
    tui.wait_viewport("question card", has_question_card);
    tui.send(DOWN);
    tui.wait_viewport("option 2 highlighted", |v| row_selected(v, 2));
    tui.enter();
    tui.wait_for_text("sqlite it is");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let results = tool_results(&requests[1]).join("\n");
    assert!(results.contains("Database: SQLite"), "{results}");
}

// 3. A digit picks that option directly.
#[test]
fn digit_picks_an_option() {
    let provider = ScriptedProvider::start(vec![
        Step::tool("q3", "ask_user", question_args()),
        Step::text("postgres it is"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("set up storage");
    tui.wait_viewport("question card", has_question_card);
    tui.send(b"1");
    tui.wait_for_text("postgres it is");
    let results = tool_results(&provider.requests()[1]).join("\n");
    assert!(results.contains("Database: Postgres"), "{results}");
}

// 4. "Type my own answer": typing free text and Enter sends exactly that text.
#[test]
fn typed_own_answer_reaches_the_model() {
    let provider = ScriptedProvider::start(vec![
        Step::tool("q4", "ask_user", question_args()),
        Step::text("custom noted"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("set up storage");
    tui.wait_viewport("question card", has_question_card);
    tui.send(DOWN);
    tui.send(DOWN);
    tui.wait_viewport("other row highlighted", |v| row_selected(v, 3));
    tui.type_text("DuckDB please");
    tui.wait_viewport_text("Answer: DuckDB please");
    tui.enter();
    tui.wait_for_text("custom noted");
    let results = tool_results(&provider.requests()[1]).join("\n");
    assert!(results.contains("Database: DuckDB please"), "{results}");
}

// 5. Esc declines: the model is told, the turn is NOT cancelled and continues.
#[test]
fn esc_declines_the_question_and_the_turn_continues() {
    let provider = ScriptedProvider::start(vec![
        Step::tool("q5", "ask_user", question_args()),
        Step::text("proceeding with a default"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("set up storage");
    tui.wait_viewport("question card", has_question_card);
    tui.key(ESC);
    tui.wait_for_text("proceeding with a default");
    let screen = tui.screen_text();
    assert!(
        !screen.contains("Turn cancelled"),
        "Esc must not kill the turn:\n{screen}"
    );
    assert!(!has_question_card(&tui.viewport()), "{screen}");
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "the turn continued to a second model call"
    );
    let results = tool_results(&requests[1]).join("\n");
    assert!(
        results.contains("declined"),
        "the model is told the user declined: {results}"
    );
}
