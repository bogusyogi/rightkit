use super::*;
use crate::coding_tools::registry::ToolRegistry;
use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use std::sync::Mutex;

struct FakeTool {
    name: &'static str,
    read_only: bool,
}

#[async_trait::async_trait]
impl Tool for FakeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "fake"
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn is_read_only(&self) -> bool {
        self.read_only
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::ok("unused")
    }
}

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(String, Value)>>,
}

impl NestedToolDispatcher for Recorder {
    fn invoke<'a>(
        &'a self,
        tool: &'a str,
        args: Value,
        _c: &'a ToolContext,
    ) -> NestedToolFuture<'a> {
        self.calls
            .lock()
            .unwrap()
            .push((tool.to_string(), args.clone()));
        Box::pin(async move {
            if tool == "slow_read" {
                tokio::time::sleep(Duration::from_secs(11)).await;
            }
            ToolResult::ok(format!("read:{}", args["path"].as_str().unwrap_or("")))
        })
    }
}

struct Passthrough;

impl ContextToolDispatcher for Passthrough {
    fn compress<'a>(
        &'a self,
        output: String,
        _c: &'a ToolContext,
        _d: &'a dyn NestedToolDispatcher,
        _p: &'a str,
    ) -> NestedToolFuture<'a> {
        Box::pin(async move { ToolResult::ok(output) })
    }
}

fn specs(names: &[&str]) -> Vec<ProgramToolSpec> {
    names
        .iter()
        .map(|n| ProgramToolSpec {
            name: n.to_string(),
            description: "d".into(),
            schema: json!({"type":"object"}),
        })
        .collect()
}

async fn run(source: &str, allowed: &[&str], d: &Recorder) -> ToolResult {
    let ctx = ToolContext::new(
        "code-mode-test",
        std::env::temp_dir(),
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );
    execute_program(
        source,
        &specs(allowed),
        &ctx,
        d,
        Some(&Passthrough),
        "parent",
    )
    .await
}

#[tokio::test]
async fn bridge_reaches_read_only_tools() {
    let d = Recorder::default();
    let r = run(
        r#"const a = await tools.read_file({path:"a.txt"}); const b = await tools.read_file({path:"b.txt"}); return [a, b];"#,
        &["read_file"],
        &d,
    )
    .await;
    assert!(!r.is_error, "{}", r.content);
    assert_eq!(r.content, r#"["read:a.txt","read:b.txt"]"#);
    let calls = d.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "read_file");
}

#[test]
fn registry_exposes_only_read_only_tools() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeTool {
        name: "read_file",
        read_only: true,
    }));
    registry.register(Box::new(FakeTool {
        name: "write_file",
        read_only: false,
    }));
    registry.register(Box::new(FakeTool {
        name: "exec",
        read_only: true,
    }));
    let tool = RunProgramTool::new(&registry);
    let names: Vec<_> = tool.tool_specs().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"read_file"));
    assert!(!names.contains(&"write_file"));
    assert!(!names.contains(&"exec"));
}

#[tokio::test]
async fn mutating_tools_are_rejected() {
    let d = Recorder::default();
    let r = run(
        r#"return await tools.write_file({path:"x", content:"y"});"#,
        &["read_file"],
        &d,
    )
    .await;
    assert!(r.is_error);
    assert!(
        d.calls.lock().unwrap().is_empty(),
        "mutating tool reached dispatcher"
    );
    // Indexing by string and prototype tricks must not reach it either.
    let r = run(
        r#"const t = tools["write_file"] ?? Object.getPrototypeOf(tools).write_file; return typeof t;"#,
        &["read_file"],
        &d,
    )
    .await;
    assert_eq!(r.content, r#""undefined""#);
    assert!(d.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn infinite_loop_is_interrupted() {
    let d = Recorder::default();
    let started = Instant::now();
    let r = run("while (true) {}", &[], &d).await;
    assert!(r.is_error);
    assert!(started.elapsed() < PROGRAM_TIMEOUT + Duration::from_secs(5));
}

#[tokio::test]
async fn nested_call_timeout_is_reported() {
    let d = Recorder::default();
    let r = run(
        r#"return await tools.slow_read({path:"p"});"#,
        &["slow_read"],
        &d,
    )
    .await;
    assert!(r.is_error);
    assert!(r.content.contains("timed out"), "{}", r.content);
}

#[tokio::test]
async fn memory_limit_is_enforced() {
    let d = Recorder::default();
    let r = run(
        r#"const keep = []; while (true) { keep.push("x".repeat(1 << 20) + keep.length); }"#,
        &[],
        &d,
    )
    .await;
    assert!(r.is_error, "{}", r.content);
}

#[tokio::test]
async fn call_limit_is_enforced() {
    let d = Recorder::default();
    let r = run(
        r#"for (let i = 0; i < 100; i++) { await tools.read_file({path:"p"}); } return "done";"#,
        &["read_file"],
        &d,
    )
    .await;
    assert!(r.is_error);
    assert_eq!(d.calls.lock().unwrap().len(), PROGRAM_MAX_CALLS);
}

#[tokio::test]
async fn no_global_escape() {
    let d = Recorder::default();
    let r = run(
        r#"return ["require","process","fetch","XMLHttpRequest","WebSocket","Deno","Bun","std","os","readFile","import"]
            .map(n => typeof globalThis[n]);"#,
        &[],
        &d,
    )
    .await;
    assert!(!r.is_error, "{}", r.content);
    let v: Vec<String> = serde_json::from_str(&r.content).unwrap();
    assert!(v.iter().all(|t| t == "undefined"), "{v:?}");
    for code in [
        "return require('fs');",
        "return process.env;",
        "return await fetch('http://127.0.0.1/');",
        "return await import('fs');",
    ] {
        let r = run(code, &[], &d).await;
        assert!(r.is_error, "{code} -> {}", r.content);
    }
    assert!(d.calls.lock().unwrap().is_empty());
}

async fn run_profile(
    profile: ProgramProfile,
    source: &str,
    allowed: &[&str],
    d: &dyn NestedToolDispatcher,
) -> ToolResult {
    let ctx = ToolContext::new(
        "code-mode-test",
        std::env::temp_dir(),
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );
    execute_program_with_profile(
        profile,
        source,
        &specs(allowed),
        &ctx,
        d,
        Some(&Passthrough),
        "parent",
    )
    .await
}

#[tokio::test]
async fn workspace_profile_dispatches_mutation_through_host() {
    let d = Recorder::default();
    let r = run_profile(
        ProgramProfile::Workspace,
        r#"return await tools.write_file({path:"x.txt", content:"y"});"#,
        &["read_file", "write_file"],
        &d,
    )
    .await;
    assert!(!r.is_error, "{}", r.content);
    let calls = d.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "write_file");
}

#[tokio::test]
async fn read_only_profile_drops_mutating_tools_even_if_passed() {
    let d = Recorder::default();
    let r = run_profile(
        ProgramProfile::ReadOnly,
        r#"return await tools.write_file({path:"x", content:"y"});"#,
        &["read_file", "write_file", "shell"],
        &d,
    )
    .await;
    assert!(r.is_error);
    assert!(
        d.calls.lock().unwrap().is_empty(),
        "mutation reached dispatcher"
    );
}

#[test]
fn workspace_registry_exposes_only_allowlisted_mutators() {
    let mut registry = ToolRegistry::new();
    for (name, read_only) in [
        ("read_file", true),
        ("write_file", false),
        ("edit_file", false),
        ("shell", false),
        ("git_write", false),
        ("mcp_call", false),
        ("send_message", false),
        ("spawn_subagent", false),
        ("exec", false),
    ] {
        registry.register(Box::new(FakeTool { name, read_only }));
    }
    let tool = RunProgramTool::new_workspace(&registry);
    let names: Vec<_> = tool.tool_specs().iter().map(|t| t.name.as_str()).collect();
    for expected in ["read_file", "write_file", "edit_file", "shell"] {
        assert!(names.contains(&expected), "{expected} missing: {names:?}");
    }
    for banned in [
        "git_write",
        "mcp_call",
        "send_message",
        "spawn_subagent",
        "exec",
    ] {
        assert!(!names.contains(&banned), "{banned} exposed: {names:?}");
    }
    let read_only = RunProgramTool::new(&registry);
    assert!(read_only.tool_specs().iter().all(|t| t.name == "read_file"));
}

#[tokio::test]
async fn non_allowlisted_mutator_is_rejected_even_in_workspace_profile() {
    let d = Recorder::default();
    let r = run_profile(
        ProgramProfile::Workspace,
        r#"return await tools.git_write({});"#,
        &["read_file", "write_file"],
        &d,
    )
    .await;
    assert!(r.is_error);
    assert!(d.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mutation_limit_is_enforced() {
    let d = Recorder::default();
    let r = run_profile(
        ProgramProfile::Workspace,
        r#"for (let i = 0; i < 20; i++) { await tools.write_file({path:"p", content:"c"}); } return "done";"#,
        &["write_file"],
        &d,
    )
    .await;
    assert!(r.is_error);
    assert_eq!(d.calls.lock().unwrap().len(), PROGRAM_MAX_MUTATIONS);
}

/// Dispatcher that denies mutations the way a refused approval would.
struct Denying(Mutex<Vec<String>>);

impl NestedToolDispatcher for Denying {
    fn invoke<'a>(&'a self, tool: &'a str, _a: Value, _c: &'a ToolContext) -> NestedToolFuture<'a> {
        self.0.lock().unwrap().push(tool.to_string());
        Box::pin(async move { ToolResult::error("approval denied") })
    }
}

#[tokio::test]
async fn denied_mutation_surfaces_as_error_and_stops_program() {
    let d = Denying(Mutex::new(Vec::new()));
    let r = run_profile(
        ProgramProfile::Workspace,
        r#"await tools.write_file({path:"p", content:"c"}); await tools.write_file({path:"q", content:"c"}); return "done";"#,
        &["write_file"],
        &d,
    )
    .await;
    assert!(r.is_error);
    assert!(r.content.contains("approval denied"), "{}", r.content);
    assert_eq!(
        d.0.lock().unwrap().len(),
        1,
        "program continued after denial"
    );
}

/// Tracks overlapping in-flight calls to prove mutation serialization.
#[derive(Default)]
struct Overlap {
    in_flight: AtomicUsize,
    max: AtomicUsize,
}

impl NestedToolDispatcher for Overlap {
    fn invoke<'a>(&'a self, _t: &'a str, _a: Value, _c: &'a ToolContext) -> NestedToolFuture<'a> {
        Box::pin(async move {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            ToolResult::ok("ok")
        })
    }
}

#[tokio::test]
async fn parallel_mutations_are_serialized() {
    let d = Overlap::default();
    let r = run_profile(
        ProgramProfile::Workspace,
        r#"await Promise.all([tools.write_file({path:"a"}), tools.write_file({path:"b"}), tools.edit_file({path:"c"})]); return "done";"#,
        &["write_file", "edit_file"],
        &d,
    )
    .await;
    assert!(!r.is_error, "{}", r.content);
    assert_eq!(d.max.load(Ordering::SeqCst), 1, "mutations overlapped");
}

#[tokio::test]
async fn membrane_respects_the_read_only_allowlist() {
    let d = Recorder::default();
    // Allowlist lacks every Membrane target: all three retrieval ops refused.
    let r = run(
        r#"const out = [];
        for (const op of ["blueprint_search", "blueprint_expand", "ledger"]) {
          try { await membrane[op]({}); out.push("reached"); } catch (e) { out.push("blocked"); }
        }
        return out;"#,
        &["read_file"],
        &d,
    )
    .await;
    assert!(!r.is_error, "{}", r.content);
    assert_eq!(r.content, r#"["blocked","blocked","blocked"]"#);
    assert!(
        d.calls.lock().unwrap().is_empty(),
        "membrane bypassed allowlist"
    );

    // With the target allowlisted, only that operation reaches the dispatcher.
    let r = run(
        r#"await membrane.blueprint_search({q:"x"});
        try { await membrane.ledger({}); return "ledger reached"; } catch (e) { return "ledger blocked"; }"#,
        &["blueprint_search"],
        &d,
    )
    .await;
    assert_eq!(r.content, r#""ledger blocked""#);
    let calls = d.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "blueprint_search");
}

#[tokio::test]
async fn membrane_cannot_reach_mutating_tools_in_read_only_profile() {
    let d = Recorder::default();
    let r = run(
        r#"try { await membrane.write_file({}); return "reached"; } catch (e) { return "blocked"; }"#,
        &["read_file"],
        &d,
    )
    .await;
    assert!(d.calls.lock().unwrap().is_empty());
    assert!(r.is_error || r.content == r#""blocked""#, "{}", r.content);
}

#[test]
fn stage_one_allowlist_is_closed_local_read_only() {
    let mut registry = ToolRegistry::new();
    for name in [
        "read_file",
        "list_files",
        "search",
        "web_fetch",
        "web_search",
        "browser_fetch",
        "browser_open",
        "browser_read",
        "recursive_fetch",
        "context7",
        "ask_user",
        "brand_new_read_only_tool",
    ] {
        registry.register(Box::new(FakeTool {
            name,
            read_only: true,
        }));
    }
    let tool = RunProgramTool::new(&registry);
    let names: Vec<_> = tool.tool_specs().iter().map(|t| t.name.as_str()).collect();
    for kept in ["read_file", "list_files", "search"] {
        assert!(names.contains(&kept), "{kept} missing");
    }
    for dropped in [
        "web_fetch",
        "web_search",
        "browser_fetch",
        "browser_open",
        "browser_read",
        "recursive_fetch",
        "context7",
        "ask_user",
        "brand_new_read_only_tool",
    ] {
        assert!(!names.contains(&dropped), "{dropped} must not be exposed");
    }
}

#[tokio::test]
async fn network_and_interactive_tools_are_rejected_even_if_specs_name_them() {
    let d = Recorder::default();
    for tool in [
        "web_fetch",
        "browser_fetch",
        "web_search",
        "recursive_fetch",
        "context7",
        "ask_user",
        "browser_open",
    ] {
        let source = format!(r#"return await tools.{tool}({{url:"https://example.invalid"}});"#);
        let r = run(&source, &["read_file", tool], &d).await;
        assert!(r.is_error, "{tool}: {}", r.content);
    }
    assert!(
        d.calls.lock().unwrap().is_empty(),
        "excluded tool reached dispatcher"
    );
}

#[test]
fn registered_membrane_readers_are_exposed_to_programs() {
    let mut registry = ToolRegistry::new();
    for name in [
        "blueprint_search",
        "blueprint_expand",
        "membrane_source_read",
    ] {
        registry.register(Box::new(FakeTool {
            name,
            read_only: true,
        }));
    }
    let tool = RunProgramTool::new(&registry);
    let names: Vec<_> = tool.tool_specs().iter().map(|t| t.name.as_str()).collect();
    for expected in [
        "blueprint_search",
        "blueprint_expand",
        "membrane_source_read",
    ] {
        assert!(names.contains(&expected), "{expected} missing from specs");
    }
}

#[tokio::test]
async fn membrane_reader_names_are_dispatched_not_rejected_as_unknown() {
    let d = Recorder::default();
    let r = run(
        r#"await tools.blueprint_search({q:"a"});
        await tools.blueprint_expand({id:"b"});
        await tools.membrane_source_read({path:"c"});
        await membrane.blueprint_search({q:"a"});
        await membrane.blueprint_expand({id:"b"});
        await membrane.ledger({path:"c"});
        return "ok";"#,
        &[
            "blueprint_search",
            "blueprint_expand",
            "membrane_source_read",
        ],
        &d,
    )
    .await;
    assert!(!r.is_error, "{}", r.content);
    let calls = d.calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "blueprint_search",
            "blueprint_expand",
            "membrane_source_read",
            "blueprint_search",
            "blueprint_expand",
            "membrane_source_read",
        ]
    );
}

#[tokio::test]
async fn expired_deadline_blocks_admission() {
    let d = Recorder::default();
    let ctx = ToolContext::new(
        "code-mode-test",
        std::env::temp_dir(),
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );
    let r =
        invoke_before_deadline(&d, "read_file", json!({"path":"a"}), &ctx, Instant::now()).await;
    assert!(r.is_error);
    assert!(r.content.contains("not attempted"), "{}", r.content);
    assert!(d.calls.lock().unwrap().is_empty());
}
