//! PTY journeys for transcript paging/search and workspace diff inspection.

use crate::support::tui_harness as h;
use h::*;

#[test]
fn ctrl_t_opens_searchable_transcript_and_escape_returns_to_chat() {
    let provider = ScriptedProvider::start(vec![Step::text("TRANSCRIPT-ANSWER-MARKER")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("TRANSCRIPT-USER-MARKER");
    tui.wait_for_text("TRANSCRIPT-ANSWER-MARKER");

    tui.send(b"\x14"); // Ctrl+T
    let transcript = tui.wait_viewport("transcript reader", |view| {
        view.contains("Transcript")
            && view.contains("TRANSCRIPT-USER-MARKER")
            && view.contains("TRANSCRIPT-ANSWER-MARKER")
    });
    assert!(
        transcript.contains("Ctrl+F find"),
        "search affordance missing:\n{transcript}"
    );
    tui.send(b"\x06"); // Ctrl+F opens the find bar
    tui.type_text("ANSWER-MARKER");
    tui.enter();
    let searched = tui.wait_viewport("transcript search result", |view| {
        view.contains("Find: ANSWER-MARKER") && view.contains("1/1")
    });
    assert!(
        searched.contains("TRANSCRIPT-ANSWER-MARKER"),
        "search result not visible:\n{searched}"
    );
    // Esc is two-step: the first closes the Find bar (the reader stays open),
    // the second closes the reader and restores chat.
    tui.key(ESC);
    tui.wait_viewport("find bar closed, reader still open", |view| {
        view.contains("Transcript") && !view.contains("Find: ANSWER-MARKER")
    });
    tui.key(ESC);
    tui.wait_viewport("chat restored", |view| view.contains("Ask anything"));
}

#[test]
fn diff_command_opens_searchable_actual_workspace_diff() {
    let provider = ScriptedProvider::start(vec![]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            files: &[("README.md", "inspection baseline\n")],
            ..Default::default()
        },
    );
    std::fs::write(
        tui.workspace.path().join("README.md"),
        "inspection changed\n",
    )
    .expect("make a workspace diff");

    tui.ready();
    tui.submit("/diff");
    let diff = tui.wait_viewport("workspace diff viewer", |view| {
        view.contains("Workspace diff")
            && view.contains("inspection baseline")
            && view.contains("inspection changed")
    });
    assert!(
        diff.contains("/ search"),
        "diff search affordance missing:\n{diff}"
    );
    tui.send(b"/");
    tui.type_text("inspection changed");
    tui.enter();
    let searched = tui.wait_viewport("diff search result", |view| {
        view.contains("· /inspection changed") && view.contains("1/1 matches")
    });
    assert!(
        searched.contains("inspection changed"),
        "changed line not visible:\n{searched}"
    );
    tui.key(ESC);
    tui.wait_viewport("chat restored after diff", |view| {
        view.contains("Ask anything")
    });
    assert!(
        provider.requests().is_empty(),
        "/diff must use workspace state without asking the model"
    );
}

// A plain TUI session has no client-owned browser surface (WKWebView), so the
// `browser_control_*` tools must not be offered to the model; ordinary tools still are.
#[test]
fn client_browser_control_tools_are_not_offered_in_a_tui_session() {
    let provider = ScriptedProvider::start(vec![Step::text("tool list inspected")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("which tools do you have");
    tui.wait_for_text("tool list inspected");

    let requests = provider.requests();
    let names: Vec<String> = requests[0]["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        names.iter().any(|name| name == "read_file"),
        "ordinary tools are offered: {names:?}"
    );
    let leaked: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with("browser_control_"))
        .collect();
    assert!(
        leaked.is_empty(),
        "no client browser surface is attached, yet the model was offered {leaked:?}"
    );
}
