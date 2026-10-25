// rightkit-hooks 0.2.0 consumed by exact registry pin (default features, the set the Pulse chat
// hooks use), outside the workspace.
//
// Proves, through the public API only:
// - event parsing: Wire and EventKind names (host spellings and the lower-case aliases normalise to
//   the host names, an unknown name is kept verbatim); HookEvent::from_value, from_slice and
//   from_reader read the field aliases (hook_event_name / hookEventName / event, thread_id /
//   session_id / sessionId, tool_name / toolName, prompt / user_prompt), skip blank strings, keep
//   the untouched JSON in raw, and refuse malformed JSON, a non-object, a missing event name and
//   input over STDIN_CAP (ParseError); is_own_turn follows the subagent guard on both wires;
// - output builders: Reply::additional_context for the three events that take it and an
//   OutputError for the others, block_stop, deny_tool and ask_tool on Claude (stdout JSON, exit 0)
//   and Codex (stderr, exit 2), with_system_message, stdout_line, write_to;
// - the dispatcher: modules run in order; contexts join and are capped on a character boundary; a
//   decision wins over context and is dropped where the event does not take it; an error, a panic
//   and a timed-out module become typed results with status Error and the fallback reply; a spent
//   total deadline records the remaining modules as errors without starting them; compose_reply on
//   its own; run_with over byte streams exits 0 on garbage input and 2 only for a Codex deny;
// - receipts: marker and tag_context (the cap is never exceeded and the marker survives), unique
//   alphanumeric receipt ids, ReceiptStore record_emitted / tag_and_record / list / observe against
//   a transcript file, the Membrane store's two field spellings on disk, scan_transcript_tail on a
//   missing file, and ObserveModule flipping receipts on a Stop event and doing nothing on another;
// - the Membrane wire types: module ids and their order, status builders, aggregate status, and
//   project_hook_host_response for Stop, PreToolUse and an event without a host projection, with a
//   JSON round trip that reproduces the wire and refuses unknown fields.
// Does not prove: the process stdin/stdout entry point `run` (it reads the real process streams),
// the 3 s and 8 s default deadlines under load, a real Claude Code or Codex host, or any module
// body (the crate holds no policy). Public API only.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no real user data. The longest wait is a 150 ms module deadline.
use rightkit_hooks::dispatch::{
    DEFAULT_CONTEXT_CAP_BYTES, DEFAULT_MODULE_DEADLINE, DEFAULT_TOTAL_DEADLINE,
};
use rightkit_hooks::membrane::{
    aggregate_hook_status, project_hook_host_response, HookDispatchResultV1, HookHostResponseV1,
    HookInvocationStatus, HookModuleId, HookModuleOutputV1, HookModuleResultV1, HookModuleState,
    HOOK_MODULE_DEADLINE_MS, HOOK_SCHEMA_VERSION,
};
use rightkit_hooks::receipt::{MEMBRANE_MARKER_PREFIX, SCAN_BUDGET, SCAN_TAIL_BYTES};
use rightkit_hooks::{
    compose_reply, dispatch, marker, new_receipt_id, run_with, scan_transcript_tail, tag_context,
    Decision, DeliveryLevel, DispatchOptions, EventKind, FnModule, HookEvent, HookModule,
    ModuleOutput, ObserveModule, OutputError, ParseError, ReceiptStore, Reply, Status, Wire,
    STDIN_CAP,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

type Modules = Vec<Arc<dyn HookModule>>;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rk-hooks-consumer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create consumer temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() {
    // Modules that panic on purpose run on threads named hook-module-*: keep their panic reports
    // out of the log, and leave every other panic (a failed assertion) untouched.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let module_thread = std::thread::current()
            .name()
            .is_some_and(|name| name.starts_with("hook-module-"));
        if !module_thread {
            previous(info);
        }
    }));
    names_and_events();
    parsing_and_aliases();
    output_shapes();
    dispatcher();
    process_streams();
    let temp = TempDir::new();
    receipts(&temp.0);
    membrane_wire();
    println!("OK rightkit-hooks 0.2.0 (registry consumer)");
}

fn event(wire: Wire, payload: Value) -> HookEvent {
    HookEvent::from_value(wire, payload).expect("valid payload")
}

fn module(
    id: &str,
    run: impl Fn(&HookEvent) -> Result<ModuleOutput, String> + Send + Sync + 'static,
) -> Arc<dyn HookModule> {
    Arc::new(FnModule::new(id, move |event, _ctx| run(event)))
}

// ---- names ---------------------------------------------------------------------------------

fn names_and_events() {
    assert_eq!(Wire::parse("claude"), Some(Wire::Claude));
    assert_eq!(Wire::parse("codex"), Some(Wire::Codex));
    assert_eq!(Wire::parse("Claude"), None);
    assert_eq!(Wire::Claude.as_str(), "claude");
    assert_eq!(Wire::Codex.to_string(), "codex");

    let table = [
        ("SessionStart", EventKind::SessionStart),
        ("session-start", EventKind::SessionStart),
        ("UserPromptSubmit", EventKind::UserPromptSubmit),
        ("user-prompt-submit", EventKind::UserPromptSubmit),
        ("PreToolUse", EventKind::PreToolUse),
        ("pre-effect", EventKind::PreToolUse),
        ("PostToolUse", EventKind::PostToolUse),
        ("post-effect", EventKind::PostToolUse),
        ("PostToolUseFailure", EventKind::PostToolUseFailure),
        ("post-effect-failure", EventKind::PostToolUseFailure),
        ("Stop", EventKind::Stop),
        ("stop", EventKind::Stop),
        ("SubagentStart", EventKind::SubagentStart),
        ("SubagentStop", EventKind::SubagentStop),
        ("PreCompact", EventKind::PreCompact),
        ("PostCompact", EventKind::PostCompact),
        ("SessionEnd", EventKind::SessionEnd),
        ("Notification", EventKind::Notification),
    ];
    for (name, kind) in table {
        assert_eq!(EventKind::from_name(name), kind, "{name}");
    }
    let custom = EventKind::from_name("CustomEvent");
    assert_eq!(custom, EventKind::Other("CustomEvent".into()));
    assert_eq!(
        custom.as_str(),
        "CustomEvent",
        "unknown names are kept, never rejected"
    );
    assert_eq!(EventKind::PreToolUse.as_str(), "PreToolUse");
    assert_eq!(EventKind::Stop.to_string(), "Stop");
    assert_eq!(
        serde_json::to_value(EventKind::PostToolUse).unwrap(),
        json!("PostToolUse")
    );
    assert_eq!(
        serde_json::from_value::<EventKind>(json!("pre-effect")).unwrap(),
        EventKind::PreToolUse
    );
    let takes: Vec<&str> = [
        EventKind::SessionStart,
        EventKind::UserPromptSubmit,
        EventKind::PreToolUse,
        EventKind::PostToolUse,
        EventKind::Stop,
        EventKind::SessionEnd,
    ]
    .iter()
    .filter(|kind| kind.takes_additional_context())
    .map(EventKind::as_str)
    .collect();
    assert_eq!(takes, ["SessionStart", "UserPromptSubmit", "PostToolUse"]);
    assert_eq!(STDIN_CAP, 4 * 1024 * 1024);
}

// ---- parsing -------------------------------------------------------------------------------

fn parsing_and_aliases() {
    let payload = json!({
        "hookEventName": "PreToolUse",
        "sessionId": "sess-1",
        "toolName": "Bash",
        "toolUseId": "tu-1",
        "cwd": "/work",
        "transcript_path": "/work/t.jsonl",
        "turn_id": "turn-1",
        "agent_id": "sub-1",
        "parent_tool_use_id": "tu-0",
        "user_prompt": "do the thing",
        "stop_hook_active": true,
        "last_assistant_message": "done",
        "tool_input": { "command": "ls" },
        "extra": [1, 2, 3]
    });
    let parsed = event(Wire::Claude, payload.clone());
    assert_eq!(parsed.wire, Wire::Claude);
    assert_eq!(parsed.kind, EventKind::PreToolUse);
    assert_eq!(parsed.session_id.as_deref(), Some("sess-1"));
    assert_eq!(parsed.tool_name.as_deref(), Some("Bash"));
    assert_eq!(parsed.tool_use_id.as_deref(), Some("tu-1"));
    assert_eq!(parsed.cwd.as_deref(), Some("/work"));
    assert_eq!(parsed.transcript_path.as_deref(), Some("/work/t.jsonl"));
    assert_eq!(parsed.turn_id.as_deref(), Some("turn-1"));
    assert_eq!(parsed.agent_id.as_deref(), Some("sub-1"));
    assert_eq!(parsed.parent_tool_use_id.as_deref(), Some("tu-0"));
    assert_eq!(parsed.prompt.as_deref(), Some("do the thing"));
    assert_eq!(parsed.stop_hook_active, Some(true));
    assert_eq!(parsed.last_assistant_message.as_deref(), Some("done"));
    assert_eq!(parsed.tool_input, Some(json!({ "command": "ls" })));
    assert_eq!(parsed.raw, payload, "the untouched payload stays in raw");

    // Alias precedence: thread_id wins over session_id wins over sessionId; blanks are skipped.
    let ids = event(
        Wire::Codex,
        json!({ "event": "Stop", "thread_id": "  ", "session_id": "s-2", "sessionId": "s-3", "prompt": "", "user_prompt": "fallback" }),
    );
    assert_eq!(ids.kind, EventKind::Stop);
    assert_eq!(ids.session_id.as_deref(), Some("s-2"));
    assert_eq!(ids.prompt.as_deref(), Some("fallback"));
    let thread = event(
        Wire::Codex,
        json!({ "hook_event_name": "stop", "thread_id": "t-1", "session_id": "s-2" }),
    );
    assert_eq!(thread.session_id.as_deref(), Some("t-1"));

    let bytes = serde_json::to_vec(&payload).unwrap();
    assert_eq!(HookEvent::from_slice(Wire::Claude, &bytes).unwrap(), parsed);
    assert_eq!(
        HookEvent::from_reader(Wire::Claude, &bytes[..]).unwrap(),
        parsed
    );

    assert!(matches!(
        HookEvent::from_slice(Wire::Claude, b"{nope"),
        Err(ParseError::Malformed(_))
    ));
    assert_eq!(
        HookEvent::from_value(Wire::Claude, json!([1])),
        Err(ParseError::NotObject)
    );
    assert_eq!(
        HookEvent::from_value(Wire::Claude, json!("text")),
        Err(ParseError::NotObject)
    );
    assert_eq!(
        HookEvent::from_value(Wire::Claude, json!({ "cwd": "/" })),
        Err(ParseError::EventRequired)
    );
    assert_eq!(
        HookEvent::from_value(Wire::Claude, json!({ "event": "   " })),
        Err(ParseError::EventRequired)
    );
    let oversized = vec![b' '; STDIN_CAP as usize + 1];
    assert_eq!(
        HookEvent::from_reader(Wire::Claude, &oversized[..]),
        Err(ParseError::TooLarge)
    );
    for error in [
        ParseError::NotObject,
        ParseError::EventRequired,
        ParseError::TooLarge,
    ] {
        assert!(!error.to_string().is_empty());
    }

    // The subagent guard: only PostToolUse can belong to a subagent.
    assert!(event(Wire::Claude, json!({ "event": "Stop", "agent_id": "x" })).is_own_turn());
    assert!(event(Wire::Claude, json!({ "event": "PostToolUse" })).is_own_turn());
    assert!(!event(
        Wire::Claude,
        json!({ "event": "PostToolUse", "agent_id": "x" })
    )
    .is_own_turn());
    let codex_post = |path: Option<&str>| {
        let mut payload = json!({ "event": "PostToolUse", "session_id": "abc123" });
        if let Some(path) = path {
            payload["transcript_path"] = json!(path);
        }
        event(Wire::Codex, payload)
    };
    assert!(codex_post(Some("/s/rollout-abc123.jsonl")).is_own_turn());
    assert!(!codex_post(Some("/s/rollout-other.jsonl")).is_own_turn());
    assert!(
        !codex_post(None).is_own_turn(),
        "no transcript path, not provably the chat's own"
    );
}

// ---- output shapes -------------------------------------------------------------------------

fn output_shapes() {
    assert!(Reply::none().is_none());
    assert_eq!(Reply::none().stdout_line(), None);

    for kind in [
        EventKind::SessionStart,
        EventKind::UserPromptSubmit,
        EventKind::PostToolUse,
    ] {
        let reply = Reply::additional_context(&kind, "remember this").expect("context allowed");
        assert_eq!(
            reply.stdout,
            Some(
                json!({ "hookSpecificOutput": { "hookEventName": kind.as_str(), "additionalContext": "remember this" } })
            )
        );
        assert_eq!(reply.exit_code, 0);
        assert!(!reply.is_none());
    }
    for kind in [
        EventKind::Stop,
        EventKind::PreToolUse,
        EventKind::SessionEnd,
        EventKind::Other("X".into()),
    ] {
        assert!(
            matches!(
                Reply::additional_context(&kind, "x"),
                Err(OutputError::Unsupported { .. })
            ),
            "{kind:?} takes no additionalContext"
        );
    }
    let refused = Reply::additional_context(&EventKind::Stop, "x").unwrap_err();
    assert!(refused.to_string().contains("Stop"));

    assert_eq!(
        Reply::block_stop("keep going").stdout,
        Some(json!({ "decision": "block", "reason": "keep going" }))
    );

    let deny = Reply::deny_tool(Wire::Claude, "no shell");
    assert_eq!(
        deny.stdout,
        Some(
            json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "no shell" } })
        )
    );
    assert_eq!((deny.stderr.clone(), deny.exit_code), (None, 0));
    let ask = Reply::ask_tool(Wire::Claude, "sure?");
    assert_eq!(
        ask.stdout.as_ref().unwrap()["hookSpecificOutput"]["permissionDecision"],
        "ask"
    );
    for codex in [
        Reply::deny_tool(Wire::Codex, "no shell"),
        Reply::ask_tool(Wire::Codex, "no shell"),
    ] {
        assert_eq!(codex.stdout, None, "Codex denies on stderr with exit 2");
        assert_eq!(codex.stderr.as_deref(), Some("no shell"));
        assert_eq!(codex.exit_code, 2);
    }

    let notice = Reply::none().with_system_message("heads up");
    assert_eq!(notice.stdout, Some(json!({ "systemMessage": "heads up" })));
    let both = Reply::block_stop("r").with_system_message("m");
    assert_eq!(
        both.stdout,
        Some(json!({ "decision": "block", "reason": "r", "systemMessage": "m" }))
    );
    assert_eq!(
        Reply::deny_tool(Wire::Codex, "r")
            .with_system_message("m")
            .stdout,
        None,
        "a Codex denial keeps stdout empty"
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = Reply::deny_tool(Wire::Codex, "blocked").write_to(&mut out, &mut err);
    assert_eq!(
        (code, out.as_slice(), err.as_slice()),
        (2, &b""[..], &b"blocked\n"[..])
    );
    let mut out = Vec::new();
    let code = Reply::block_stop("go on").write_to(&mut out, Vec::new());
    assert_eq!(code, 0);
    let line = String::from_utf8(out).unwrap();
    assert!(
        line.ends_with('\n') && line.trim_end() == r#"{"decision":"block","reason":"go on"}"#,
        "{line}"
    );
}

// ---- dispatcher ----------------------------------------------------------------------------

fn dispatcher() {
    assert_eq!(DEFAULT_MODULE_DEADLINE, Duration::from_millis(3_000));
    assert_eq!(DEFAULT_TOTAL_DEADLINE, Duration::from_millis(8_000));
    assert_eq!(DEFAULT_CONTEXT_CAP_BYTES, 60_000);
    let defaults = DispatchOptions::default();
    assert_eq!(defaults.module_deadline, DEFAULT_MODULE_DEADLINE);
    assert_eq!(defaults.total_deadline, DEFAULT_TOTAL_DEADLINE);
    assert_eq!(defaults.context_cap_bytes, DEFAULT_CONTEXT_CAP_BYTES);
    assert!(defaults.fallback.is_none());

    let start = || {
        event(
            Wire::Claude,
            json!({ "event": "SessionStart", "session_id": "s" }),
        )
    };

    // Order, context join and the empty case.
    let modules: Modules = vec![
        module("first", |_| Ok(ModuleOutput::context("alpha"))),
        module("quiet", |_| Ok(ModuleOutput::empty())),
        module("second", |_| Ok(ModuleOutput::context("beta"))),
    ];
    let result = dispatch(start(), &modules, &DispatchOptions::default());
    assert_eq!(result.status, Status::Ok);
    assert_eq!(result.event, EventKind::SessionStart);
    assert_eq!(
        result
            .results
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "quiet", "second"]
    );
    assert!(result
        .results
        .iter()
        .all(|r| r.status == Status::Ok && r.error.is_none()));
    assert_eq!(
        result.reply.stdout.as_ref().unwrap()["hookSpecificOutput"]["additionalContext"],
        "alpha\n\nbeta",
        "contexts join with a blank line"
    );
    let nothing = dispatch(start(), &[], &DispatchOptions::default());
    assert!(nothing.reply.is_none() && nothing.status == Status::Ok);

    // Context cap on a character boundary.
    let big: Modules = vec![module("big", |_| {
        Ok(ModuleOutput::context("\u{e9}".repeat(100)))
    })];
    let capped = dispatch(
        start(),
        &big,
        &DispatchOptions {
            context_cap_bytes: 11,
            ..DispatchOptions::default()
        },
    );
    let text = capped.reply.stdout.unwrap()["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        text,
        "\u{e9}".repeat(5),
        "11 bytes cut back to the last whole character"
    );

    // Failure kinds: error, panic, timeout. The fallback is printed only when nothing else is.
    let fallback = Reply::none().with_system_message("hook degraded");
    let failing: Modules = vec![
        module("errs", |_| Err("broken".into())),
        module("panics", |_| panic!("module bug")),
    ];
    let failed = dispatch(
        start(),
        &failing,
        &DispatchOptions {
            fallback: fallback.clone(),
            ..DispatchOptions::default()
        },
    );
    assert_eq!(failed.status, Status::Error);
    assert_eq!(failed.results[0].error.as_deref(), Some("broken"));
    assert_eq!(failed.results[1].error.as_deref(), Some("module panicked"));
    assert!(failed.results.iter().all(|r| r.output.is_none()));
    assert_eq!(
        failed.reply, fallback,
        "no module produced anything, so the fallback is printed"
    );
    let partial: Modules = vec![
        module("errs", |_| Err("broken".into())),
        module("ok", |_| Ok(ModuleOutput::context("kept"))),
    ];
    let partial = dispatch(
        start(),
        &partial,
        &DispatchOptions {
            fallback: fallback.clone(),
            ..DispatchOptions::default()
        },
    );
    assert_eq!(partial.status, Status::Error);
    assert_ne!(
        partial.reply, fallback,
        "a module's own output beats the fallback"
    );

    let slow: Arc<dyn HookModule> = Arc::new(FnModule::new("slow", |_event, ctx| {
        // Cooperative: leave once the dispatcher marks the module cancelled.
        let give_up = std::time::Instant::now() + Duration::from_secs(10);
        while !ctx.is_cancelled() && std::time::Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(ModuleOutput::context("too late"))
    }));
    let options = DispatchOptions {
        module_deadline: Duration::from_millis(150),
        total_deadline: Duration::from_secs(5),
        ..DispatchOptions::default()
    };
    let timed = dispatch(
        start(),
        &[
            slow.clone(),
            module("after", |_| Ok(ModuleOutput::context("on time"))),
        ],
        &options,
    );
    assert_eq!(timed.status, Status::Error);
    assert_eq!(timed.results[0].status, Status::Error);
    assert!(
        timed.results[0]
            .error
            .as_deref()
            .unwrap()
            .contains("timed out"),
        "{:?}",
        timed.results[0].error
    );
    assert!(timed.results[0].elapsed < Duration::from_secs(5));
    assert_eq!(
        timed.results[1].status,
        Status::Ok,
        "the next module still runs"
    );
    assert!(timed.reply.stdout.unwrap().to_string().contains("on time"));

    // A spent total deadline: later modules are recorded, not started.
    let spent = dispatch(
        start(),
        &[
            slow,
            module("never", |_| Ok(ModuleOutput::context("never"))),
        ],
        &DispatchOptions {
            module_deadline: Duration::from_millis(150),
            total_deadline: Duration::from_millis(150),
            ..DispatchOptions::default()
        },
    );
    assert!(spent.results[0]
        .error
        .as_deref()
        .unwrap()
        .contains("timed out"));
    assert!(
        spent.results[1]
            .error
            .as_deref()
            .unwrap()
            .contains("total deadline"),
        "{:?}",
        spent.results[1].error
    );
    assert_eq!(spent.results[1].elapsed, Duration::ZERO);

    // Decisions: honoured where the event takes them, dropped elsewhere; a decision beats context.
    let deny = |reason: &str| ModuleOutput::decide(Decision::DenyTool(reason.into()));
    let mixed: Modules = vec![
        module("ctx", |_| Ok(ModuleOutput::context("note"))),
        module("deny", move |_| Ok(deny("no"))),
    ];
    let pre = event(Wire::Claude, json!({ "event": "PreToolUse" }));
    let reply = dispatch(pre.clone(), &mixed, &DispatchOptions::default()).reply;
    assert_eq!(
        reply.stdout.as_ref().unwrap()["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
    let codex_pre = event(Wire::Codex, json!({ "event": "PreToolUse" }));
    let reply = dispatch(codex_pre, &mixed, &DispatchOptions::default()).reply;
    assert_eq!(
        (reply.exit_code, reply.stderr.as_deref(), reply.stdout),
        (2, Some("no"), None)
    );
    let ask: Modules = vec![module("ask", |_| {
        Ok(ModuleOutput::decide(Decision::AskTool("sure?".into())))
    })];
    let reply = dispatch(pre, &ask, &DispatchOptions::default()).reply;
    assert_eq!(
        reply.stdout.unwrap()["hookSpecificOutput"]["permissionDecision"],
        "ask"
    );
    let block: Modules = vec![module("block", |_| {
        Ok(ModuleOutput::decide(Decision::BlockStop("again".into())))
    })];
    let stop = event(Wire::Claude, json!({ "event": "Stop" }));
    assert_eq!(
        dispatch(stop.clone(), &block, &DispatchOptions::default())
            .reply
            .stdout,
        Some(json!({ "decision": "block", "reason": "again" }))
    );
    assert!(
        dispatch(start(), &block, &DispatchOptions::default())
            .reply
            .is_none(),
        "a Stop decision on SessionStart is dropped, never emitted"
    );
    let notice: Modules = vec![module("notice", |_| {
        Ok(ModuleOutput {
            system_message: Some("fyi".into()),
            ..ModuleOutput::default()
        })
    })];
    assert_eq!(
        dispatch(stop, &notice, &DispatchOptions::default())
            .reply
            .stdout,
        Some(json!({ "systemMessage": "fyi" }))
    );

    // compose_reply on its own, over hand-built results.
    let composed = compose_reply(&start(), &timed.results, 1_000);
    assert!(composed.stdout.unwrap().to_string().contains("on time"));
}

// ---- process streams -----------------------------------------------------------------------

fn process_streams() {
    let modules: Modules = vec![module("ctx", |_| Ok(ModuleOutput::context("hello")))];
    let options = DispatchOptions::default();
    let run = |wire: Wire, input: &[u8], modules: &Modules, options: &DispatchOptions| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(wire, modules, options, input, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };

    let (code, out, err) = run(
        Wire::Claude,
        br#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#,
        &modules,
        &options,
    );
    assert_eq!(code, 0);
    assert_eq!(err, "");
    let printed: Value = serde_json::from_str(out.trim_end()).expect("one JSON line");
    assert_eq!(
        printed["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert_eq!(printed["hookSpecificOutput"]["additionalContext"], "hello");

    // Every failure exits 0 and prints the configured fallback (nothing by default).
    for bad in [&b"not json"[..], b"[]", br#"{"cwd":"/"}"#, b""] {
        assert_eq!(
            run(Wire::Claude, bad, &modules, &options),
            (0, String::new(), String::new())
        );
    }
    let with_fallback = DispatchOptions {
        fallback: Reply::none().with_system_message("degraded"),
        ..DispatchOptions::default()
    };
    let (code, out, _) = run(Wire::Codex, b"garbage", &modules, &with_fallback);
    assert_eq!(code, 0);
    assert!(out.contains("degraded"));
    let panicking: Modules = vec![module("boom", |_| panic!("module bug"))];
    assert_eq!(
        run(Wire::Claude, br#"{"event":"Stop"}"#, &panicking, &options).0,
        0
    );

    let deny: Modules = vec![module("deny", |_| {
        Ok(ModuleOutput::decide(Decision::DenyTool("blocked".into())))
    })];
    let payload = br#"{"event":"PreToolUse","tool_name":"Bash"}"#;
    assert_eq!(
        run(Wire::Codex, payload, &deny, &options),
        (2, String::new(), "blocked\n".to_owned())
    );
    let (code, out, _) = run(Wire::Claude, payload, &deny, &options);
    assert_eq!(code, 0, "Claude denies in JSON with exit 0");
    assert!(out.contains("\"permissionDecision\":\"deny\""));
}

// ---- receipts ------------------------------------------------------------------------------

fn receipts(dir: &PathBuf) {
    assert_eq!(marker("pulse", "abc123"), "[pulse:abc123]");
    let ids: BTreeSet<String> = (0..200).map(|_| new_receipt_id()).collect();
    assert_eq!(ids.len(), 200, "receipt ids do not repeat");
    assert!(ids
        .iter()
        .all(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())));

    let tagged = tag_context("body text", "pulse", "id1", 1_000);
    assert_eq!(tagged, "body text\n[pulse:id1]");
    let cut = tag_context("0123456789", "pulse", "id1", 18);
    assert_eq!(
        cut, "012345\n[pulse:id1]",
        "the body is cut, the marker never"
    );
    assert!(cut.len() <= 18);
    let tiny = tag_context("0123456789", "pulse", "id1", 5);
    assert_eq!(
        tiny, "01234",
        "a cap smaller than the marker line drops the marker"
    );
    let multibyte = tag_context("\u{e9}\u{e9}\u{e9}", "p", "i", 10);
    assert!(
        multibyte.len() <= 10
            && multibyte.ends_with("\n[p:i]")
            && multibyte.is_char_boundary(multibyte.len() - 6),
        "{multibyte:?}"
    );

    assert_eq!(SCAN_TAIL_BYTES, 4 * 1024 * 1024);
    assert_eq!(SCAN_BUDGET, Duration::from_secs(2));
    assert_eq!(MEMBRANE_MARKER_PREFIX, "membrane");
    assert!(
        scan_transcript_tail(&dir.join("missing.jsonl"), &["[x:y]".into()], SCAN_BUDGET).is_empty()
    );

    let store = ReceiptStore::new(dir.join("pulse-store"), "pulse");
    assert_eq!(store.prefix(), "pulse");
    let stop_event = event(
        Wire::Claude,
        json!({ "hook_event_name": "UserPromptSubmit", "session_id": "chat/1", "transcript_path": "unused" }),
    );
    let first = store
        .tag_and_record(&stop_event, "first context", 1_000)
        .expect("record first");
    let second = store
        .tag_and_record(&stop_event, "second context", 1_000)
        .expect("record second");
    let ids: Vec<String> = [&first, &second]
        .iter()
        .map(|text| {
            text.lines()
                .last()
                .unwrap()
                .trim_start_matches("[pulse:")
                .trim_end_matches(']')
                .to_owned()
        })
        .collect();
    assert_ne!(ids[0], ids[1]);
    let listed = store.list("chat/1");
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|r| r.level == DeliveryLevel::Emitted
        && r.wire == "claude"
        && r.event == "UserPromptSubmit"
        && r.session == "chat/1"));
    assert_eq!(
        listed.iter().map(|r| r.chars).sum::<usize>(),
        first.len() + second.len(),
        "the receipt keeps the length, never the context"
    );
    assert!(store.list("another-session").is_empty());
    assert!(
        store.list("..").is_empty(),
        "a session id that is not a file name has no receipts"
    );
    let no_session = event(Wire::Claude, json!({ "event": "Stop" }));
    assert!(
        store.record_emitted(&no_session, "abc", "x").is_err(),
        "an event without a session id cannot be recorded"
    );
    assert!(
        store
            .record_emitted(&stop_event, "not alnum!", "x")
            .is_err(),
        "a receipt id must be alphanumeric"
    );

    // Observe: only the marker found in the transcript tail flips.
    let transcript = dir.join("transcript.jsonl");
    fs::write(
        &transcript,
        format!(
            "{{\"text\":\"earlier\"}}\n{{\"text\":\"{}\"}}\n",
            first.replace('\n', " ")
        ),
    )
    .unwrap();
    assert_eq!(
        scan_transcript_tail(
            &transcript,
            &[marker("pulse", &ids[0]), marker("pulse", &ids[1])],
            SCAN_BUDGET
        ),
        [marker("pulse", &ids[0])]
    );
    assert_eq!(
        store.observe("chat/1", Wire::Codex, &transcript),
        0,
        "receipts of another wire are left alone"
    );
    assert_eq!(store.observe("chat/1", Wire::Claude, &transcript), 1);
    let after = store.list("chat/1");
    assert_eq!(
        after
            .iter()
            .filter(|r| r.level == DeliveryLevel::Observed)
            .count(),
        1
    );
    assert_eq!(
        after
            .iter()
            .filter(|r| r.level == DeliveryLevel::Emitted)
            .count(),
        1
    );
    assert_eq!(
        store.observe("chat/1", Wire::Claude, &transcript),
        0,
        "a flipped receipt is not counted twice"
    );

    // ObserveModule: flips receipts on a Stop event, does nothing on another event.
    let observe = ObserveModule::new(store.clone());
    assert_eq!(observe.id(), "rightkit.receipts.observe");
    fs::write(
        &transcript,
        format!(
            "{} {}\n",
            first.replace('\n', " "),
            second.replace('\n', " ")
        ),
    )
    .unwrap();
    let stop = event(
        Wire::Claude,
        json!({ "hook_event_name": "Stop", "session_id": "chat/1", "transcript_path": transcript.to_str().unwrap() }),
    );
    let modules: Modules = vec![Arc::new(ObserveModule::new(store.clone()))];
    let run = dispatch(stop, &modules, &DispatchOptions::default());
    assert_eq!(run.status, Status::Ok);
    assert_eq!(
        run.results[0].output.as_ref().unwrap().detail,
        json!({ "observed": 1 })
    );
    assert!(store
        .list("chat/1")
        .iter()
        .all(|r| r.level == DeliveryLevel::Observed));
    let not_stop = event(
        Wire::Claude,
        json!({ "event": "UserPromptSubmit", "session_id": "chat/1", "transcript_path": transcript.to_str().unwrap() }),
    );
    let result = dispatch(
        not_stop,
        &[Arc::new(ObserveModule::new(store.clone())) as Arc<dyn HookModule>],
        &DispatchOptions::default(),
    );
    assert_eq!(
        result.results[0].output.as_ref().unwrap().detail,
        Value::Null
    );

    // The Membrane store: shared layout, both field spellings on disk.
    let membrane = ReceiptStore::membrane(dir.join("membrane-store"));
    assert_eq!(membrane.prefix(), "membrane");
    let tagged = membrane
        .tag_and_record(&stop_event, "ctx", 1_000)
        .expect("record");
    assert!(tagged.contains("[membrane:"));
    let session_dir = dir.join("membrane-store").join("receipts").join("chat_1");
    let file = fs::read_dir(&session_dir)
        .expect("receipt directory")
        .next()
        .expect("one receipt")
        .unwrap()
        .path();
    let disk: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(disk["wire"], "claude");
    assert_eq!(disk["harness"], "claude");
    assert_eq!(disk["level"], "emitted");
    assert_eq!(disk["state"], "emitted");
    assert!(
        disk["chars"].as_u64().unwrap() > 0 && disk.get("context").is_none(),
        "content-free"
    );
    // Membrane flips only `state`; this crate still reads the receipt as observed.
    let mut flipped = disk.clone();
    flipped["state"] = json!("delivered");
    fs::write(&file, serde_json::to_vec(&flipped).unwrap()).unwrap();
    assert_eq!(membrane.list("chat/1")[0].level, DeliveryLevel::Observed);
}

// ---- Membrane wire types -------------------------------------------------------------------

fn membrane_wire() {
    assert_eq!(HOOK_SCHEMA_VERSION, 1);
    assert_eq!(HOOK_MODULE_DEADLINE_MS, 3_000);
    assert_eq!(HookModuleId::ORDERED.len(), 16);
    assert_eq!(HookModuleId::ORDERED[0], HookModuleId::CortexStatus);
    assert_eq!(HookModuleId::ORDERED[15], HookModuleId::MemorySessionEnd);
    assert_eq!(
        serde_json::to_value(HookModuleId::MemoryRecall).unwrap(),
        json!("membrane.memory-recall")
    );
    assert_eq!(
        serde_json::to_value(HookModuleId::DiagnosticsCompletionFence).unwrap(),
        json!("membrane.diagnostics-completion-fence")
    );
    let names: BTreeSet<String> = HookModuleId::ORDERED
        .iter()
        .map(|id| {
            serde_json::to_value(id)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(names.len(), 16, "every module id has its own wire name");
    assert!(names.iter().all(|name| name.starts_with("membrane.")));
    for id in HookModuleId::ORDERED {
        let wire = serde_json::to_value(id).unwrap();
        assert_eq!(serde_json::from_value::<HookModuleId>(wire).unwrap(), id);
    }
    assert!(serde_json::from_value::<HookModuleId>(json!("membrane.unknown")).is_err());

    let status = HookModuleOutputV1::status(HookModuleState::Available, "ready", json!({ "n": 1 }));
    assert_eq!(
        (status.schema_version, status.kind.as_str()),
        (1, "membrane.hook.status")
    );
    let skipped = HookModuleOutputV1::skipped();
    assert_eq!(
        (skipped.state, skipped.reason.as_str(), &skipped.detail),
        (
            HookModuleState::Skipped,
            "event_not_applicable",
            &Value::Null
        )
    );
    let ok = HookModuleResultV1::skipped(HookModuleId::MemoryBump);
    let error = HookModuleResultV1::error(HookModuleId::MemoryNag, "boom");
    assert_eq!(
        (ok.status, error.status),
        (HookInvocationStatus::Ok, HookInvocationStatus::Error)
    );
    assert_eq!(error.error.as_deref(), Some("boom"));
    assert_eq!(
        aggregate_hook_status(std::slice::from_ref(&ok)),
        HookInvocationStatus::Ok
    );
    assert_eq!(
        aggregate_hook_status(&[ok.clone(), error.clone()]),
        HookInvocationStatus::Error
    );
    assert_eq!(aggregate_hook_status(&[]), HookInvocationStatus::Ok);
    assert_eq!(
        HookDispatchResultV1::new(EventKind::Stop, vec![ok, error]).status,
        HookInvocationStatus::Error
    );

    let blocked = |event: EventKind| {
        let output = HookModuleOutputV1::status(
            HookModuleState::Blocked,
            "fence_open",
            json!({ "detail": "tests not run", "additionalContext": "ctx" }),
        );
        HookDispatchResultV1::new(
            event,
            vec![HookModuleResultV1::ok(
                HookModuleId::DiagnosticsFence,
                output,
            )],
        )
    };

    let stop = serde_json::to_value(project_hook_host_response(blocked(EventKind::Stop))).unwrap();
    assert_eq!(
        (stop["decision"].as_str(), stop["reason"].as_str()),
        (Some("block"), Some("tests not run"))
    );
    assert!(
        stop.get("hookSpecificOutput").is_none(),
        "Stop carries top-level control only"
    );
    assert_eq!(stop["membraneHook"]["event"], "Stop");

    let pre =
        serde_json::to_value(project_hook_host_response(blocked(EventKind::PreToolUse))).unwrap();
    assert!(pre.get("decision").is_none());
    assert_eq!(pre["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        pre["hookSpecificOutput"]["permissionDecisionReason"],
        "tests not run"
    );
    assert_eq!(pre["hookSpecificOutput"]["additionalContext"], "ctx");

    let compact =
        serde_json::to_value(project_hook_host_response(blocked(EventKind::PreCompact))).unwrap();
    assert!(
        compact.get("hookSpecificOutput").is_none() && compact.get("decision").is_none(),
        "an event without a host projection is never given one"
    );

    // Round trip: the wire is the contract, so re-serialising reproduces it exactly.
    for kind in [
        EventKind::SessionStart,
        EventKind::UserPromptSubmit,
        EventKind::PreToolUse,
        EventKind::PostToolUse,
        EventKind::Stop,
        EventKind::PreCompact,
        EventKind::SessionEnd,
        EventKind::Other("CustomEvent".into()),
    ] {
        let response = project_hook_host_response(blocked(kind.clone()));
        let wire = serde_json::to_string(&response).unwrap();
        let back: HookHostResponseV1 =
            serde_json::from_str(&wire).unwrap_or_else(|error| panic!("{kind:?}: {error}"));
        assert_eq!(back.membrane_hook, response.membrane_hook, "{kind:?}");
        assert_eq!(serde_json::to_string(&back).unwrap(), wire, "{kind:?}");
    }
    let wire = serde_json::to_string(&project_hook_host_response(blocked(
        EventKind::SessionStart,
    )))
    .unwrap();
    assert!(
        serde_json::from_str::<HookHostResponseV1>(&wire.replacen('{', "{\"unexpected\":1,", 1))
            .is_err(),
        "unknown fields are refused"
    );
    let mut missing: Value = serde_json::from_str(
        &serde_json::to_string(&project_hook_host_response(blocked(EventKind::PreToolUse)))
            .unwrap(),
    )
    .unwrap();
    missing
        .as_object_mut()
        .unwrap()
        .remove("hookSpecificOutput");
    assert!(
        serde_json::from_value::<HookHostResponseV1>(missing).is_err(),
        "a projection event requires hookSpecificOutput on read"
    );
}
