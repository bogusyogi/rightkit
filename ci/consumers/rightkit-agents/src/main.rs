// rightkit-agents 0.2.0 consumed by exact registry pin with default features off and the features
// "claude" and "codex", the set CodeRight's driver host uses, outside the workspace.
//
// Proves, through the public API only, with inline transcripts and no vendor CLI:
// - the event model: AgentEvent serialises with a snake_case "type" tag and round-trips, the
//   Unknown policy and ParseError helpers, RateLimitState (observed data wins, a failed probe keeps
//   the last snapshot, unsupported never hides data);
// - the Claude Code stream-json side: build_args and build_args_with produce the exact argv
//   (config mode, resume, tools, budget, host approvals) and refuse bypassPermissions, a bad id or
//   mode, a resume option without a resume id and a fallback equal to the model; the stdin frames
//   (user turn, interrupt, permission allow and deny, initialize, set_model, set_permission_mode)
//   and next_request_id; StreamParser over a transcript (init, partial text deltas, text, thinking,
//   tool use and result, a subagent that starts and finishes, usage, a result with a permission
//   denial and a budget failure, compaction, rate limits, control requests and responses), unknown
//   lines kept as Unknown, and UnknownPolicy::FailClosed refusing unknown required kinds while
//   telemetry passes; parse_can_use_tool, parse_control_response, parse_initialize_response,
//   parse_interrupt_receipt, parse_rate_limit, parse_result_summary; answer_can_use_tool failing
//   closed with no receiver, a timeout or a disconnect;
// - the Codex app-server side: parse_line for responses, server requests and notifications,
//   approval requests carrying their event, bookkeeping notifications that produce nothing,
//   turn/failed and usage-limit errors, FailClosed refusing unknown notifications in the turn/,
//   item/ and serverRequest/ families and error, the method tables and PINNED_CODEX_VERSION;
//   the frames (approval decision, error response), build_args, thread_params and launch_env; and
//   CodexClient driving initialize, thread_start, turn_start, thread_resume, turn_interrupt and
//   respond_approval over in-memory streams, with RPC errors, a closed stream and a refused line;
// - vendor discovery with a stand-in CLI: this program, copied to a temporary directory under the
//   names claude and codex, answers --version, auth status, a hang and chatty output when launched
//   by that name. find_binaries and find_binary search PATH, probe reads the version and sign-in
//   (Codex is not asked for sign-in), detect caches, RIGHTKIT_DISABLE_VENDOR_CLIS turns every
//   probe off, claude_auth_status reads only the official boolean, and run_bounded caps output at
//   PROBE_OUTPUT_CAP, gives up at its deadline and returns None for a program that cannot start.
// Does not prove: the session feature (Session::spawn, approvals, cancel and tree teardown need a
// running vendor CLI), a real Claude Code or Codex process, sign-in, accounts or rate limits as a
// vendor really reports them, list_models, or the Codex schema snapshot drift test of the source
// repository. Nothing here is signed in to anything. Public API only.
// State: a per-process directory under the OS temp directory, removed at the end; one short-lived
// copy of this program per probe. No network, no credentials, no real user data. The longest
// wait is a 400 ms deadline.
use rightkit_agents::claude::{
    answer_can_use_tool, build_args, build_args_with, check_permission_mode,
    enable_permission_prompt, initialize_frame, interrupt_frame, next_request_id,
    parse_can_use_tool, parse_control_response, parse_initialize_response, parse_interrupt_receipt,
    parse_rate_limit, parse_result_summary, permission_allow_frame, permission_deny_frame,
    set_model_frame, set_permission_mode_frame, user_turn_frame, ClaudeExtras, ClaudeOptions,
    ConfigMode, PermissionAnswer, PermissionDecision, StreamParser, DEFAULT_PERMISSION_TIMEOUT,
    INTERRUPT_RECEIPT_CAPABILITY, NO_ANSWER_MESSAGE, PERMISSION_MODES,
};
use rightkit_agents::codex::{
    approval_response_frame, build_args as codex_args, error_response_frame,
    is_required_notification, launch_env, parse_line, parse_line_with, thread_params, ClientError,
    CodexClient, CodexOptions, Decision, Incoming, APPROVAL_REQUESTS, KNOWN_NOTIFICATIONS,
    PINNED_CODEX_VERSION,
};
use rightkit_agents::discovery::{
    claude_auth_status, detect, find_binaries, find_binary, probe, run_bounded, VendorCli,
    DISABLE_VENDOR_CLIS_ENV, PROBE_OUTPUT_CAP, PROBE_TIMEOUT,
};
use rightkit_agents::rate_limit::{RateLimitSnapshot, RateLimitState};
use rightkit_agents::{
    AgentEvent, ControlOutcome, ControlResponse, McpServerStatus, ParseError, PermissionDenial,
    RateLimitInfo, ResultSummary, SessionInit, UnknownPolicy, UsageCounts,
};
use serde_json::{json, Value};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn main() {
    // This program doubles as the stand-in vendor CLI when launched under the name claude or codex.
    if let Some(code) = stand_in_vendor_cli() {
        std::process::exit(code);
    }
    event_model();
    claude_argv_and_frames();
    claude_control_helpers();
    claude_stream_parser();
    claude_fail_closed();
    codex_lines();
    codex_frames_and_params();
    codex_client();
    let temp = TempDir::new();
    discovery(&temp);
    println!("OK rightkit-agents 0.2.0 (registry consumer, claude + codex)");
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

// ---- the event model -----------------------------------------------------------------------

fn event_model() {
    let text = AgentEvent::TextDelta { text: "hi".into() };
    assert_eq!(
        serde_json::to_value(&text).unwrap(),
        json!({ "type": "text_delta", "text": "hi" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::Compacted).unwrap(),
        json!({ "type": "compacted" })
    );
    let events = vec![
        AgentEvent::Init {
            session_id: "s1".into(),
            model: Some("model-a".into()),
            tools: strings(&["Bash"]),
            permission_mode: None,
        },
        AgentEvent::SessionInit(SessionInit {
            session_id: "s1".into(),
            tools: strings(&["Bash"]),
            mcp_servers: vec![McpServerStatus {
                name: "docs".into(),
                status: "connected".into(),
            }],
            ..SessionInit::default()
        }),
        AgentEvent::ToolUse {
            id: "t1".into(),
            name: "Read".into(),
            input: json!({ "file_path": "/work/a.txt" }),
            parent_tool_use_id: None,
        },
        AgentEvent::Usage {
            usage: UsageCounts {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_write: 4,
            },
        },
        AgentEvent::Result {
            cost_usd: Some(0.5),
            duration_ms: Some(10),
            num_turns: Some(1),
            permission_denials: vec![PermissionDenial {
                tool: "Bash".into(),
                tool_use_id: Some("t2".into()),
                input: json!({ "command": "ls" }),
            }],
            is_error: false,
            subtype: "success".into(),
        },
        AgentEvent::ResultDetail(ResultSummary {
            subtype: "success".into(),
            ..ResultSummary::default()
        }),
        AgentEvent::RateLimit(RateLimitInfo {
            status: "rejected".into(),
            resets_at: Some(1_700_000_000),
            ..RateLimitInfo::default()
        }),
        AgentEvent::ControlResponse(ControlResponse {
            request_id: "r1".into(),
            outcome: ControlOutcome::Error {
                message: "no".into(),
            },
        }),
        AgentEvent::Unknown {
            raw: json!({ "x": 1 }),
        },
    ];
    for event in events {
        let wire = serde_json::to_string(&event).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentEvent>(&wire).unwrap(),
            event,
            "{wire}"
        );
    }
    assert_eq!(
        serde_json::to_value(ControlOutcome::Success {
            payload: json!({ "ok": true })
        })
        .unwrap(),
        json!({ "outcome": "success", "payload": { "ok": true } })
    );

    assert_eq!(UnknownPolicy::default(), UnknownPolicy::Lenient);
    let refused = ParseError::unknown_required("turn/new");
    assert_eq!(refused.unknown_required_method(), Some("turn/new"));
    assert_eq!(ParseError("plain".into()).unknown_required_method(), None);
    assert_eq!(refused.to_string(), "unknown required event: turn/new");
    let boxed: Box<dyn std::error::Error> = Box::new(refused);
    assert!(boxed.to_string().contains("turn/new"));
    assert_eq!(
        UsageCounts::default(),
        UsageCounts {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0
        }
    );

    let snapshot = |status: &str| RateLimitSnapshot {
        status: status.to_owned(),
        ..RateLimitSnapshot::default()
    };
    let mut state = RateLimitState::default();
    assert_eq!(state, RateLimitState::NoData);
    assert!(state.snapshot().is_none());
    state.mark_unsupported("no limits here");
    assert!(matches!(&state, RateLimitState::Unsupported { reason } if reason == "no limits here"));
    state.observe(snapshot("allowed"));
    assert_eq!(
        state.snapshot(),
        Some(&snapshot("allowed")),
        "observed data replaces unsupported"
    );
    state.mark_unsupported("ignored");
    assert!(
        matches!(state, RateLimitState::Known { .. }),
        "unsupported never hides known data"
    );
    state.probe_failed("timeout");
    assert_eq!(
        state.snapshot(),
        Some(&snapshot("allowed")),
        "a failed probe keeps the last snapshot"
    );
    assert!(
        matches!(&state, RateLimitState::ProbeFailed { message, last_known: Some(_) } if message == "timeout")
    );
    let mut fresh = RateLimitState::default();
    fresh.probe_failed("down");
    assert!(fresh.snapshot().is_none());
}

// ---- Claude argv and frames ----------------------------------------------------------------

fn claude_argv_and_frames() {
    let base = [
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
    ];
    assert_eq!(
        build_args(&ClaudeOptions::default()).unwrap(),
        strings(&base)
    );
    assert_eq!(ConfigMode::default(), ConfigMode::Full);

    let options = ClaudeOptions {
        config: ConfigMode::Bare,
        model: Some("model-a".into()),
        resume_session_id: Some("sess_1".into()),
        allowed_tools: strings(&["Read", "Edit"]),
        disallowed_tools: strings(&["Bash"]),
        permission_mode: Some("acceptEdits".into()),
        max_budget_usd: Some(1.5),
        append_system_prompt: Some("Be brief.".into()),
        settings_path: Some("/work/settings.json".into()),
        include_partial_messages: true,
        host_approvals: true,
        ..ClaudeOptions::default()
    };
    let mut expected = strings(&base);
    expected.extend(strings(&[
        "--include-partial-messages",
        "--bare",
        "--model",
        "model-a",
        "--resume",
        "sess_1",
        "--allowedTools",
        "Read",
        "Edit",
        "--disallowedTools",
        "Bash",
        "--permission-mode",
        "acceptEdits",
        "--max-budget-usd",
        "1.5",
        "--append-system-prompt",
        "Be brief.",
        "--permission-prompt-tool",
        "stdio",
        "--settings",
        "/work/settings.json",
    ]));
    assert_eq!(build_args(&options).unwrap(), expected);

    let mut extras = ClaudeExtras::default();
    extras.tools = Some(vec!["Read".into(), "Edit".into()]);
    extras.no_chrome = true;
    extras.fork_session = true;
    extras.resume_session_at = Some("msg_9".into());
    extras.max_turns = Some(7);
    extras.fallback_model = Some("model-b".into());
    let args = build_args_with(&options, &extras).unwrap();
    for pair in [
        ["--tools", "Read,Edit"],
        ["--resume-session-at", "msg_9"],
        ["--max-turns", "7"],
        ["--fallback-model", "model-b"],
    ] {
        assert!(args.windows(2).any(|w| w == pair), "{pair:?} in {args:?}");
    }
    assert!(
        args.contains(&"--no-chrome".to_owned()) && args.contains(&"--fork-session".to_owned())
    );
    let mut empty_tools = ClaudeExtras::default();
    empty_tools.tools = Some(Vec::new());
    let args = build_args_with(&ClaudeOptions::default(), &empty_tools).unwrap();
    assert!(
        args.windows(2).any(|w| w == ["--tools", ""]),
        "an empty list disables every built-in tool"
    );

    let refuse = |options: ClaudeOptions| build_args(&options).unwrap_err();
    assert!(refuse(ClaudeOptions {
        session_id: Some("a".into()),
        resume_session_id: Some("b".into()),
        ..Default::default()
    })
    .contains("mutually exclusive"));
    assert!(refuse(ClaudeOptions {
        permission_mode: Some("bypassPermissions".into()),
        ..Default::default()
    })
    .contains("bypassPermissions"));
    assert!(refuse(ClaudeOptions {
        permission_mode: Some("whatever".into()),
        ..Default::default()
    })
    .contains("must be one of"));
    for bad in ["", "-flag", "has space", "semi;colon", &"x".repeat(257)] {
        let error = refuse(ClaudeOptions {
            model: Some(bad.to_owned()),
            ..Default::default()
        });
        assert!(
            error.contains("model is not a valid identifier"),
            "{bad:?}: {error}"
        );
        assert!(
            !error.contains(bad) || bad.is_empty(),
            "the message names the field, never the value"
        );
    }
    assert!(build_args(&ClaudeOptions {
        model: Some("vendor/model-a:1[big]".into()),
        ..Default::default()
    })
    .is_ok());
    let mut orphan = ClaudeExtras::default();
    orphan.fork_session = true;
    assert!(build_args_with(&ClaudeOptions::default(), &orphan)
        .unwrap_err()
        .contains("fork_session requires"));
    let mut orphan = ClaudeExtras::default();
    orphan.resume_session_at = Some("m".into());
    assert!(build_args_with(&ClaudeOptions::default(), &orphan)
        .unwrap_err()
        .contains("resume_session_at requires"));
    let mut same = ClaudeExtras::default();
    same.fallback_model = Some("model-a".into());
    assert!(build_args_with(
        &ClaudeOptions {
            model: Some("model-a".into()),
            ..Default::default()
        },
        &same
    )
    .unwrap_err()
    .contains("must differ"));
    let mut bad_tool = ClaudeExtras::default();
    bad_tool.tools = Some(vec!["Read,Edit".into()]);
    assert!(build_args_with(&ClaudeOptions::default(), &bad_tool).is_err());

    assert_eq!(
        PERMISSION_MODES,
        ["default", "acceptEdits", "dontAsk", "plan"]
    );
    for mode in PERMISSION_MODES {
        assert!(check_permission_mode(mode).is_ok());
    }
    assert!(
        check_permission_mode("bypassPermissions").is_err() && check_permission_mode("").is_err()
    );
    let mut args = strings(&["--print"]);
    enable_permission_prompt(&mut args);
    enable_permission_prompt(&mut args);
    assert_eq!(
        args,
        strings(&["--print", "--permission-prompt-tool", "stdio"]),
        "idempotent"
    );
    let mut args = strings(&["--permission-prompt-tool", "mcp__other"]);
    enable_permission_prompt(&mut args);
    assert_eq!(
        args,
        strings(&["--permission-prompt-tool", "stdio"]),
        "an existing flag is pointed at the stdio channel"
    );

    assert_eq!(
        user_turn_frame("summarise the diff"),
        "{\"message\":{\"content\":\"summarise the diff\",\"role\":\"user\"},\"type\":\"user\"}\n"
    );
    let frame = |line: String| -> Value {
        assert!(
            line.ends_with('\n') && !line[..line.len() - 1].contains('\n'),
            "one newline-terminated line"
        );
        serde_json::from_str(&line).unwrap()
    };
    assert_eq!(
        frame(interrupt_frame("r1")),
        json!({ "type": "control_request", "request_id": "r1", "request": { "subtype": "interrupt" } })
    );
    let allow = frame(permission_allow_frame(
        "r2",
        &json!({ "file_path": "/work/a.txt" }),
    ));
    assert_eq!(
        allow["response"]["response"],
        json!({ "behavior": "allow", "updatedInput": { "file_path": "/work/a.txt" } })
    );
    assert_eq!(
        (
            allow["type"].as_str(),
            allow["response"]["subtype"].as_str(),
            allow["response"]["request_id"].as_str()
        ),
        (Some("control_response"), Some("success"), Some("r2"))
    );
    let deny = frame(permission_deny_frame("r3", "not now", true));
    assert_eq!(
        deny["response"]["response"],
        json!({ "behavior": "deny", "message": "not now", "interrupt": true })
    );
    assert_eq!(
        frame(initialize_frame("r4", None))["request"],
        json!({ "subtype": "initialize" })
    );
    let hooks = json!({ "PreToolUse": [] });
    assert_eq!(
        frame(initialize_frame("r4", Some(&hooks)))["request"]["hooks"],
        hooks
    );
    assert_eq!(
        frame(set_model_frame("r5", Some("model-a")).unwrap())["request"],
        json!({ "subtype": "set_model", "model": "model-a" })
    );
    assert_eq!(
        frame(set_model_frame("r5", None).unwrap())["request"]["model"],
        Value::Null
    );
    assert!(set_model_frame("r5", Some("bad value")).is_err());
    assert_eq!(
        frame(set_permission_mode_frame("r6", "plan").unwrap())["request"],
        json!({ "subtype": "set_permission_mode", "mode": "plan" })
    );
    assert!(set_permission_mode_frame("r6", "bypassPermissions").is_err());
    let (first, second) = (next_request_id("probe"), next_request_id("probe"));
    assert!(first.starts_with("rk-probe-") && second.starts_with("rk-probe-") && first != second);
}

// ---- Claude control helpers ----------------------------------------------------------------

fn claude_control_helpers() {
    let success = json!({ "type": "control_response", "response": { "subtype": "success", "request_id": "r1", "response": { "ok": true } } });
    assert_eq!(
        parse_control_response(&success),
        Some(ControlResponse {
            request_id: "r1".into(),
            outcome: ControlOutcome::Success {
                payload: json!({ "ok": true })
            }
        })
    );
    let failure = json!({ "type": "control_response", "response": { "subtype": "error", "request_id": "r2", "error": "unsupported" } });
    assert_eq!(
        parse_control_response(&failure).unwrap().outcome,
        ControlOutcome::Error {
            message: "unsupported".into()
        }
    );
    assert!(
        parse_control_response(&json!({ "response": { "subtype": "success" } })).is_none(),
        "no request id"
    );
    assert!(parse_control_response(
        &json!({ "response": { "subtype": "other", "request_id": "r" } })
    )
    .is_none());

    let payload = json!({ "accountInfo": { "email": "user@example.test", "org": "Example Org", "plan": "team" }, "models": ["model-a", { "value": "model-b" }, 7] });
    let parsed = parse_initialize_response(&payload);
    let account = parsed.account.expect("account info");
    assert_eq!(
        (
            account.email.as_deref(),
            account.organization.as_deref(),
            account.plan.as_deref()
        ),
        (Some("user@example.test"), Some("Example Org"), Some("team"))
    );
    assert_eq!(parsed.models, strings(&["model-a", "model-b"]));
    assert!(parse_initialize_response(&json!({})).account.is_none());

    let capabilities = strings(&[INTERRUPT_RECEIPT_CAPABILITY]);
    assert_eq!(INTERRUPT_RECEIPT_CAPABILITY, "interrupt_receipt_v1");
    let receipt = json!({ "queuedUserMessages": ["a", "b"] });
    assert_eq!(
        parse_interrupt_receipt(&receipt, &capabilities).map(|r| r.queued_user_messages),
        Some(2)
    );
    assert!(
        parse_interrupt_receipt(&receipt, &[]).is_none(),
        "only when the session advertised the capability"
    );

    let request = json!({ "type": "control_request", "request_id": "req_1", "request": { "subtype": "can_use_tool", "tool_name": "Edit", "input": { "file_path": "/work/a.txt" }, "tool_use_id": "tu1", "agent_id": "ag1", "blocked_path": "/work" } });
    let ask = parse_can_use_tool(&request).expect("can_use_tool");
    assert_eq!(
        (
            ask.request_id.as_str(),
            ask.tool_name.as_str(),
            ask.tool_use_id.as_deref(),
            ask.agent_id.as_deref(),
            ask.blocked_path.as_deref()
        ),
        ("req_1", "Edit", Some("tu1"), Some("ag1"), Some("/work"))
    );
    assert!(parse_can_use_tool(&json!({ "request": { "subtype": "interrupt" } })).is_none());
    assert!(parse_can_use_tool(&json!({ "type": "user" })).is_none());

    assert_eq!(DEFAULT_PERMISSION_TIMEOUT, Duration::from_secs(300));
    let original = ask.input.clone();
    assert_eq!(
        PermissionDecision::from_answer(PermissionAnswer::Allow, &original),
        PermissionDecision::Allow {
            updated_input: original.clone()
        }
    );
    assert_eq!(
        PermissionDecision::from_answer(PermissionAnswer::Deny { message: None }, &original),
        PermissionDecision::Deny {
            message: "Denied by the host".into()
        }
    );
    let decision = PermissionDecision::Deny {
        message: "no".into(),
    };
    let denial: Value = serde_json::from_str(&decision.frame("req_1")).unwrap();
    assert_eq!(
        denial["response"]["response"],
        json!({ "behavior": "deny", "message": "no", "interrupt": false })
    );

    // The host answers over a channel it owns; every failure to answer is a denial.
    let (tx, rx) = mpsc::channel();
    tx.send(PermissionAnswer::Allow).unwrap();
    assert_eq!(
        answer_can_use_tool(&ask, Some(rx), Duration::from_secs(5)),
        PermissionDecision::Allow {
            updated_input: original.clone()
        }
    );
    let (tx, rx) = mpsc::channel();
    tx.send(PermissionAnswer::Deny {
        message: Some("review first".into()),
    })
    .unwrap();
    assert_eq!(
        answer_can_use_tool(&ask, Some(rx), Duration::from_secs(5)),
        PermissionDecision::Deny {
            message: "review first".into()
        }
    );
    let no_answer = PermissionDecision::Deny {
        message: NO_ANSWER_MESSAGE.into(),
    };
    assert_eq!(NO_ANSWER_MESSAGE, "Denied: no host answer in time");
    assert_eq!(
        answer_can_use_tool(&ask, None, Duration::from_secs(5)),
        no_answer,
        "no receiver"
    );
    let (tx, rx) = mpsc::channel::<PermissionAnswer>();
    let started = Instant::now();
    assert_eq!(
        answer_can_use_tool(&ask, Some(rx), Duration::from_millis(100)),
        no_answer,
        "timeout"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        tx.send(PermissionAnswer::Allow).is_err(),
        "a late answer finds the receiver gone"
    );
    let (tx, rx) = mpsc::channel::<PermissionAnswer>();
    drop(tx);
    assert_eq!(
        answer_can_use_tool(&ask, Some(rx), Duration::from_secs(5)),
        no_answer,
        "disconnect"
    );

    let info = parse_rate_limit(
        &json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "allowed_warning", "rateLimitType": "five_hour", "resetsAt": 1_700_000_000u64, "utilization": 0.82, "overageStatus": "rejected", "errorCode": "x" } }),
    );
    assert_eq!(
        (
            info.status.as_str(),
            info.rate_limit_type.as_deref(),
            info.resets_at,
            info.utilization,
            info.overage_status.as_deref(),
            info.error_code.as_deref()
        ),
        (
            "allowed_warning",
            Some("five_hour"),
            Some(1_700_000_000),
            Some(0.82),
            Some("rejected"),
            Some("x")
        )
    );
    assert_eq!(
        parse_rate_limit(&json!({ "status": "allowed" })).status,
        "allowed",
        "fields may sit on the message itself"
    );
    assert_eq!(parse_rate_limit(&json!({})).status, "");

    let summary = parse_result_summary(&json!({
        "subtype": "interrupted", "is_error": true, "errors": ["first", { "message": "second" }],
        "usage": { "input_tokens": 1, "output_tokens": 2, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 4 },
        "modelUsage": { "model-a": { "inputTokens": 5, "outputTokens": 6, "cacheReadInputTokens": 7, "cacheCreationInputTokens": 8 } }
    }));
    assert!(summary.interrupted && summary.is_error);
    assert_eq!(summary.failure_message.as_deref(), Some("first\nsecond"));
    assert_eq!(
        summary.usage,
        Some(UsageCounts {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4
        })
    );
    assert_eq!(
        summary.model_usage["model-a"],
        UsageCounts {
            input: 5,
            output: 6,
            cache_read: 7,
            cache_write: 8
        }
    );
    let failed = parse_result_summary(
        &json!({ "subtype": "error_during_execution", "is_error": true, "result": "it broke" }),
    );
    assert_eq!(
        (failed.interrupted, failed.failure_message.as_deref()),
        (false, Some("it broke"))
    );
    assert_eq!(
        parse_result_summary(&json!({ "subtype": "success", "result": "fine" })).failure_message,
        None
    );
}

// ---- Claude stream parser ------------------------------------------------------------------

const TRANSCRIPT: &[&str] = &[
    r#"{"type":"system","subtype":"hook_started","session_id":"s1"}"#,
    r#"{"type":"system","subtype":"init","session_id":"s1","model":"model-a","cwd":"/work","tools":["Bash","Read","Task"],"permissionMode":"default","capabilities":["interrupt_receipt_v1"],"mcp_servers":[{"name":"docs","status":"connected"}]}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_0","name":"Read"}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}"#,
    r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1"}}}"#,
    r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","content":[{"type":"thinking","thinking":"plan"},{"type":"text","text":"Hello"},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/work/a.txt"}},{"type":"tool_use","id":"toolu_task","name":"Task","input":{"description":"Survey the repo"}}],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}},"parent_tool_use_id":null}"#,
    r#"{"type":"assistant","message":{"id":"msg_2","role":"assistant","content":[{"type":"text","text":"inner"}]},"parent_tool_use_id":"toolu_task"}"#,
    r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"line 1"},{"type":"image"},{"type":"text","text":"line 2"}]}]},"parent_tool_use_id":null}"#,
    r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_task","is_error":true,"content":"gave up"}]},"parent_tool_use_id":null}"#,
    r#"{"type":"system","subtype":"compact_boundary"}"#,
    r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#,
    r#"{"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{"file_path":"/work/a.txt"}}}"#,
    r#"{"type":"control_response","response":{"subtype":"success","request_id":"rk-interrupt-1","response":{}}}"#,
    r#"{"type":"future_message_kind","payload":{"x":1}}"#,
    r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":4200,"num_turns":2,"result":"Hello","total_cost_usd":0.0123,"usage":{"input_tokens":10,"output_tokens":5},"permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_rm","tool_input":{"command":"rm -rf build"}}]}"#,
];

fn claude_stream_parser() {
    let mut parser = StreamParser::new();
    let mut events: Vec<Vec<AgentEvent>> = Vec::new();
    for line in TRANSCRIPT {
        events.push(
            parser
                .parse_line(line)
                .unwrap_or_else(|error| panic!("{line}: {error}")),
        );
    }
    assert!(events[0].is_empty(), "hook telemetry produces nothing");
    assert_eq!(
        events[1],
        [
            AgentEvent::Init {
                session_id: "s1".into(),
                model: Some("model-a".into()),
                tools: strings(&["Bash", "Read", "Task"]),
                permission_mode: Some("default".into())
            },
            AgentEvent::SessionInit(SessionInit {
                session_id: "s1".into(),
                model: Some("model-a".into()),
                cwd: Some("/work".into()),
                tools: strings(&["Bash", "Read", "Task"]),
                permission_mode: Some("default".into()),
                capabilities: strings(&["interrupt_receipt_v1"]),
                mcp_servers: vec![McpServerStatus {
                    name: "docs".into(),
                    status: "connected".into()
                }],
            }),
        ]
    );
    assert_eq!(
        events[2],
        [AgentEvent::ContentBlockStart {
            index: Some(0),
            block_type: "tool_use".into(),
            id: Some("toolu_0".into()),
            name: Some("Read".into())
        }]
    );
    assert_eq!(
        events[3],
        [
            AgentEvent::TextDelta { text: "Hel".into() },
            AgentEvent::TextBlockDelta {
                index: Some(0),
                text: "Hel".into()
            }
        ]
    );
    assert!(events[4].is_empty());
    assert_eq!(
        events[5],
        [
            AgentEvent::Thinking {
                text: "plan".into(),
                parent_tool_use_id: None
            },
            AgentEvent::Text {
                text: "Hello".into(),
                parent_tool_use_id: None
            },
            AgentEvent::ToolUse {
                id: "toolu_1".into(),
                name: "Read".into(),
                input: json!({ "file_path": "/work/a.txt" }),
                parent_tool_use_id: None
            },
            AgentEvent::SubagentStarted {
                id: "toolu_task".into(),
                description: Some("Survey the repo".into()),
                parent_tool_use_id: None
            },
            AgentEvent::ToolUse {
                id: "toolu_task".into(),
                name: "Task".into(),
                input: json!({ "description": "Survey the repo" }),
                parent_tool_use_id: None
            },
            AgentEvent::Usage {
                usage: UsageCounts {
                    input: 10,
                    output: 5,
                    cache_read: 100,
                    cache_write: 20
                }
            },
        ]
    );
    assert_eq!(
        events[6],
        [AgentEvent::Text {
            text: "inner".into(),
            parent_tool_use_id: Some("toolu_task".into())
        }]
    );
    assert_eq!(
        events[7],
        [
            AgentEvent::ToolResultContent {
                id: "toolu_1".into(),
                is_error: false,
                content: "line 1\nline 2".into(),
                parent_tool_use_id: None
            },
            AgentEvent::ToolResult {
                id: "toolu_1".into(),
                is_error: false,
                parent_tool_use_id: None
            },
        ],
        "text parts are joined with newlines; a plain tool is not a subagent"
    );
    assert_eq!(
        events[8],
        [
            AgentEvent::ToolResultContent {
                id: "toolu_task".into(),
                is_error: true,
                content: "gave up".into(),
                parent_tool_use_id: None
            },
            AgentEvent::ToolResult {
                id: "toolu_task".into(),
                is_error: true,
                parent_tool_use_id: None
            },
            AgentEvent::SubagentFinished {
                id: "toolu_task".into(),
                is_error: true
            },
        ]
    );
    assert_eq!(events[9], [AgentEvent::Compacted]);
    assert_eq!(
        events[10],
        [AgentEvent::RateLimit(RateLimitInfo {
            status: "allowed".into(),
            ..RateLimitInfo::default()
        })]
    );
    assert_eq!(
        events[11],
        [AgentEvent::ApprovalRequested {
            id: "req_1".into(),
            tool: "Edit".into(),
            input: json!({ "file_path": "/work/a.txt" })
        }]
    );
    assert_eq!(
        events[12],
        [AgentEvent::ControlResponse(ControlResponse {
            request_id: "rk-interrupt-1".into(),
            outcome: ControlOutcome::Success { payload: json!({}) }
        })]
    );
    assert_eq!(
        events[13],
        [AgentEvent::Unknown {
            raw: json!({ "type": "future_message_kind", "payload": { "x": 1 } })
        }],
        "unknown kinds degrade, never fail"
    );
    match events[14].as_slice() {
        [AgentEvent::ResultDetail(detail), AgentEvent::Result {
            cost_usd,
            duration_ms,
            num_turns,
            permission_denials,
            is_error,
            subtype,
        }] => {
            assert_eq!(
                (detail.subtype.as_str(), detail.is_error),
                ("success", false)
            );
            assert_eq!(
                detail.usage,
                Some(UsageCounts {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0
                })
            );
            assert_eq!(
                (
                    *cost_usd,
                    *duration_ms,
                    *num_turns,
                    *is_error,
                    subtype.as_str()
                ),
                (Some(0.0123), Some(4200), Some(2), false, "success")
            );
            assert_eq!(
                permission_denials,
                &[PermissionDenial {
                    tool: "Bash".into(),
                    tool_use_id: Some("toolu_rm".into()),
                    input: json!({ "command": "rm -rf build" })
                }]
            );
        }
        other => panic!("unexpected result events: {other:?}"),
    }

    // A budget failure is an error result with its subtype.
    let budget = parser.parse_line(r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"duration_ms":30000,"num_turns":6,"total_cost_usd":1.0042,"permission_denials":[]}"#).unwrap();
    assert!(
        matches!(&budget[1], AgentEvent::Result { is_error: true, subtype, num_turns: Some(6), .. } if subtype == "error_max_budget_usd")
    );

    assert!(parser.parse_line("{not json").is_err());
    assert_eq!(
        parser.parse_line(r#"{"no":"type"}"#),
        Err(ParseError("missing type".into()))
    );
    assert_eq!(
        parser
            .parse_line(r#"{"type":"assistant","message":{"content":[{"type":"mystery"}]}}"#)
            .unwrap(),
        Vec::<AgentEvent>::new()
    );
}

fn claude_fail_closed() {
    let mut strict = StreamParser::with_policy(UnknownPolicy::FailClosed);
    for line in TRANSCRIPT {
        let method = strict
            .parse_line(line)
            .err()
            .and_then(|e| e.unknown_required_method().map(str::to_owned));
        if line.contains("future_message_kind") {
            assert_eq!(method.as_deref(), Some("future_message_kind"));
        } else {
            assert_eq!(method, None, "{line}");
        }
    }
    let mut refused = |line: &str| {
        strict
            .parse_line(line)
            .err()
            .and_then(|e| e.unknown_required_method().map(str::to_owned))
    };
    assert_eq!(
        refused(r#"{"type":"system","subtype":"tool_denied"}"#).as_deref(),
        Some("system:tool_denied"),
        "a subtype named for execution truth fails closed"
    );
    assert_eq!(
        refused(r#"{"type":"system","subtype":"quiet","permission_denials":[]}"#).as_deref(),
        Some("system:quiet"),
        "so does a payload that carries it"
    );
    for telemetry in [
        "hook_response",
        "status",
        "api_retry",
        "task_started",
        "informational",
    ] {
        assert_eq!(
            refused(&format!(r#"{{"type":"system","subtype":"{telemetry}"}}"#)),
            None,
            "{telemetry}"
        );
    }
    assert_eq!(
        refused(r#"{"type":"system","subtype":"something_new"}"#),
        None,
        "an unknown but harmless subtype passes"
    );
}

// ---- Codex lines ---------------------------------------------------------------------------

fn codex_lines() {
    assert_eq!(PINNED_CODEX_VERSION, "0.161.0");
    assert_eq!(
        APPROVAL_REQUESTS,
        [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval"
        ]
    );
    assert_eq!(
        KNOWN_NOTIFICATIONS,
        [
            "thread/started",
            "turn/started",
            "turn/completed",
            "item/started",
            "item/completed",
            "item/agentMessage/delta",
            "thread/tokenUsage/updated",
            "thread/compacted",
            "serverRequest/resolved"
        ]
    );

    assert_eq!(
        parse_line(r#"{"id":1,"result":{"userAgent":"codex/0.161.0"}}"#).unwrap(),
        Incoming::Response {
            id: json!(1),
            result: Some(json!({ "userAgent": "codex/0.161.0" })),
            error: None
        }
    );
    assert!(matches!(
        parse_line(r#"{"id":"x","error":{"code":-1,"message":"no"}}"#).unwrap(),
        Incoming::Response {
            error: Some(_),
            result: None,
            ..
        }
    ));
    assert_eq!(
        parse_line(r#"{"jsonrpc":"2.0"}"#),
        Err(ParseError("missing id and method".into()))
    );
    assert_eq!(
        parse_line("[1]"),
        Err(ParseError("root must be an object".into()))
    );
    assert!(parse_line("{nope").is_err());

    let notification = |line: &str| match parse_line(line).unwrap() {
        Incoming::Notification { method, events } => (method, events),
        other => panic!("expected a notification, got {other:?}"),
    };
    let (method, events) = notification(
        r#"{"method":"thread/started","params":{"thread":{"id":"thr_1","model":"model-a"}}}"#,
    );
    assert_eq!(method, "thread/started");
    assert_eq!(
        events,
        [AgentEvent::Init {
            session_id: "thr_1".into(),
            model: Some("model-a".into()),
            tools: vec![],
            permission_mode: None
        }]
    );
    assert_eq!(
        notification(r#"{"method":"item/agentMessage/delta","params":{"delta":"Run"}}"#).1,
        [AgentEvent::TextDelta { text: "Run".into() }]
    );
    assert_eq!(notification(r#"{"method":"item/started","params":{"item":{"type":"commandExecution","id":"c1","command":"ls"}}}"#).1[0], AgentEvent::ToolUse { id: "c1".into(), name: "commandExecution".into(), input: json!({ "type": "commandExecution", "id": "c1", "command": "ls" }), parent_tool_use_id: None });
    assert_eq!(notification(r#"{"method":"item/completed","params":{"item":{"type":"commandExecution","id":"c1","status":"declined"}}}"#).1, [AgentEvent::ToolResult { id: "c1".into(), is_error: true, parent_tool_use_id: None }]);
    assert_eq!(notification(r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m","text":"Done"}}}"#).1, [AgentEvent::Text { text: "Done".into(), parent_tool_use_id: None }]);
    assert_eq!(notification(r#"{"method":"item/completed","params":{"item":{"type":"reasoning","summary":["a","b"]}}}"#).1, [AgentEvent::Thinking { text: "a\nb".into(), parent_tool_use_id: None }]);
    let sub_start = notification(r#"{"method":"item/started","params":{"item":{"type":"collabAgentToolCall","id":"s1","prompt":"Survey"}}}"#).1;
    assert!(
        matches!(&sub_start[0], AgentEvent::SubagentStarted { id, description: Some(d), .. } if id == "s1" && d == "Survey")
    );
    let sub_end = notification(r#"{"method":"item/completed","params":{"item":{"type":"collabAgentToolCall","id":"s1","status":"completed"}}}"#).1;
    assert!(
        matches!(&sub_end[1], AgentEvent::SubagentFinished { id, is_error: false } if id == "s1")
    );
    assert_eq!(notification(r#"{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"total":{"inputTokens":120,"outputTokens":40,"cachedInputTokens":60,"cacheWriteInputTokens":2}}}}"#).1, [AgentEvent::Usage { usage: UsageCounts { input: 120, output: 40, cache_read: 60, cache_write: 2 } }]);
    assert_eq!(
        notification(r#"{"method":"thread/compacted","params":{}}"#).1,
        [AgentEvent::Compacted]
    );
    assert_eq!(
        notification(r#"{"method":"serverRequest/resolved","params":{"requestId":"srv_1"}}"#).1,
        [AgentEvent::ApprovalResolved { id: "srv_1".into() }]
    );
    assert_eq!(notification(r#"{"method":"turn/completed","params":{"turn":{"id":"t1","status":"completed","durationMs":2500}}}"#).1, [AgentEvent::Result { cost_usd: None, duration_ms: Some(2500), num_turns: Some(1), permission_denials: vec![], is_error: false, subtype: "completed".into() }]);
    assert!(
        matches!(&notification(r#"{"method":"turn/completed","params":{"turn":{"status":"failed"}}}"#).1[0], AgentEvent::Result { is_error: true, subtype, .. } if subtype == "failed")
    );
    let limited = notification(r#"{"method":"turn/failed","params":{"turn":{"id":"t1","durationMs":900},"error":{"message":"You hit your usage limit","codexErrorInfo":"usageLimitExceeded","resetsAt":1700000000}}}"#).1;
    assert!(
        matches!(&limited[0], AgentEvent::Result { is_error: true, subtype, duration_ms: Some(900), .. } if subtype == "failed")
    );
    assert!(
        matches!(&limited[1], AgentEvent::RateLimit(info) if info.status == "rejected" && info.resets_at == Some(1_700_000_000))
    );
    assert_eq!(
        notification(r#"{"method":"turn/failed","params":{"error":{"message":"overloaded"}}}"#)
            .1
            .len(),
        1,
        "a plain failure has no rate limit"
    );
    assert!(matches!(
        &notification(r#"{"method":"error","params":{"message":"Usage limit reached"}}"#).1[0],
        AgentEvent::RateLimit(_)
    ));
    for quiet in [
        "turn/started",
        "item/reasoning/textDelta",
        "account/rateLimits/updated",
        "warning",
        "thread/status/changed",
    ] {
        assert!(
            notification(&format!(r#"{{"method":"{quiet}","params":{{}}}}"#))
                .1
                .is_empty(),
            "{quiet}"
        );
    }
    assert!(matches!(
        &notification(r#"{"method":"some/future/notification","params":{"x":1}}"#).1[0],
        AgentEvent::Unknown { .. }
    ));

    match parse_line(r#"{"jsonrpc":"2.0","id":"srv_1","method":"item/commandExecution/requestApproval","params":{"itemId":"c1","command":"rm -rf build"}}"#).unwrap() {
        Incoming::ServerRequest { id, method, event: Some(AgentEvent::ApprovalRequested { id: event_id, tool, input }), .. } => {
            assert_eq!((id, method.as_str(), event_id.as_str(), tool.as_str()), (json!("srv_1"), "item/commandExecution/requestApproval", "srv_1", "item/commandExecution/requestApproval"));
            assert_eq!(input["command"], "rm -rf build");
        }
        other => panic!("expected an approval request, got {other:?}"),
    }
    assert!(matches!(
        parse_line(r#"{"id":9,"method":"some/other/request","params":{}}"#).unwrap(),
        Incoming::ServerRequest { event: None, .. }
    ));

    for method in ["turn/new", "item/new", "serverRequest/new", "error"] {
        assert!(is_required_notification(method), "{method}");
        let line = format!(r#"{{"method":"{method}","params":{{}}}}"#);
        let refusal = parse_line_with(&line, UnknownPolicy::FailClosed).unwrap_err();
        assert_eq!(refusal.unknown_required_method(), Some(method));
        assert!(parse_line_with(&line, UnknownPolicy::Lenient).is_ok());
    }
    assert!(!is_required_notification("thread/new"));
    assert!(
        parse_line_with(
            r#"{"method":"thread/new","params":{}}"#,
            UnknownPolicy::FailClosed
        )
        .is_ok(),
        "an unknown notification outside the required families passes"
    );
    assert!(
        parse_line_with(
            r#"{"method":"turn/completed","params":{"turn":{}}}"#,
            UnknownPolicy::FailClosed
        )
        .is_ok(),
        "known methods always pass"
    );
}

// ---- Codex frames, params and client -------------------------------------------------------

fn codex_frames_and_params() {
    assert_eq!(
        approval_response_frame(&json!("srv_1"), Decision::Accept),
        "{\"id\":\"srv_1\",\"jsonrpc\":\"2.0\",\"result\":{\"decision\":\"accept\"}}\n"
    );
    for (decision, wire) in [
        (Decision::Accept, "accept"),
        (Decision::AcceptForSession, "acceptForSession"),
        (Decision::Decline, "decline"),
        (Decision::Cancel, "cancel"),
    ] {
        let frame: Value =
            serde_json::from_str(&approval_response_frame(&json!(7), decision)).unwrap();
        assert_eq!(
            (frame["id"].clone(), frame["result"]["decision"].as_str()),
            (json!(7), Some(wire))
        );
    }
    let error: Value = serde_json::from_str(&error_response_frame(
        &json!("srv_2"),
        -32601,
        "method not found",
    ))
    .unwrap();
    assert_eq!(
        error,
        json!({ "jsonrpc": "2.0", "id": "srv_2", "error": { "code": -32601, "message": "method not found" } })
    );

    assert_eq!(codex_args(), strings(&["app-server"]));
    assert_eq!(
        thread_params(&CodexOptions::default(), None),
        json!({ "approvalPolicy": "on-request" }),
        "approvals reach the host by default"
    );
    let options = CodexOptions {
        model: Some("model-a".into()),
        approval_policy: Some("never".into()),
        sandbox: Some("read-only".into()),
        resume_thread_id: Some("thr_1".into()),
        thread_params: Some(json!({ "approvalPolicy": "untrusted", "extra": 1 })),
    };
    assert_eq!(
        thread_params(&options, Some("/work")),
        json!({ "model": "model-a", "cwd": "/work", "approvalPolicy": "untrusted", "sandbox": "read-only", "threadId": "thr_1", "extra": 1 }),
        "extra params are merged over the built ones"
    );
    let env = launch_env(Path::new("/state/codex"));
    assert_eq!(env.len(), 1);
    assert_eq!(
        (env[0].0.to_str(), env[0].1.to_str()),
        (Some("CODEX_HOME"), Some("/state/codex"))
    );
}

fn codex_client() {
    let script = [
        r#"{"id":1,"result":{"userAgent":"codex/0.161.0"}}"#,
        r#"{"method":"thread/started","params":{"thread":{"id":"thr_1","model":"model-a"}}}"#,
        r#"{"id":2,"result":{"thread":{"id":"thr_1"}}}"#,
        r#"{"method":"turn/started","params":{"threadId":"thr_1","turn":{"id":"turn_1"}}}"#,
        r#"{"id":3,"result":{"turn":{"id":"turn_1"}}}"#,
        r#"{"method":"item/agentMessage/delta","params":{"delta":"Run"}}"#,
        r#"{"jsonrpc":"2.0","id":"srv_1","method":"item/commandExecution/requestApproval","params":{"itemId":"c1","command":"rm -rf build"}}"#,
        r#"{"id":4,"result":{}}"#,
        r#"{"id":5,"result":{"threadId":"thr_9"}}"#,
        r#"{"method":"turn/completed","params":{"turn":{"id":"turn_1","status":"completed","durationMs":10}}}"#,
    ]
    .join("\n");
    let mut written: Vec<u8> = Vec::new();
    {
        let mut client = CodexClient::new(Cursor::new(script.into_bytes()), &mut written);
        let init = client.initialize("rightkit-consumer", "0.0.0").unwrap();
        assert_eq!(init["userAgent"], "codex/0.161.0");
        let thread = client
            .thread_start(thread_params(&CodexOptions::default(), Some("/work")))
            .unwrap();
        assert_eq!(thread, "thr_1");
        assert_eq!(
            client.turn_start(&thread, "list the files").unwrap(),
            Some("turn_1".to_owned())
        );
        let pending = client.drain_pending();
        assert_eq!(
            pending.len(),
            2,
            "lines that were not the awaited response are buffered in order"
        );
        assert!(
            matches!(&pending[0], Incoming::Notification { method, .. } if method == "thread/started")
        );
        assert!(
            matches!(&pending[1], Incoming::Notification { method, .. } if method == "turn/started")
        );
        client.turn_interrupt(&thread, "turn_1").unwrap();
        let resumed = client
            .thread_resume("thr_9", json!({ "approvalPolicy": "on-request" }))
            .unwrap();
        assert_eq!(resumed, "thr_9");
        match client.next_incoming().unwrap().expect("the delta") {
            Incoming::Notification { events, .. } => {
                assert_eq!(events, [AgentEvent::TextDelta { text: "Run".into() }])
            }
            other => panic!("{other:?}"),
        }
        let request = client
            .next_incoming()
            .unwrap()
            .expect("the approval request");
        let Incoming::ServerRequest { id, .. } = request else {
            panic!("expected a server request")
        };
        client.respond_approval(&id, Decision::Decline).unwrap();
        assert!(
            matches!(client.next_incoming().unwrap(), Some(Incoming::Notification { method, .. }) if method == "turn/completed")
        );
        assert!(client.next_incoming().unwrap().is_none(), "end of stream");
    }
    let sent: Vec<Value> = String::from_utf8(written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let methods: Vec<&str> = sent
        .iter()
        .map(|m| m["method"].as_str().unwrap_or("(response)"))
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "thread/start",
            "turn/start",
            "turn/interrupt",
            "thread/resume",
            "(response)"
        ]
    );
    assert_eq!(
        sent[0]["params"]["clientInfo"],
        json!({ "name": "rightkit-consumer", "version": "0.0.0" })
    );
    assert_eq!(
        sent[3]["params"],
        json!({ "threadId": "thr_1", "input": [{ "type": "text", "text": "list the files" }] })
    );
    assert_eq!(
        sent[4]["params"],
        json!({ "threadId": "thr_1", "turnId": "turn_1" })
    );
    assert_eq!(sent[5]["params"]["threadId"], "thr_9");
    assert_eq!(
        sent[6],
        json!({ "jsonrpc": "2.0", "id": "srv_1", "result": { "decision": "decline" } })
    );
    assert_eq!(
        sent.iter()
            .filter(|m| m["id"].is_u64())
            .map(|m| m["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5],
        "request ids count up"
    );

    let mut sink = Vec::new();
    let mut rpc = CodexClient::new(
        Cursor::new(br#"{"id":1,"error":{"code":-32000,"message":"nope"}}"#.to_vec()),
        &mut sink,
    );
    assert!(
        matches!(rpc.call("anything", json!({})), Err(ClientError::Rpc(v)) if v["message"] == "nope")
    );
    let mut sink = Vec::new();
    let mut closed = CodexClient::new(Cursor::new(Vec::new()), &mut sink);
    assert!(matches!(
        closed.call("anything", json!({})),
        Err(ClientError::Closed)
    ));
    assert!(ClientError::Closed.to_string().contains("closed stdout"));
    let mut sink = Vec::new();
    let mut strict = CodexClient::new(
        Cursor::new(b"\n{\"method\":\"turn/new\",\"params\":{}}\n".to_vec()),
        &mut sink,
    )
    .with_policy(UnknownPolicy::FailClosed);
    match strict.next_incoming() {
        Err(ClientError::Parse(error)) => {
            assert_eq!(error.unknown_required_method(), Some("turn/new"))
        }
        other => panic!("expected a refused line, got {other:?}"),
    }
    let mut sink = Vec::new();
    let mut lenient = CodexClient::new(
        Cursor::new(b"{\"method\":\"turn/new\",\"params\":{}}\n".to_vec()),
        &mut sink,
    );
    assert!(matches!(
        lenient.next_incoming(),
        Ok(Some(Incoming::Notification { .. }))
    ));
    let mut sink = Vec::new();
    let mut garbled = CodexClient::new(Cursor::new(b"not json\n".to_vec()), &mut sink);
    assert!(matches!(
        garbled.next_incoming(),
        Err(ClientError::Parse(_))
    ));
}

// ---- discovery with a stand-in CLI ---------------------------------------------------------

/// When this program is started under the name `claude` or `codex` it answers like a vendor CLI.
fn stand_in_vendor_cli() -> Option<i32> {
    let exe = std::env::current_exe().ok()?;
    let name = exe.file_stem()?.to_str()?.to_owned();
    if name != "claude" && name != "codex" {
        return None;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--version"] => println!("9.9.9 (stand-in {name})"),
        ["auth", "status"] => println!("{{\"loggedIn\":true}}"),
        ["--hang"] => std::thread::sleep(Duration::from_secs(120)),
        ["--chatty"] => {
            let mut out = std::io::stdout().lock();
            let block = [b'x'; 4096];
            for _ in 0..75 {
                if out.write_all(&block).is_err() {
                    break;
                }
            }
            let _ = out.flush();
            eprint!("err");
        }
        _ => return Some(2),
    }
    Some(0)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rk-agents-consumer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create consumer temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A just-killed child can keep its executable open for a moment on Windows.
        for _ in 0..20 {
            if fs::remove_dir_all(&self.0).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Windows searches with the PATHEXT spelling (claude.EXE) of a file that is claude.exe.
fn same_file_name(found: &Path, expected: &Path) -> bool {
    if cfg!(windows) {
        found
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.to_string_lossy())
    } else {
        found == expected
    }
}

fn install_stand_in(dir: &Path, name: &str) -> PathBuf {
    let target = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    fs::copy(std::env::current_exe().expect("current exe"), &target)
        .expect("copy the stand-in CLI");
    target
}

fn discovery(temp: &TempDir) {
    assert_eq!(claude_auth_status(br#"{"loggedIn":true}"#), Some(true));
    assert_eq!(claude_auth_status(br#"{"loggedIn":false}"#), Some(false));
    assert_eq!(claude_auth_status(b"failed to start"), None);
    assert_eq!(claude_auth_status(br#"{"error":"unavailable"}"#), None);
    assert_eq!(
        claude_auth_status(br#"{"loggedIn":"yes"}"#),
        None,
        "only the official boolean counts"
    );
    assert_eq!(
        (
            VendorCli::Claude.binary_name(),
            VendorCli::Codex.binary_name()
        ),
        ("claude", "codex")
    );
    assert_eq!(PROBE_OUTPUT_CAP, 64 * 1024);
    assert_eq!(PROBE_TIMEOUT, Duration::from_secs(10));
    assert_eq!(DISABLE_VENDOR_CLIS_ENV, "RIGHTKIT_DISABLE_VENDOR_CLIS");

    let bin = temp.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let claude = install_stand_in(&bin, "claude");
    let codex = install_stand_in(&bin, "codex");
    std::env::remove_var(DISABLE_VENDOR_CLIS_ENV);
    std::env::set_var("PATH", std::env::join_paths([&bin]).unwrap());

    assert!(same_file_name(
        &find_binary("claude").expect("claude on PATH"),
        &claude
    ));
    let found = find_binaries("codex");
    assert!(
        found.len() == 1 && same_file_name(&found[0], &codex),
        "{found:?}"
    );
    assert!(find_binary("definitely-not-installed").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let decoy = temp.0.join("decoy");
        fs::create_dir_all(&decoy).unwrap();
        fs::write(decoy.join("claude"), b"not executable").unwrap();
        fs::set_permissions(decoy.join("claude"), fs::Permissions::from_mode(0o644)).unwrap();
        std::env::set_var("PATH", std::env::join_paths([&decoy, &bin]).unwrap());
        assert!(
            same_file_name(&find_binary("claude").unwrap(), &claude),
            "a file without the executable bit is not launchable"
        );
        std::env::set_var("PATH", std::env::join_paths([&bin]).unwrap());
    }

    // Bounded runs: output is capped, a hang is cut at the deadline, a missing program is None.
    let long = Duration::from_secs(60);
    let version = run_bounded(&claude, &["--version"], long).expect("the stand-in answers");
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "9.9.9 (stand-in claude)"
    );
    let chatty = run_bounded(&claude, &["--chatty"], long).expect("chatty output");
    assert_eq!(
        chatty.stdout.len(),
        PROBE_OUTPUT_CAP,
        "output beyond the cap is drained and discarded"
    );
    assert_eq!(chatty.stderr, b"err".to_vec());
    let started = Instant::now();
    assert!(run_bounded(&claude, &["--hang"], Duration::from_millis(400)).is_none());
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the deadline ends the child"
    );
    assert!(run_bounded(&bin.join("no-such-program"), &["--version"], long).is_none());

    // Probes: version, and sign-in for Claude only.
    let status = probe(VendorCli::Claude, long);
    assert!(status.installed);
    assert!(same_file_name(
        status.path.as_deref().expect("path"),
        &claude
    ));
    assert_eq!(status.version.as_deref(), Some("9.9.9 (stand-in claude)"));
    assert_eq!(status.signed_in, Some(true));
    let status = probe(VendorCli::Codex, long);
    assert_eq!(
        (
            status.installed,
            status.version.as_deref(),
            status.signed_in
        ),
        (true, Some("9.9.9 (stand-in codex)"), None),
        "Codex sign-in is not probed"
    );

    // The cached snapshot: computed once, not re-read when PATH changes.
    let cached = detect(VendorCli::Claude);
    assert_eq!(cached, probe(VendorCli::Claude, long));
    std::env::set_var(
        "PATH",
        std::env::join_paths([temp.0.join("empty")]).unwrap(),
    );
    assert_eq!(detect(VendorCli::Claude), cached);
    assert!(
        !probe(VendorCli::Claude, long).installed,
        "a fresh probe sees the new PATH"
    );
    assert_eq!(probe(VendorCli::Claude, long), Default::default());

    // A disabled host reports every vendor as not installed.
    std::env::set_var("PATH", std::env::join_paths([&bin]).unwrap());
    std::env::set_var(DISABLE_VENDOR_CLIS_ENV, "1");
    assert_eq!(probe(VendorCli::Codex, long), Default::default());
    std::env::set_var(DISABLE_VENDOR_CLIS_ENV, "0");
    assert!(
        probe(VendorCli::Codex, long).installed,
        "0 does not disable"
    );
    std::env::set_var(DISABLE_VENDOR_CLIS_ENV, "");
    assert!(
        probe(VendorCli::Codex, long).installed,
        "an empty value does not disable"
    );
    std::env::remove_var(DISABLE_VENDOR_CLIS_ENV);
}
