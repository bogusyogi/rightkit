//! rightkit-agent-tools: the toolbox gate with real tools. A caller calls through exposure rules (a paste tool cannot be
//! registered), the per-caller allowlist (including subject-level patterns), argument-schema validation, an admission hook,
//! cancellation, a JSONL journal and metrics; and the M22 applicability record is parsed and checked against disk.
//!
//! Needs: nothing beyond the engine's `rightkit-agent-tools` dependency (default features off: no desktop/browser/speech
//! backends, so no live app or browser is driven here).
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_agent_tools::{
    AgentRole, ApplicabilityRecord, CallCtx, Caller, CancelToken, Effect, JsonlJournal, MemoryJournal, Tool, ToolAdmission,
    ToolError, ToolEvent, ToolRequest, ToolSpec, Toolbox, Verdict, ERR_CANCELLED, ERR_DENIED, ERR_INVALID_ARGS, ERR_NOT_ALLOWED,
    ERR_NOT_EXPOSED, ERR_UNKNOWN_TOOL,
};
use serde_json::{json, Value};

/// `e2e.note`: records what it was asked to do. Its subject is the `verb` argument, so allowlists can grant single verbs.
struct Note {
    spec: ToolSpec,
    ran: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Tool for Note {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn subject(&self, args: &Value) -> Option<String> {
        args["verb"].as_str().map(str::to_string)
    }
    fn call(&self, ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(format!("{}:{}", ctx.caller.app, args["verb"]));
        Ok(json!({"verb": args["verb"], "text": args["text"], "caller": ctx.caller.app}))
    }
}

struct Fails(ToolSpec);
impl Tool for Fails {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }
    fn call(&self, _ctx: &CallCtx, _args: Value) -> Result<Value, ToolError> {
        Err(ToolError::retryable("backend_down", "the backend is down"))
    }
}

/// Denies any call whose arguments are larger than 200 bytes; records settles.
struct SizeGate {
    settled: Mutex<Vec<(String, bool, Option<String>)>>,
}
impl ToolAdmission for SizeGate {
    fn approve(&self, req: &ToolRequest) -> Verdict {
        if req.arg_bytes > 200 {
            Verdict::Deny(format!("{} bytes of arguments is over the cap", req.arg_bytes))
        } else {
            Verdict::Allow
        }
    }
    fn settle(&self, req: &ToolRequest, ok: bool, error_code: Option<&str>) {
        self.settled.lock().unwrap().push((req.tool.clone(), ok, error_code.map(str::to_string)));
    }
}

fn note_spec(name: &str) -> ToolSpec {
    ToolSpec::new(
        name,
        "records a note",
        Effect::Act,
        json!({"type": "object", "required": ["verb"], "properties": {"verb": {"type": "string", "enum": ["read", "write", "delete"]}, "text": {"type": "string"}}, "additionalProperties": false}),
    )
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("agent_tools")?;
    let ran = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(SizeGate { settled: Mutex::new(Vec::new()) });
    let mem = MemoryJournal::new();
    let jsonl_path = dir.join("journal.jsonl");

    let mut tb = Toolbox::new();
    tb.register(Arc::new(Note { spec: note_spec("e2e.note"), ran: ran.clone(), seen: seen.clone() }))
        .map_err(|e| format!("register e2e.note: {e}"))?;
    tb.register(Arc::new(Fails(ToolSpec::new("e2e.flaky", "always fails", Effect::Network, json!({"type": "object"})))))
        .map_err(|e| e.to_string())?;
    // Exposure rules are not configurable: a paste tool is refused for any app.
    for bad in ["e2e.paste", "e2e.pasteText", "e2e.read_pasteboard"] {
        match tb.register(Arc::new(Note { spec: note_spec(bad), ran: ran.clone(), seen: seen.clone() })) {
            Err(e) if e.code == ERR_NOT_EXPOSED => {}
            other => return Err(format!("registering `{bad}` was not refused as not_exposed: {:?}", other.err())),
        }
    }
    tb.allow("agent-a", ["e2e.note:read", "e2e.note:write", "e2e.flaky"]);
    tb.allow("agent-b", ["e2e.*"]);
    tb.set_admission(gate.clone());
    tb.add_journal(mem.clone());
    tb.add_journal(Arc::new(JsonlJournal::new(&jsonl_path)));

    let a = Caller::new("agent-a");
    let b = Caller::new("agent-b");
    let stranger = Caller::new("nobody");

    // Allowed call returns the tool's value.
    let ok = tb.call(&a, "e2e.note", json!({"verb": "read", "text": "hello"})).map_err(|e| format!("allowed call: {e}"))?;
    if ok != json!({"verb": "read", "text": "hello", "caller": "agent-a"}) {
        return Err(format!("allowed call returned {ok}"));
    }

    // Subject-level allowlist: agent-a may write but not delete; a wildcard caller may.
    let code_of = |r: Result<Value, ToolError>| r.err().map(|e| e.code).unwrap_or_else(|| "ok".into());
    let del_a = code_of(tb.call(&a, "e2e.note", json!({"verb": "delete"})));
    if del_a != ERR_NOT_ALLOWED {
        return Err(format!("agent-a delete gave {del_a}, expected {ERR_NOT_ALLOWED}"));
    }
    if code_of(tb.call(&b, "e2e.note", json!({"verb": "delete"}))) != "ok" {
        return Err("a wildcard caller could not delete".into());
    }
    if code_of(tb.call(&stranger, "e2e.note", json!({"verb": "read"}))) != ERR_NOT_ALLOWED {
        return Err("a caller with no allowlist entry was admitted".into());
    }
    if code_of(tb.call(&a, "e2e.missing", json!({}))) != ERR_UNKNOWN_TOOL {
        return Err("an unknown tool was not unknown_tool".into());
    }
    let ran_before_bad = ran.load(Ordering::SeqCst);

    // Argument schema: missing required, wrong type, value outside enum, unknown key.
    for (args, why) in [
        (json!({}), "missing verb"),
        (json!({"verb": 7}), "verb not a string"),
        (json!({"verb": "format"}), "verb outside the enum"),
        (json!({"verb": "read", "extra": true}), "unknown argument"),
    ] {
        let c = code_of(tb.call(&b, "e2e.note", args));
        if c != ERR_INVALID_ARGS {
            return Err(format!("{why}: got {c}, expected {ERR_INVALID_ARGS}"));
        }
    }

    // Admission hook: an oversized call is denied before the tool runs.
    let big = "x".repeat(300);
    let denied = tb.call(&b, "e2e.note", json!({"verb": "read", "text": big}));
    if code_of(denied) != ERR_DENIED {
        return Err("the admission hook did not deny an oversized call".into());
    }
    if ran.load(Ordering::SeqCst) != ran_before_bad {
        return Err("a refused call still reached the tool".into());
    }

    // Cancellation before start, and a tool failure carried through typed.
    let token = CancelToken::new();
    token.cancel();
    match tb.call_cancellable(&b, "e2e.note", json!({"verb": "read"}), &token) {
        Err(e) if e.code == ERR_CANCELLED => {}
        other => return Err(format!("a cancelled call gave {:?}", other.map(|v| v.to_string()))),
    }
    match tb.call(&a, "e2e.flaky", json!({})) {
        Err(e) if e.code == "backend_down" && e.retryable => {}
        other => return Err(format!("tool failure was not carried typed: {:?}", other.map(|v| v.to_string()))),
    }

    // Visible tool list follows the allowlist.
    let names_a: Vec<String> = tb.list_for("agent-a").into_iter().map(|s| s.name).collect();
    if names_a != vec!["e2e.flaky".to_string(), "e2e.note".to_string()] || !tb.list_for("nobody").is_empty() {
        return Err(format!("list_for(agent-a) = {names_a:?}"));
    }

    // Metrics and journals saw every outcome.
    let m = tb.metrics();
    if m.ok != 2 || m.failed != 2 || m.denied != 7 || m.calls != 12 {
        return Err(format!("metrics {m:?} do not match 12 calls: 2 ok, 2 failed (cancelled, flaky), 7 denied, 1 unknown tool"));
    }
    let events = mem.events();
    let started = events.iter().filter(|e| matches!(e, ToolEvent::Started { .. })).count();
    let settled = events.iter().filter(|e| matches!(e, ToolEvent::Settled { .. })).count();
    if started != 4 || settled != 4 {
        return Err(format!("journal has {started} started / {settled} settled, expected 4 / 4"));
    }
    let lines = std::fs::read_to_string(&jsonl_path).map_err(|e| format!("read journal file: {e}"))?;
    let rows: Vec<Value> = lines.lines().map(|l| serde_json::from_str(l).map_err(|e| format!("journal line: {e}"))).collect::<Result<_, _>>()?;
    if rows.len() != events.len() {
        return Err(format!("the JSONL journal has {} rows for {} events", rows.len(), events.len()));
    }
    if lines.contains("hello") || lines.contains(&big) {
        return Err("the journal holds argument values".into());
    }
    if gate.settled.lock().unwrap().len() != 4 {
        return Err("the admission hook did not settle each executed call".into());
    }

    // ---- M22 applicability record against a real app root on disk -----------------------------------------------------
    let root = dir.join("app");
    std::fs::create_dir_all(root.join("src")).map_err(|e| e.to_string())?;
    std::fs::write(root.join("src/agent.rs"), "pub fn register_tools() {}\n").map_err(|e| e.to_string())?;
    let manifest = json!({"M22": {"role": "client", "entrypoints": ["src/agent.rs#register_tools", "register_tools", "src/gone.rs"]}}).to_string();
    let rec = ApplicabilityRecord::from_manifest(&manifest).map_err(|e| e.to_string())?.ok_or("no M22 record parsed")?;
    if rec.role != AgentRole::Client || rec.unverified_entrypoints() != 1 || !rec.has_agent_surface() {
        return Err(format!("record parsed wrong: {rec:?}"));
    }
    let missing = rec.missing_entrypoint_files(&root);
    if missing.len() != 1 || !missing[0].contains("src/gone.rs") {
        return Err(format!("missing entrypoints: {missing:?}"));
    }
    let bad = json!({"M22": {"role": "absent", "entrypoints": ["x.rs"]}}).to_string();
    if ApplicabilityRecord::from_manifest(&bad).is_ok() {
        return Err("an `absent` record with entrypoints was accepted".into());
    }

    Ok(json!({"calls": m.calls, "ok": m.ok, "failed": m.failed, "denied": m.denied, "journal_rows": rows.len(), "m22_role": rec.role.label()}))
}
