//! rightkit-hooks: typed Claude Code / Codex hook payloads dispatched through real modules under deadlines, the exact
//! per-event output shapes and exit codes through `run_with` (the hook process entry), and the emitted -> observed receipt
//! cycle against a transcript file on disk.
//!
//! Needs: nothing beyond the engine's `rightkit-hooks` dependency.
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_hooks::{
    dispatch, run_with, Decision, DispatchOptions, EventKind, FnModule, HookEvent, HookModule, ModuleOutput, ParseError,
    ReceiptStore, Reply, Status, Wire,
};
use rightkit_hooks::DeliveryLevel;
use serde_json::{json, Value};

fn module(
    id: &str,
    f: impl Fn(&HookEvent, &rightkit_hooks::ModuleContext) -> Result<ModuleOutput, String> + Send + Sync + 'static,
) -> Arc<dyn HookModule> {
    Arc::new(FnModule::new(id, f))
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("hooks")?;

    // ---- typed parsing of real payload shapes ---------------------------------------------------------------------
    let pre = json!({
        "hook_event_name": "PreToolUse", "session_id": "s-e2e", "tool_name": "Bash", "tool_use_id": "toolu-1",
        "cwd": "/work", "tool_input": {"command": "rm -rf /tmp/x"}
    });
    let ev = HookEvent::from_slice(Wire::Claude, pre.to_string().as_bytes()).map_err(|e| format!("parse pre: {e}"))?;
    if ev.kind != EventKind::PreToolUse
        || ev.tool_name.as_deref() != Some("Bash")
        || ev.tool_input.as_ref().map(|v| v["command"].clone()) != Some(json!("rm -rf /tmp/x"))
    {
        return Err("PreToolUse payload did not parse into its typed fields".into());
    }
    if HookEvent::from_slice(Wire::Claude, b"{").is_ok() {
        return Err("a truncated payload parsed".into());
    }
    if HookEvent::from_value(Wire::Claude, json!({"session_id": "a"})) != Err(ParseError::EventRequired) {
        return Err("a payload with no event name was not EventRequired".into());
    }
    if EventKind::from_name("pre-effect") != EventKind::PreToolUse {
        return Err("a Legion alias did not normalize".into());
    }

    // ---- dispatch: deny decision, context merge, error isolation, deadline ------------------------------------------
    let opts = DispatchOptions {
        module_deadline: Duration::from_millis(300),
        total_deadline: Duration::from_millis(900),
        ..DispatchOptions::default()
    };
    let deny = module("deny-rm", |e, _| {
        let cmd = e.tool_input.as_ref().and_then(|v| v["command"].as_str()).unwrap_or("");
        if cmd.contains("rm -rf") {
            Ok(ModuleOutput::decide(Decision::DenyTool("destructive command".into())))
        } else {
            Ok(ModuleOutput::empty())
        }
    });
    let r = dispatch(ev.clone(), &[deny.clone()], &opts);
    let reply = &r.reply;
    let expected = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "destructive command"}});
    if reply.stdout.as_ref() != Some(&expected) || reply.exit_code != 0 {
        return Err(format!("Claude deny reply is wrong: {:?} exit {}", reply.stdout, reply.exit_code));
    }
    let codex_ev = HookEvent::from_slice(Wire::Codex, pre.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let r = dispatch(codex_ev, &[deny], &opts);
    if r.reply.exit_code != 2 || r.reply.stdout.is_some() || r.reply.stderr.as_deref() != Some("destructive command") {
        return Err("a Codex deny is not empty stdout + reason on stderr + exit 2".into());
    }

    // A slow module is cut at its deadline, a panicking one is isolated, and the healthy ones still answer.
    let prompt = HookEvent::from_value(
        Wire::Claude,
        json!({"hook_event_name": "UserPromptSubmit", "session_id": "s-e2e", "prompt": "hello"}),
    )
    .map_err(|e| e.to_string())?;
    let slow = module("slow", |_, c| {
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until && !c.is_cancelled() {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(ModuleOutput::context("too late"))
    });
    let boom = module("boom", |_, _| -> Result<ModuleOutput, String> { panic!("module bug") });
    let fine_a = module("a", |e, _| Ok(ModuleOutput::context(format!("prompt was {}", e.prompt.clone().unwrap_or_default()))));
    let fine_b = module("b", |_, _| Ok(ModuleOutput::context("second")));
    let started = Instant::now();
    let r = dispatch(prompt.clone(), &[fine_a, slow, boom, fine_b], &opts);
    let took = started.elapsed();
    if took > Duration::from_millis(1500) {
        return Err(format!("dispatch took {took:?}, the per-module deadline did not bound it"));
    }
    let statuses: Vec<(String, Status)> = r.results.iter().map(|m| (m.id.clone(), m.status)).collect();
    if statuses != vec![("a".into(), Status::Ok), ("slow".into(), Status::Error), ("boom".into(), Status::Error), ("b".into(), Status::Ok)] {
        return Err(format!("module statuses wrong: {statuses:?}"));
    }
    if !r.results[1].error.as_deref().unwrap_or("").contains("timed out") || r.status != Status::Error {
        return Err("the slow module was not reported as timed out".into());
    }
    let text = r.reply.stdout.as_ref().and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str()).unwrap_or("");
    if text != "prompt was hello\n\nsecond" {
        return Err(format!("merged context is {text:?}"));
    }

    // ---- the hook process entry: stdin payload in, stdout line and exit code out -----------------------------------
    let mut out = Vec::new();
    let mut err = Vec::new();
    let modules = vec![module("ctx", |_, _| Ok(ModuleOutput::context("injected")))];
    let code = run_with(Wire::Claude, &modules, &opts, prompt_bytes().as_slice(), &mut out, &mut err);
    let line: Value = serde_json::from_slice(&out).map_err(|e| format!("run_with stdout is not one JSON line: {e}"))?;
    if code != 0 || line["hookSpecificOutput"]["additionalContext"] != "injected" {
        return Err(format!("run_with printed {line} with exit {code}"));
    }
    let (mut out2, mut err2) = (Vec::new(), Vec::new());
    let code = run_with(Wire::Claude, &modules, &opts, &b"not json"[..], &mut out2, &mut err2);
    if code != 0 || !out2.is_empty() {
        return Err("a garbage payload did not exit 0 silently".into());
    }
    let block = Reply::block_stop("keep going");
    if block.stdout != Some(json!({"decision": "block", "reason": "keep going"})) {
        return Err("block_stop shape wrong".into());
    }

    // ---- receipts on disk: emitted, then observed once the marker appears in the transcript tail -------------------
    let store = ReceiptStore::new(dir.join("store"), "e2e");
    let start = HookEvent::from_value(Wire::Claude, json!({"hook_event_name": "SessionStart", "session_id": "sess-1"}))
        .map_err(|e| e.to_string())?;
    let tagged = store.tag_and_record(&start, "context for the model", 4096).map_err(|e| format!("tag_and_record: {e}"))?;
    let receipts = store.list("sess-1");
    if receipts.len() != 1 || receipts[0].level != DeliveryLevel::Emitted || receipts[0].chars != tagged.len() {
        return Err(format!("emitted receipt wrong: {receipts:?}"));
    }
    let transcript = dir.join("transcript.jsonl");
    std::fs::write(&transcript, "{\"msg\":\"nothing to see\"}\n").map_err(|e| e.to_string())?;
    if store.observe("sess-1", Wire::Claude, &transcript) != 0 {
        return Err("a receipt was observed without its marker in the transcript".into());
    }
    let marker = tagged.lines().last().unwrap_or("").to_string();
    std::fs::write(&transcript, format!("{{\"msg\":{}}}\n", serde_json::to_string(&format!("seen {marker}")).unwrap()))
        .map_err(|e| e.to_string())?;
    if store.observe("sess-1", Wire::Claude, &transcript) != 1 || store.list("sess-1")[0].level != DeliveryLevel::Observed {
        return Err("the receipt did not flip to observed once the marker was in the transcript".into());
    }

    Ok(json!({
        "dispatch_ms": took.as_millis() as u64,
        "claude_deny": "hookSpecificOutput deny, exit 0",
        "codex_deny": "stderr reason, exit 2",
        "isolated": ["timeout", "panic"],
        "receipt": "emitted -> observed",
    }))
}

fn prompt_bytes() -> Vec<u8> {
    json!({"hook_event_name": "UserPromptSubmit", "session_id": "s-e2e", "prompt": "hello"}).to_string().into_bytes()
}
