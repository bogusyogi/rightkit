//! Fixtures under `fixtures/` use only fields shipped code already reads or emits:
//! Pulse (`core/src/bridge/codex_hook.rs`, `core/tests/context_cmd.rs`), Legion
//! (`engine/bins/legion-hook` tests), Membrane (`membrane/tests/acceptance.rs`, `hook.rs`) and
//! `packages/build-control-policy` tests. Expected output JSON is the shape those emit.

use rightkit_hooks::*;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn event(wire: Wire, name: &str) -> HookEvent {
    HookEvent::from_slice(wire, &fixture(name)).unwrap()
}

fn m(
    id: &str,
    f: impl Fn(&HookEvent, &ModuleContext) -> Result<ModuleOutput, String> + Send + Sync + 'static,
) -> Arc<dyn HookModule> {
    Arc::new(FnModule::new(id, f))
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rk-hooks-{name}-{}", new_receipt_id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---- parsing ----

#[test]
fn parses_claude_and_codex_fixtures() {
    let e = event(Wire::Claude, "claude_session_start_cwd");
    assert_eq!(e.kind, EventKind::SessionStart);
    assert_eq!(e.session_id.as_deref(), Some("acceptance"));
    assert_eq!(e.cwd.as_deref(), Some("/work/repo"));

    let e = event(Wire::Claude, "claude_user_prompt_submit");
    assert_eq!(e.kind, EventKind::UserPromptSubmit);
    assert_eq!(e.prompt.as_deref(), Some("fix the build"));

    let e = event(Wire::Claude, "claude_pre_tool_use_subagent");
    assert_eq!(e.kind, EventKind::PreToolUse);
    assert_eq!(e.tool_name.as_deref(), Some("Write"));
    assert_eq!(e.parent_tool_use_id.as_deref(), Some("toolu-parent"));
    assert_eq!(e.tool_input.as_ref().unwrap()["file_path"], "src/signin.rs");

    let e = event(Wire::Claude, "claude_stop_legion");
    assert_eq!(e.kind, EventKind::Stop);
    assert_eq!(e.stop_hook_active, Some(true));
    assert_eq!(e.last_assistant_message.as_deref(), Some("Done."));
    assert_eq!(e.session_id, None);

    let e = event(Wire::Codex, "codex_stop");
    assert_eq!(e.turn_id.as_deref(), Some("22222222-2222-4222-8222-222222222222"));
    assert_eq!(e.stop_hook_active, Some(false));

    let e = event(Wire::Codex, "codex_pre_tool_use_apply_patch");
    assert_eq!(e.tool_name.as_deref(), Some("apply_patch"));
    assert!(e.tool_input.as_ref().unwrap()["patch"].as_str().unwrap().starts_with("*** Begin Patch"));
}

#[test]
fn keeps_js_alias_precedence_and_raw_payload() {
    let e = event(Wire::Claude, "camel_case_aliases");
    assert_eq!(e.kind, EventKind::PreToolUse);
    assert_eq!(e.session_id.as_deref(), Some("s-9"));
    assert_eq!(e.tool_name.as_deref(), Some("Bash"));
    assert_eq!(e.tool_use_id.as_deref(), Some("toolu-9"));

    let e = event(Wire::Codex, "thread_id_wins");
    assert_eq!(e.kind, EventKind::Stop);
    assert_eq!(e.session_id.as_deref(), Some("thread-1"));
    assert_eq!(e.raw["session_id"], "session-1");
}

#[test]
fn legion_aliases_unknown_events_and_blank_fields() {
    assert_eq!(EventKind::from_name("pre-effect"), EventKind::PreToolUse);
    assert_eq!(EventKind::from_name("session-start"), EventKind::SessionStart);
    assert_eq!(EventKind::from_name("Mystery"), EventKind::Other("Mystery".into()));
    let e = HookEvent::from_value(Wire::Claude, json!({"hook_event_name":"Stop","session_id":"  "})).unwrap();
    assert_eq!(e.session_id, None);
}

#[test]
fn rejects_what_js_rejects() {
    assert_eq!(HookEvent::from_value(Wire::Claude, json!([1])), Err(ParseError::NotObject));
    assert_eq!(HookEvent::from_value(Wire::Claude, json!({"session_id":"a"})), Err(ParseError::EventRequired));
    assert!(matches!(HookEvent::from_slice(Wire::Claude, b"{"), Err(ParseError::Malformed(_))));
}

#[test]
fn stdin_cap_is_enforced() {
    let big = vec![b' '; (STDIN_CAP + 1) as usize];
    assert_eq!(HookEvent::from_reader(Wire::Claude, &big[..]), Err(ParseError::TooLarge));
}

#[test]
fn own_turn_follows_pulse_rule() {
    assert!(event(Wire::Claude, "claude_stop").is_own_turn());
    assert!(!event(Wire::Claude, "claude_post_tool_use_subagent").is_own_turn());
    assert!(event(Wire::Codex, "codex_post_tool_use_own").is_own_turn());
    assert!(!event(Wire::Codex, "codex_post_tool_use_subagent").is_own_turn());
}

// ---- output shapes ----

#[test]
fn additional_context_shape_and_event_gate() {
    let r = Reply::additional_context(&EventKind::SessionStart, "ctx").unwrap();
    assert_eq!(
        r.stdout.unwrap(),
        json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"ctx"}})
    );
    for k in [EventKind::UserPromptSubmit, EventKind::PostToolUse] {
        assert!(Reply::additional_context(&k, "x").is_ok());
    }
    for k in [EventKind::Stop, EventKind::PreToolUse, EventKind::PreCompact, EventKind::SessionEnd] {
        assert!(Reply::additional_context(&k, "x").is_err(), "{k}");
    }
}

#[test]
fn stop_block_and_system_message_shapes() {
    let r = Reply::block_stop("keep going").with_system_message("shown");
    assert_eq!(r.stdout.unwrap(), json!({"decision":"block","reason":"keep going","systemMessage":"shown"}));
    assert_eq!(r.exit_code, 0);
}

#[test]
fn pre_tool_use_decisions_per_wire() {
    let deny = Reply::deny_tool(Wire::Claude, "no");
    assert_eq!(
        deny.stdout.unwrap(),
        json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"no"}})
    );
    assert_eq!(deny.exit_code, 0);
    let ask = Reply::ask_tool(Wire::Claude, "sure?");
    assert_eq!(ask.stdout.unwrap()["hookSpecificOutput"]["permissionDecision"], "ask");
    for r in [Reply::deny_tool(Wire::Codex, "no"), Reply::ask_tool(Wire::Codex, "no")] {
        assert_eq!((r.stdout, r.stderr.as_deref(), r.exit_code), (None, Some("no"), 2));
    }
}

#[test]
fn write_to_emits_one_json_line() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = Reply::additional_context(&EventKind::SessionStart, "c").unwrap().write_to(&mut out, &mut err);
    assert_eq!(code, 0);
    assert!(err.is_empty());
    let text = String::from_utf8(out).unwrap();
    assert!(text.ends_with('\n') && text.matches('\n').count() == 1);
    let _: Value = serde_json::from_str(&text).unwrap();
}

// ---- dispatch ----

#[test]
fn runs_modules_in_order_and_merges_context() {
    let mods = [
        m("a", |_, _| Ok(ModuleOutput::context("one"))),
        m("b", |_, _| Ok(ModuleOutput::context("two"))),
    ];
    let r = dispatch(event(Wire::Claude, "claude_session_start"), &mods, &DispatchOptions::default());
    assert_eq!(r.status, Status::Ok);
    assert_eq!(r.results.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(
        r.reply.stdout.unwrap()["hookSpecificOutput"]["additionalContext"],
        "one\n\ntwo"
    );
}

#[test]
fn timeout_error_and_panic_become_typed_results_with_exit_zero() {
    let mods = [
        m("slow", |_, ctx| {
            while !ctx.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(ModuleOutput::empty())
        }),
        m("boom", |_, _| Err("nope".into())),
        m("panic", |_, _| panic!("kaboom")),
        m("fine", |_, _| Ok(ModuleOutput::context("still here"))),
    ];
    let opts = DispatchOptions { module_deadline: Duration::from_millis(30), ..Default::default() };
    let r = dispatch(event(Wire::Claude, "claude_session_start"), &mods, &opts);
    assert_eq!(r.status, Status::Error);
    assert!(r.results[0].error.as_ref().unwrap().contains("timed out"));
    assert_eq!(r.results[1].error.as_deref(), Some("nope"));
    assert!(r.results[2].error.as_ref().unwrap().contains("panicked"));
    assert_eq!(r.results[3].status, Status::Ok);
    assert_eq!(r.reply.exit_code, 0);
    assert!(r.reply.stdout.is_some());
}

#[test]
fn total_deadline_skips_the_rest() {
    let mods = [
        m("hog", |_, _| {
            std::thread::sleep(Duration::from_millis(120));
            Ok(ModuleOutput::empty())
        }),
        m("late", |_, _| Ok(ModuleOutput::context("x"))),
    ];
    let opts = DispatchOptions {
        module_deadline: Duration::from_millis(500),
        total_deadline: Duration::from_millis(60),
        ..Default::default()
    };
    let r = dispatch(event(Wire::Claude, "claude_session_start"), &mods, &opts);
    assert!(r.results[0].error.as_ref().unwrap().contains("timed out"));
    assert!(r.results[1].error.as_ref().unwrap().contains("total deadline"));
    assert!(r.reply.is_none());
}

#[test]
fn context_is_capped_on_a_char_boundary() {
    let mods = [m("big", |_, _| Ok(ModuleOutput::context("é".repeat(100))))];
    let opts = DispatchOptions { context_cap_bytes: 11, ..Default::default() };
    let r = dispatch(event(Wire::Claude, "claude_session_start"), &mods, &opts);
    let text = r.reply.stdout.unwrap()["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_owned();
    assert_eq!(text, "é".repeat(5));
}

#[test]
fn decisions_apply_only_where_the_schema_takes_them() {
    let deny = [m("g", |_, _| Ok(ModuleOutput::decide(Decision::DenyTool("blocked".into()))))];
    let o = DispatchOptions::default();
    // Claude PreToolUse: JSON deny, exit 0.
    let r = dispatch(event(Wire::Claude, "claude_pre_tool_use_subagent"), &deny, &o).reply;
    assert_eq!(r.stdout.unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(r.exit_code, 0);
    // Codex PreToolUse: stderr + exit 2.
    let r = dispatch(event(Wire::Codex, "codex_pre_tool_use_apply_patch"), &deny, &o).reply;
    assert_eq!((r.stdout, r.stderr.as_deref(), r.exit_code), (None, Some("blocked"), 2));
    // A deny on SessionStart is dropped.
    assert!(dispatch(event(Wire::Claude, "claude_session_start"), &deny, &o).reply.is_none());
    // Stop block.
    let stop = [m("s", |_, _| Ok(ModuleOutput::decide(Decision::BlockStop("more".into()))))];
    let r = dispatch(event(Wire::Codex, "codex_stop"), &stop, &o).reply;
    assert_eq!(r.stdout.unwrap(), json!({"decision":"block","reason":"more"}));
    // Context on Stop is dropped (host would reject the response).
    let ctx = [m("c", |_, _| Ok(ModuleOutput::context("x")))];
    assert!(dispatch(event(Wire::Codex, "codex_stop"), &ctx, &o).reply.is_none());
}

#[test]
fn bad_input_prints_nothing_and_exits_zero() {
    let mods = [m("a", |_, _| Ok(ModuleOutput::context("x")))];
    for input in [&b"not json"[..], b"[]", b"{}", b""] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(Wire::Claude, &mods, &DispatchOptions::default(), input, &mut out, &mut err);
        assert_eq!(code, 0);
        assert!(out.is_empty() && err.is_empty());
    }
}

#[test]
fn run_with_prints_the_reply() {
    let mods = [m("a", |e, _| Ok(ModuleOutput::context(format!("hi {}", e.session_id.clone().unwrap()))))];
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(Wire::Claude, &mods, &DispatchOptions::default(), &fixture("claude_session_start")[..], &mut out, &mut err);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["additionalContext"], "hi t");
}

#[test]
fn fallback_is_used_only_when_something_failed_and_nothing_was_composed() {
    let fallback = Reply::additional_context(&EventKind::SessionStart, "context off: failed").unwrap();
    let opts = DispatchOptions { fallback: fallback.clone(), ..Default::default() };
    let bad = [m("x", |_, _| Err("down".into()))];
    assert_eq!(dispatch(event(Wire::Claude, "claude_session_start"), &bad, &opts).reply, fallback);
    let good = [m("x", |_, _| Ok(ModuleOutput::empty()))];
    assert!(dispatch(event(Wire::Claude, "claude_session_start"), &good, &opts).reply.is_none());
}

// ---- receipts ----

#[test]
fn tag_context_keeps_marker_within_cap() {
    let t = tag_context(&"x".repeat(100), "pulse", "abc123", 40);
    assert!(t.len() <= 40);
    assert!(t.ends_with("\n[pulse:abc123]"));
    assert_eq!(tag_context("short", "pulse", "i", 100), "short\n[pulse:i]");
}

#[test]
fn emitted_then_observed_via_transcript_scan() {
    let dir = scratch("observe");
    let store = ReceiptStore::new(&dir, "pulse");
    let start = event(Wire::Claude, "claude_session_start_cwd");
    let tagged = store.tag_and_record(&start, "orientation", 10_000).unwrap();
    let receipts = store.list("acceptance");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].level, DeliveryLevel::Emitted);
    assert_eq!(receipts[0].chars, tagged.len());
    assert_eq!(receipts[0].wire, "claude");
    assert_eq!(receipts[0].event, "SessionStart");

    // Marker absent: stays emitted.
    let transcript = dir.join("t.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
    assert_eq!(store.observe("acceptance", Wire::Claude, &transcript), 0);
    assert_eq!(store.list("acceptance")[0].level, DeliveryLevel::Emitted);

    // Marker present (as the host would record it inside a JSON string): observed.
    let line = json!({"type":"attachment","content":tagged}).to_string();
    std::fs::write(&transcript, format!("{line}\n")).unwrap();
    assert_eq!(store.observe("acceptance", Wire::Codex, &transcript), 0, "other wire");
    assert_eq!(store.observe("acceptance", Wire::Claude, &transcript), 1);
    assert_eq!(store.list("acceptance")[0].level, DeliveryLevel::Observed);
    assert_eq!(store.observe("acceptance", Wire::Claude, &transcript), 0, "idempotent");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn observe_module_runs_on_own_stop_only() {
    let dir = scratch("module");
    let store = ReceiptStore::new(&dir, "pulse");
    let start = HookEvent::from_value(Wire::Claude, json!({"hook_event_name":"SessionStart","session_id":"s1"})).unwrap();
    let tagged = store.tag_and_record(&start, "ctx", 1000).unwrap();
    let transcript = dir.join("s1.jsonl");
    std::fs::write(&transcript, serde_json::to_string(&tagged).unwrap()).unwrap();
    let stop = |extra: Value| {
        let mut v = json!({"hook_event_name":"Stop","session_id":"s1","transcript_path":transcript});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        HookEvent::from_value(Wire::Claude, v).unwrap()
    };
    let mods: [Arc<dyn HookModule>; 1] = [Arc::new(ObserveModule::new(store.clone()))];
    // A non-Stop event does not scan.
    let r = dispatch(HookEvent::from_value(Wire::Claude, json!({"hook_event_name":"UserPromptSubmit","session_id":"s1","transcript_path":transcript})).unwrap(), &mods, &DispatchOptions::default());
    assert_eq!(r.status, Status::Ok);
    assert_eq!(store.list("s1")[0].level, DeliveryLevel::Emitted);
    // The Stop scans and flips it; its own reply is empty.
    let r = dispatch(stop(json!({})), &mods, &DispatchOptions::default());
    assert!(r.reply.is_none());
    assert_eq!(r.results[0].output.as_ref().unwrap().detail["observed"], 1);
    assert_eq!(store.list("s1")[0].level, DeliveryLevel::Observed);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unsafe_ids_never_escape_the_store() {
    let dir = scratch("paths");
    let store = ReceiptStore::new(&dir, "pulse");
    let e = HookEvent::from_value(Wire::Codex, json!({"hook_event_name":"SessionStart","session_id":"../../etc"})).unwrap();
    store.record_emitted(&e, "abc", "x").unwrap();
    assert!(store.record_emitted(&e, "../x", "x").is_err());
    assert!(dir.join("receipts").join(".._.._etc").join("abc.json").is_file());
    std::fs::remove_dir_all(dir).unwrap();
}
