//! `coderight run` / `-p` black-box journeys: output contracts (rpc envelope,
//! stream-event records), explicit-session resume, stdin prompt channels
//! (`--pipe`, `--input-format stream-json`), usage errors, plus the daemon-free
//! utility verbs (`completion`, `caps`, `help`, `status` with no daemon).
//!
//! Retires the unit tests for `main_runtime_output`, `main_runtime_run_session*`,
//! `main_runtime_run_input_format*`, `main_runtime_run_exit_code*` and the clap
//! parsing tests in `cli_definitions_tests_*` by asserting what a user or a CI
//! harness actually observes: stdout, stderr, the exit code, and the requests the
//! independent scripted provider received.
//!
//! Evidence map: FM-107 print mode, FM-032/FM-035 stream-json, par #32/#35 input
//! format, KRN-001/002 session execution path, CLI exit-code contract.

use crate::support::e2e_cli;
use e2e_cli::*;

use std::io::Write;
use std::process::{Command, Stdio};

/// Run a prepared command with `stdin_text` piped in, and a hard timeout.
fn finish_with_stdin(mut c: Command, stdin_text: &str, secs: u64) -> Out {
    use std::io::Read;
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().expect("spawn coderight");
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        let _ = stdin.write_all(stdin_text.as_bytes());
        // Dropping closes stdin so the run sees EOF after the supplied lines.
    }
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t2 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let code = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st.code();
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break Some(-999);
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    };
    Out {
        code,
        stdout: t1.join().unwrap(),
        stderr: t2.join().unwrap(),
    }
}

fn last_user_text(req: &Value) -> String {
    messages(req, "user")
        .last()
        .map(|m| m["content"].to_string())
        .unwrap_or_default()
}

#[test]
fn output_format_json_emits_exactly_one_envelope_with_the_terminal_event() {
    for flag in [vec!["--output-format", "json"]] {
        let p = Provider::start(vec![Chat::text("hello envelope").into()]);
        let cli = Cli::new(&p);
        let mut args = flag.clone();
        args.push("--bypass-approvals");
        let out = cli.run(&args, "say hello");
        assert_eq!(out.code, Some(0), "{flag:?}\n{}", out.diag());

        let lines: Vec<&str> = out
            .stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "{flag:?}: one envelope on stdout: {}",
            out.stdout
        );
        let env: Value = serde_json::from_str(lines[0]).expect("envelope is JSON");
        assert_eq!(env["schema_version"], "coderight.cli.rpc.v1");
        assert!(
            env["session_id"].as_str().unwrap_or("").starts_with("cli-"),
            "default runs mint a distinct cli-<uuid> session: {env}"
        );
        assert_eq!(env["terminal_event"]["$kind"], "assistantMessageCompleted");
        assert_eq!(
            env["terminal_event"]["message"]["content"],
            "hello envelope"
        );
        assert_eq!(p.count(), 1, "exactly one model call for a tool-less turn");
    }
}

#[test]
fn json_event_streams_ordered_stream_event_records_for_one_session() {
    let p = Provider::start(vec![Chat::text("streamed answer").into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--json-event", "--bypass-approvals"], "stream it");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let events = out.events();
    assert!(!events.is_empty(), "{}", out.diag());
    let session = events[0]["session_id"].as_str().unwrap_or("").to_string();
    assert!(session.starts_with("cli-"), "{session}");
    for e in &events {
        assert_eq!(e["schema_version"], "coderight.cli.stream_event.v1", "{e}");
        assert_eq!(
            e["session_id"],
            session.as_str(),
            "one session per run: {e}"
        );
        assert!(e["event"].is_string() && e["data"].is_object(), "{e}");
    }
    // Streamed deltas share the pending turn seq; durable (non-delta) events are strictly ordered.
    let all: Vec<i64> = events.iter().filter_map(|e| e["seq"].as_i64()).collect();
    assert!(
        all.windows(2).all(|w| w[0] <= w[1]),
        "seq never goes backwards: {all:?}"
    );
    let durable: Vec<i64> = events
        .iter()
        .filter(|e| e["event"] != "assistantDelta")
        .filter_map(|e| e["seq"].as_i64())
        .collect();
    assert!(
        durable.windows(2).all(|w| w[0] < w[1]),
        "durable events strictly increase: {durable:?}"
    );
    let kinds = out.kinds();
    assert!(kinds.contains(&"assistantDelta".to_string()), "{kinds:?}");
    let done = of_kind(&events, "assistantMessageCompleted");
    assert_eq!(data(done[0])["message"]["content"], "streamed answer");

    // The removed persistent `--rpc` mode is rejected as an unknown flag (A005 removed).
    let removed = cli.run(&["--rpc", "--json-event"], "x");
    assert_eq!(
        removed.code,
        Some(2),
        "removed flag is a usage error: {}",
        removed.diag()
    );
    assert!(removed.stderr.contains("--rpc"), "{}", removed.diag());
    assert_eq!(
        p.count(),
        1,
        "the rejected invocation never reached the provider"
    );
}

#[test]
fn explicit_session_id_resumes_history_and_default_runs_stay_isolated() {
    let p = Provider::start(vec![
        Chat::text("first-answer-RESUME").into(),
        Chat::text("second answer").into(),
        Chat::text("third answer").into(),
    ]);
    let cli = Cli::new(&p);
    let first = cli.run(&["-p", "--session-id", "keep-1"], "FIRST-PROMPT-RESUME");
    assert_eq!(first.code, Some(0), "{}", first.diag());
    assert_eq!(first.stdout.trim(), "first-answer-RESUME");

    let second = cli.run(&["-p", "--session-id", "keep-1"], "SECOND-PROMPT-RESUME");
    assert_eq!(second.code, Some(0), "{}", second.diag());
    let sent = p.requests()[1].to_string();
    assert!(
        sent.contains("FIRST-PROMPT-RESUME"),
        "resume replays the first user turn: {sent}"
    );
    assert!(
        sent.contains("first-answer-RESUME"),
        "resume replays the first answer: {sent}"
    );
    assert!(last_user_text(&p.requests()[1]).contains("SECOND-PROMPT-RESUME"));

    let isolated = cli.run(&["-p"], "ISOLATED-PROMPT");
    assert_eq!(isolated.code, Some(0), "{}", isolated.diag());
    let sent = p.requests()[2].to_string();
    assert!(
        !sent.contains("FIRST-PROMPT-RESUME"),
        "a default run must not append to another session"
    );
}

#[test]
fn pipe_flag_takes_the_prompt_from_stdin_instead_of_the_argument() {
    let p = Provider::start(vec![Chat::text("piped ok").into()]);
    let cli = Cli::new(&p);
    let out = finish_with_stdin(cli.command(&["-p", "--pipe"]), "PIPED-PROMPT-7\n", 90);
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(out.stdout.trim(), "piped ok");
    assert!(
        p.requests()[0].to_string().contains("PIPED-PROMPT-7"),
        "{}",
        p.requests()[0]
    );
}

#[test]
fn stream_json_input_seeds_the_prompt_from_the_first_user_message_event() {
    let p = Provider::start(vec![Chat::text("jsonl answer").into()]);
    let cli = Cli::new(&p);
    let out = finish_with_stdin(
        cli.command(&["--input-format", "stream-json", "--bypass-approvals"]),
        "{\"type\":\"user_message\",\"text\":\"JSONL-PROMPT-9\"}\n",
        90,
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert!(p.requests()[0].to_string().contains("JSONL-PROMPT-9"));
    // stream-json input implies the stream-json output contract.
    assert!(
        out.kinds()
            .contains(&"assistantMessageCompleted".to_string()),
        "{}",
        out.diag()
    );
}

#[test]
fn stream_json_input_without_a_user_message_or_with_garbage_fails_with_typed_envelopes() {
    let p = Provider::start(vec![]);
    let cli = Cli::new(&p);

    let empty = finish_with_stdin(cli.command(&["--input-format", "stream-json"]), "", 60);
    assert_eq!(empty.code, Some(2), "{}", empty.diag());
    let ev = empty.events();
    assert_eq!(ev.len(), 1, "{}", empty.stdout);
    assert_eq!(ev[0]["schema_version"], "coderight.cli.error.v1");
    assert_eq!(ev[0]["error"]["class"], "usage");
    assert!(
        ev[0]["error"]["message"]
            .to_string()
            .contains("stdin ended before a user_message"),
        "{}",
        empty.stdout
    );

    let garbage = finish_with_stdin(
        cli.command(&["--input-format", "stream-json"]),
        "this is not json\n",
        60,
    );
    assert_eq!(
        garbage.code,
        Some(9),
        "malformed input is a schema violation: {}",
        garbage.diag()
    );
    assert_eq!(garbage.events()[0]["error"]["class"], "schema_violation");
    assert_eq!(p.count(), 0, "bad input never reaches the provider");
}

#[test]
fn completion_scripts_are_generated_for_every_shell_and_reject_unknown_ones() {
    let p = Provider::start(vec![]);
    let cli = Cli::new(&p);
    let expect: [(&str, &str); 5] = [
        ("bash", "_coderight"),
        ("zsh", "#compdef coderight"),
        ("fish", "complete -c coderight"),
        ("powershell", "Register-ArgumentCompleter"),
        ("elvish", "edit:completion:arg-completer"),
    ];
    for (shell, marker) in expect {
        let out = cli.exec(&["completion", shell], &[]);
        assert_eq!(out.code, Some(0), "{shell}: {}", out.diag());
        assert!(
            out.stdout.contains(marker),
            "{shell} script lacks {marker:?}"
        );
        assert!(
            out.stdout.contains("session"),
            "{shell} script lists the session verb"
        );
    }
    let bad = cli.exec(&["completion", "nushell"], &[]);
    assert_eq!(bad.code, Some(2), "{}", bad.diag());
    assert!(bad.stdout.is_empty(), "no partial script on a usage error");
}

#[test]
fn caps_json_counts_are_consistent_with_the_listed_tool_capabilities() {
    let p = Provider::start(vec![]);
    let cli = Cli::new(&p);
    let out = cli.exec(&["caps", "--json"], &[]);
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("caps --json is JSON");
    let caps = v["capabilities"].as_array().expect("capabilities array");
    assert_eq!(v["toolCount"].as_u64().unwrap() as usize, caps.len());
    let read_only = caps.iter().filter(|c| c["readOnly"] == true).count() as u64;
    assert_eq!(v["readOnlyCount"].as_u64().unwrap(), read_only);
    assert_eq!(
        v["readOnlyCount"].as_u64().unwrap() + v["mutatingCount"].as_u64().unwrap(),
        v["toolCount"].as_u64().unwrap()
    );
    let names: Vec<&str> = caps.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(
        names.contains(&"read_file") && names.contains(&"write_file"),
        "{names:?}"
    );
    let write = caps.iter().find(|c| c["name"] == "write_file").unwrap();
    assert_eq!(
        write["readOnly"], false,
        "write_file is a mutating capability"
    );

    let md = cli.exec(&["caps"], &[]);
    assert_eq!(md.code, Some(0), "{}", md.diag());
    assert!(
        md.stdout.contains("write_file"),
        "markdown listing names the tools"
    );
    assert!(
        serde_json::from_str::<Value>(md.stdout.trim()).is_err(),
        "default form is markdown, not JSON"
    );
}

#[test]
fn help_version_and_unknown_verbs_follow_the_cli_contract() {
    let p = Provider::start(vec![]);
    let cli = Cli::new(&p);

    let help = cli.exec(&["help"], &[]);
    assert_eq!(help.code, Some(0), "{}", help.diag());
    assert!(
        help.stdout.contains("run") && help.stdout.contains("session"),
        "{}",
        help.stdout
    );
    let all = cli.exec(&["help", "--all"], &[]);
    assert_eq!(all.code, Some(0), "{}", all.diag());
    assert!(
        all.stdout.len() >= help.stdout.len(),
        "--all never shows less than the default view"
    );

    let version = cli.exec(&["--version"], &[]);
    assert_eq!(version.code, Some(0), "{}", version.diag());
    assert!(
        version.stdout.contains(env!("CARGO_PKG_VERSION")),
        "{}",
        version.stdout
    );

    // Bare positional text is a supported prompt-launch form, so an
    // unknown-looking word is user input rather than an invalid subcommand.
    // Exercise unknown-option handling on an explicit command instead.
    let unknown = cli.exec(&["run", "--definitely-not-an-option"], &[]);
    assert_eq!(unknown.code, Some(2), "{}", unknown.diag());
    assert!(
        unknown.stderr.contains("unexpected argument")
            || unknown.stderr.contains("unknown argument"),
        "{}",
        unknown.diag()
    );

    let missing_arg = cli.exec(&["session", "delete"], &[]);
    assert_eq!(missing_arg.code, Some(2), "{}", missing_arg.diag());
    assert!(
        missing_arg.stderr.contains("SESSION_ID"),
        "{}",
        missing_arg.diag()
    );

    // Prompt usage errors never reach the provider.
    let blank = cli.run(&[], "   ");
    assert_eq!(blank.code, Some(2), "{}", blank.diag());
    assert!(
        blank.stderr.contains("blank after trimming"),
        "{}",
        blank.diag()
    );
    let missing = finish_with_stdin(cli.command(&[]), "", 60);
    assert_eq!(
        missing.code,
        Some(2),
        "clap requires a prompt: {}",
        missing.diag()
    );
    assert!(
        missing.stderr.to_uppercase().contains("PROMPT"),
        "{}",
        missing.diag()
    );
    assert_eq!(p.count(), 0);
}

#[test]
fn status_and_info_without_a_daemon_report_stopped_and_exit_zero() {
    let p = Provider::start(vec![]);
    let cli = Cli::new(&p);
    let status = cli.exec(&["status"], &[]);
    assert_eq!(status.code, Some(0), "{}", status.diag());
    let v: Value = serde_json::from_str(status.stdout.trim()).expect("status is one JSON record");
    assert_eq!(v["running"], false);
    assert_eq!(v["status"], "stopped");

    let info = cli.exec(&["info", "--json"], &[]);
    assert_eq!(info.code, Some(0), "{}", info.diag());
    let v: Value = serde_json::from_str(info.stdout.trim()).unwrap();
    assert_eq!(v["schema_version"], "coderight.cli.info.v1");
    assert_eq!(v["daemon"]["running"], false);
    let text = cli.exec(&["info"], &[]);
    assert!(
        text.stdout.contains("daemon: not running"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout
            .contains(&format!("CodeRight {}", env!("CARGO_PKG_VERSION"))),
        "{}",
        text.stdout
    );
}
