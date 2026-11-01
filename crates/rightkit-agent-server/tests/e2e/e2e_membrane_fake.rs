//! Membrane e2e coverage against a fake `membrane` binary (issue #33, restoring what #26 deleted
//! with the retired in-repo membrane.exe fixture).
//!
//! The REAL `coderight` binary runs against the scripted provider and
//! `coderight-membrane-cli`'s `fake-membrane` (wired through `RIGHTKIT_MEMBRANE_BIN`, see
//! `support/fake_membrane.rs`). `RIGHTKIT_SUITE_ROOT`, `LOCALAPPDATA` and `APPDATA` are all temp dirs, so
//! no test touches the real home or an installed Pulse.
//!
//! Replaces: e2e_memory_trusted (save/recall), canon_batch_context mbr003/mbr008 and
//! e2e_prompt_cache_membrane (context pull reaches the provider), e2e_binding_qualification
//! (Pulse absent degrades without failing), e2e_adapt_learning (adapt observe on execution) and
//! the transcript export cursor discipline.

use crate::support::daemon_process;
use crate::support::e2e_cli::*;
use crate::support::fake_membrane::{append_cursors, assert_temp_rooted, FakeMembrane};

const OFF_REASON: &str = "off: pulse not installed";

fn fixture(provider: &Provider) -> (Cli, FakeMembrane) {
    let cli = Cli::new(provider);
    assert_temp_rooted(cli.home.path());
    assert_temp_rooted(cli.workspace.path());
    (cli, FakeMembrane::new())
}

/// All provider request bodies, one string each.
fn request_texts(provider: &Provider) -> Vec<String> {
    provider.requests().iter().map(|r| r.to_string()).collect()
}

// 1. A memory the model saves reaches Membrane (`memory push`) and returns in a later session's
//    turn through recall (`memory search`), as exact data inside `<workspace_memory>`.
#[test]
fn memory_save_then_recall_round_trips_through_membrane() {
    let saved = "Release freezes on Friday; never deploy after noon.";
    let provider = Provider::start(vec![
        Chat::tool("find-save", "tool_search", json!({"query": "memory_save"})).into(),
        Chat::tool(
            "save-1",
            "memory_save",
            json!({"name": "release-freeze", "content": saved, "scope": "global"}),
        )
        .into(),
        Chat::text("memory saved").into(),
        Chat::text("memory recalled").into(),
    ]);
    let (cli, fm) = fixture(&provider);
    let env = fm.env();

    let first = cli.run_env(
        &["--json-event", "--bypass-approvals"],
        "remember the release freeze",
        &env,
    );
    assert_eq!(first.code, Some(0), "save turn completes: {}", first.diag());
    assert!(first.stdout.contains("memory saved"), "{}", first.diag());

    let pushes = fm.calls_matching(&["memory", "push"]);
    assert_eq!(pushes.len(), 1, "exactly one memory push: {:?}", fm.calls());
    assert_eq!(
        pushes[0][2], saved,
        "membrane receives the saved bytes verbatim"
    );
    let stored = std::fs::read_to_string(&fm.store).expect("fake store written by push");
    assert!(
        stored.contains("Release freezes on Friday"),
        "store holds the memory: {stored}"
    );
    let requests = request_texts(&provider);
    assert!(
        !requests[0].contains("<workspace_memory>"),
        "nothing is recalled before anything was saved: {}",
        requests[0]
    );
    assert!(
        requests[2].contains("saved memory"),
        "the model sees the save succeed: {}",
        requests[2]
    );

    // A new session in the same isolated home recalls it on turn start.
    let second = cli.run_env(
        &["--json-event", "--bypass-approvals"],
        "when does the release freeze start",
        &env,
    );
    assert_eq!(
        second.code,
        Some(0),
        "recall turn completes: {}",
        second.diag()
    );
    assert!(
        !fm.calls_matching(&["memory", "search"]).is_empty(),
        "recall asks membrane: {:?}",
        fm.calls()
    );
    let recalled = request_texts(&provider)
        .pop()
        .expect("recall-turn provider request");
    assert!(
        recalled.contains("<workspace_memory>"),
        "recall block injected: {recalled}"
    );
    assert!(
        recalled.contains("Release freezes on Friday; never deploy after noon."),
        "recalled memory arrives as exact data: {recalled}"
    );
}

// 2. `membrane context pull` text is injected into the turn that reaches the provider, framed as
//    non-authoritative data.
#[test]
fn context_pull_is_injected_into_the_provider_turn() {
    let provider = Provider::start(vec![Chat::text("context-answer").into()]);
    let (cli, fm) = fixture(&provider);

    let out = cli.run_env(
        &["--json-event", "--bypass-approvals"],
        "use the project context",
        &fm.env(),
    );
    assert_eq!(out.code, Some(0), "turn completes: {}", out.diag());
    assert!(out.stdout.contains("context-answer"), "{}", out.diag());
    assert!(
        !fm.calls_matching(&["context", "pull"]).is_empty(),
        "turn pulls context from membrane: {:?}",
        fm.calls()
    );
    assert_eq!(provider.count(), 1);
    let request = provider.requests()[0].to_string();
    assert!(
        request.contains("pulse-membrane-cli"),
        "context envelope present: {request}"
    );
    assert!(
        request.contains("fake context line"),
        "pulled context reaches the model: {request}"
    );
    assert!(
        request.contains("second line"),
        "multi-line context kept whole: {request}"
    );
    assert!(
        request.contains("instructions_are_data") && request.contains("authority"),
        "context is framed as data, not authority: {request}"
    );
    assert!(
        !out.kinds().iter().any(|k| k == "contextDegraded"),
        "a present membrane is not degraded: {}",
        out.diag()
    );
}

// 3. Pulse absent: the session still runs and the typed reason is `off: pulse not installed`.
#[test]
fn pulse_absent_reports_off_reason_and_the_session_still_runs() {
    let provider = Provider::start(vec![Chat::text("absent-answer").into()]);
    let cli = Cli::new(&provider);
    assert_temp_rooted(cli.home.path());
    let fm = FakeMembrane::absent();

    let out = cli.run_env(&["--json-event", "--bypass-approvals"], "say hi", &fm.env());
    assert_eq!(out.code, Some(0), "exit 0 without Pulse: {}", out.diag());
    assert!(
        out.stdout.contains("absent-answer"),
        "the turn ran and answered: {}",
        out.diag()
    );
    assert_eq!(provider.count(), 1, "the model was called");
    let request = provider.requests()[0].to_string();
    assert!(
        !request.contains("pulse-membrane-cli"),
        "no context injected: {request}"
    );
    assert!(
        out.stdout.contains(OFF_REASON) || out.stderr.contains(OFF_REASON),
        "typed reason `{OFF_REASON}` is surfaced: {}",
        out.diag()
    );
    assert!(
        fm.calls().is_empty(),
        "no membrane process was ever started: {:?}",
        fm.calls()
    );
}

/// One failing-shell turn through a real daemon (`RIGHTKIT_RUN_HOST=service`), so the
/// asynchronous transcript/adapt exporters outlive each short-lived CLI. Returns the CLI output.
fn run_failing_shell_session(cli: &Cli, fm: &FakeMembrane, session_id: &str) -> Out {
    let daemon = daemon_process::start_daemon(cli.home.path(), &fm.env());
    let url = daemon.url();
    let token = daemon.startup.token.clone();
    let env = fm.env_with(&[
        ("RIGHTKIT_RUN_HOST", "service"),
        ("RIGHTKIT_DAEMON_URL", url.as_str()),
        ("RIGHTKIT_DAEMON_TOKEN", token.as_str()),
    ]);
    let out = cli.run_env(
        &[
            "--json-event",
            "--bypass-approvals",
            "--session-id",
            session_id,
        ],
        "run the requested check",
        &env,
    );
    assert_eq!(
        out.code,
        Some(0),
        "service session completes: {}",
        out.diag()
    );
    assert!(
        out.kinds().iter().any(|k| k == "toolResultAdded"),
        "the shell failure is durably observable: {}",
        out.diag()
    );
    // The daemon must outlive the CLI until its exporters have called membrane; it is reaped
    // (Drop) right after.
    wait_for_export(fm);
    drop(daemon);
    out
}

fn wait_for_export(fm: &FakeMembrane) {
    fm.wait_for("a committed transcript append", 45, |calls| {
        append_cursors(calls).len() >= 2
    });
    fm.wait_for("adapt observe", 45, |calls| {
        calls
            .iter()
            .any(|c| c.len() >= 2 && c[0] == "adapt" && c[1] == "observe")
    });
}

// 4. Execution observations are submitted through `membrane adapt observe`.
#[test]
fn adapt_observe_is_called_on_execution() {
    let provider = Provider::start(vec![
        Chat::tool("fail-a", "shell", json!({"command": "exit 1"})).into(),
        Chat::text("I completed successfully.").into(),
    ]);
    let (cli, fm) = fixture(&provider);
    run_failing_shell_session(&cli, &fm, "adapt-observe-session");

    let calls = fm.calls_matching(&["adapt", "observe"]);
    assert!(!calls.is_empty(), "adapt observe invoked: {:?}", fm.calls());
    let docs = fm.stdin_docs("adapt-observe");
    assert!(!docs.is_empty(), "observe request body recorded");
    let doc = &docs[0];
    assert_eq!(doc["operation"], "analyze", "{doc}");
    assert_eq!(doc["sessionId"], "adapt-observe-session", "{doc}");
    assert!(
        doc["observations"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "execution observations are submitted: {doc}"
    );
}

// 5. A fresh session appends at cursor 0; the fake's committed end is 5, so membrane answers 409
//    `transcript_cursor_mismatch`; CodeRight adopts the committed end and re-appends at cursor 5.
#[test]
fn transcript_append_recovers_from_a_cursor_conflict() {
    let provider = Provider::start(vec![
        Chat::tool("fail-b", "shell", json!({"command": "exit 1"})).into(),
        Chat::text("done").into(),
    ]);
    let (cli, fm) = fixture(&provider);
    run_failing_shell_session(&cli, &fm, "transcript-cursor-session");

    let cursors = append_cursors(&fm.calls());
    assert_eq!(
        cursors.first().map(String::as_str),
        Some("0"),
        "first append starts at 0: {cursors:?}"
    );
    let recovered = cursors
        .iter()
        .position(|c| c == "5")
        .expect("append retried at committed end 5");
    assert!(
        recovered >= 1,
        "the retry follows the refused cursor-0 append: {cursors:?}"
    );
    let docs = fm.stdin_docs("transcript-append");
    assert!(
        docs.iter()
            .all(|d| d.as_array().is_some_and(|rows| !rows.is_empty())),
        "every append carries events: {docs:?}"
    );
    let sessions = fm.calls_matching(&["transcript", "append"]);
    assert!(
        sessions
            .iter()
            .all(|c| c.iter().any(|a| a == "transcript-cursor-session")),
        "appends are bound to the CodeRight session id: {sessions:?}"
    );
}
