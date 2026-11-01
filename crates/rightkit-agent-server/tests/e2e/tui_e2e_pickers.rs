//! TUI end-to-end: in-place pickers (`/model`, `/resume`) and session replay.

use crate::support::tui_harness as h;
use h::*;
use std::time::Duration;

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

// /model opens the provider-first picker (both levels end in "Esc cancel"/"Esc back"); Esc
// dismisses it with the route untouched and the next request still names the configured model.
#[test]
fn model_picker_opens_and_esc_keeps_the_current_route() {
    let provider = ScriptedProvider::start(vec![Step::text("model-picker-answer")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("/model");
    let view = tui.wait_viewport("model picker", |v| v.contains("Esc cancel"));
    println!("{view}");
    tui.key(ESC);
    let view = tui.wait_viewport("picker dismissed", |v| {
        !v.contains("Esc cancel") && v.contains("Ask anything")
    });
    assert!(
        view.contains(MODEL),
        "model unchanged after Esc:
{view}"
    );
    tui.submit("which model");
    tui.wait_for_text("model-picker-answer");
    assert_eq!(
        provider.requests()[0]["model"],
        MODEL,
        "request carries the configured model"
    );
}

// KRN-001/KRN-002: inline hosting owns durable sessions. Selecting the current
// titled session preserves its transcript and the next model conversation.
#[test]
fn resume_in_the_inline_host_keeps_the_current_conversation() {
    let provider = ScriptedProvider::start(vec![
        Step::text("first-answer"),
        Step::text("second-answer"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("remember the walrus");
    tui.wait_for_text("first-answer");
    tui.submit("/resume");
    // The durable title supersedes the old synthetic "this session" row.
    tui.wait_viewport("session picker lists this session", |v| {
        v.contains("sessions") && v.contains("remember the walrus")
    });
    tui.key(b"\r");
    let screen = tui.wait_for_text("Already on this session");
    assert!(
        screen.contains("remember the walrus") && screen.contains("first-answer"),
        "transcript kept:
{screen}"
    );
    tui.submit("and now?");
    tui.wait_for_text("second-answer");
    let users = user_texts(&provider.requests()[1]);
    assert!(
        users.iter().any(|u| u.contains("remember the walrus"))
            && users.last().unwrap().contains("and now?"),
        "{users:?}"
    );
}

// Reasoning recorded in a session is replayed as a collapsed Thought row ahead of the answer.
#[test]
fn resume_replays_the_reasoning_row_before_the_answer() {
    let provider = ScriptedProvider::start(vec![Step::text("replay-answer-body")
        .with_reasoning("**Weighing the replay options** carefully")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("think then answer");
    tui.wait_for("source Thought row", |s| {
        s.contains("Thought")
            && s.contains("Weighing the replay options")
            && s.contains("replay-answer-body")
    });

    // Create a second real session, then select the first from the session picker.
    // This forces restoration through TurnHost::switch_session and durable history.
    tui.submit("/new");
    tui.wait_for_text("Started a new session");
    tui.wait_viewport("new session composer", |v| v.contains("Ask anything"));
    tui.submit("/resume");
    let picker = tui.wait_viewport("session picker with source turn", |v| {
        v.contains("sessions") && v.contains("think then answer")
    });
    if !picker
        .lines()
        .any(|line| line.contains("> ") && line.contains("think then answer"))
    {
        tui.key(DOWN);
    }
    tui.enter();

    let screen = tui.wait_for("replayed Thought row and answer", |s| {
        s.contains("Thought")
            && s.contains("Weighing the replay options")
            && s.contains("replay-answer-body")
    });
    let thought = screen
        .lines()
        .position(|l| l.contains("Thought") && l.contains("Weighing the replay options"))
        .unwrap_or_else(|| {
            panic!(
                "a replayed Thought row exists:
{screen}"
            )
        });
    let answer = screen
        .lines()
        .position(|l| l.contains("replay-answer-body"))
        .unwrap();
    assert!(
        thought < answer,
        "Thought row precedes the answer:
{screen}"
    );
    assert_eq!(
        screen.matches("Weighing the replay options").count(),
        1,
        "replay contains one stable Thought row:
{screen}"
    );
}

/// The text the composer holds (rows between the two rules above the footer).
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

// A018: unsent text belongs to its chat. Leaving a chat stashes the draft, the new chat starts
// empty, and coming back restores exactly what was typed.
#[test]
fn draft_is_restored_when_switching_back_to_a_session() {
    let provider = ScriptedProvider::start(vec![Step::text("alpha-chat-answer")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("alpha seed question");
    tui.wait_for_text("alpha-chat-answer");
    tui.wait_viewport("idle", |v| {
        v.contains("Ask anything") && !v.contains("esc cancel")
    });
    pause(500);

    tui.type_slow("DRAFT-ALPHA-UNSENT");
    tui.key(b"\x02"); // Ctrl+B sidebar
    tui.wait_viewport("sidebar", |v| {
        v.contains("Projects") && v.contains("Enter switch")
    });
    // The active chat is preselected; one row up is its project: Enter starts a new chat there.
    tui.send(UP);
    pause(200);
    tui.enter();
    // The project picker now reports its accepted switch through rebind_view_to_active.
    tui.wait_for("new project chat", |view| {
        view.contains("Opened project") && view.contains("new chat.")
    });
    let fresh = tui.wait_viewport("new chat composer", |v| v.contains("Ask anything"));
    assert!(
        !fresh.contains("DRAFT-ALPHA-UNSENT"),
        "the new chat must not inherit the other chat's draft:\n{fresh}"
    );

    tui.type_slow("DRAFT-BETA-UNSENT");
    tui.key(b"\x02");
    tui.wait_viewport("sidebar again", |v| {
        v.contains("Projects") && v.contains("Enter switch")
    });
    tui.type_text("alpha seed question");
    tui.wait_viewport("filtered to the first chat", |v| {
        v.contains("alpha seed question")
    });
    pause(300);
    tui.enter();
    let restored = tui.wait_viewport("first chat's draft restored", |v| {
        composer_text(v).contains("DRAFT-ALPHA-UNSENT")
    });
    assert!(
        !restored.contains("DRAFT-BETA-UNSENT"),
        "the other chat's draft must stay with that chat:\n{restored}"
    );
}

// A023 / D13: /model picks provider, model and reasoning effort; the next request uses them and a
// new chat inherits the last route.
#[test]
fn model_picker_selects_model_and_effort_and_the_next_turn_uses_them() {
    let provider = ScriptedProvider::start(vec![
        Step::text("route-one-answer"),
        Step::text("route-two-answer"),
    ]);
    let second_route = format!(
        "\n[provider-routes.qual-fixture-b]\ndisplay-name = \"Qualification fixture B\"\nbase-url = \"{}\"\nmodel = \"openai/gpt-6.1-sol\"\napi-key-required = false\n",
        provider.base_url()
    );
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            extra_config: &second_route,
            ..Default::default()
        },
    );
    tui.ready();

    tui.submit("/model");
    tui.wait_viewport("provider picker", |v| v.contains("Select provider"));
    tui.type_text("fixture B");
    pause(400);
    tui.enter();
    tui.wait_viewport("model list for the chosen provider", |v| {
        v.contains("Select model") && v.contains("gpt-6.1-sol")
    });
    tui.enter();
    let effort = tui.wait_viewport("reasoning level picker", |v| {
        v.contains("Select reasoning level")
    });
    assert!(
        effort.contains("Low") && effort.contains("High"),
        "{effort}"
    );
    tui.send(DOWN);
    pause(200);
    tui.enter();
    tui.wait_viewport("picker closed with the new route in the footer", |v| {
        !v.contains("Select reasoning level") && v.contains("gpt-6.1-sol")
    });

    tui.submit("use the new route");
    tui.wait_for_text("route-one-answer");
    let first = provider.requests()[0].clone();
    assert!(
        first["model"]
            .as_str()
            .unwrap_or("")
            .contains("gpt-6.1-sol"),
        "the request names the picked model: {}",
        first["model"]
    );
    assert!(
        first.to_string().contains("medium"),
        "the request carries the picked effort (second level): {first}"
    );

    // D13: a new chat inherits the last model + effort.
    tui.wait_viewport("idle", |v| {
        v.contains("Ask anything") && !v.contains("esc cancel")
    });
    tui.submit("/new");
    tui.wait_for_text("Started a new session");
    tui.submit("second chat question");
    tui.wait_for_text("route-two-answer");
    let second = provider.requests()[1].clone();
    assert!(
        second["model"]
            .as_str()
            .unwrap_or("")
            .contains("gpt-6.1-sol"),
        "a new chat starts on the last model: {}",
        second["model"]
    );
    assert!(
        second.to_string().contains("medium"),
        "a new chat starts on the last effort: {second}"
    );
}
