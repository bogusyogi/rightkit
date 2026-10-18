//! M22 conformance: explicit `additionalProperties: false` rejection through the
//! toolbox gate (exposure -> allowlist -> schema -> admission, invalid input never
//! executes), and the control-method list asserted against the QA crate's dispatch
//! table so the seven newer methods cannot drift.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rightkit_agent_tools::*;
use serde_json::{json, Value};

struct Echo {
    spec: ToolSpec,
    calls: Arc<AtomicUsize>,
}

impl Tool for Echo {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn call(&self, _ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(args)
    }
}

struct DenyAll(Arc<AtomicUsize>);

impl ToolAdmission for DenyAll {
    fn approve(&self, _req: &ToolRequest) -> Verdict {
        self.0.fetch_add(1, Ordering::SeqCst);
        Verdict::Deny("no".into())
    }
}

fn toolbox(schema: Value) -> (Toolbox, Arc<AtomicUsize>, Arc<MemoryJournal>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let journal = MemoryJournal::new();
    let mut tb = Toolbox::new();
    tb.register(Arc::new(Echo {
        spec: ToolSpec::new("echo.run", "echo", Effect::Observe, schema),
        calls: calls.clone(),
    }))
    .unwrap();
    tb.allow("tester", ["echo.run"]);
    tb.add_journal(journal.clone());
    (tb, calls, journal)
}

fn closed_schema() -> Value {
    json!({"type": "object", "required": ["a"], "properties": {"a": {"type": "string"}}, "additionalProperties": false})
}

#[test]
fn explicit_false_extra_is_denied_by_arguments_and_never_executes() {
    let (tb, calls, journal) = toolbox(closed_schema());
    let err = tb
        .call(
            &Caller::new("tester"),
            "echo.run",
            json!({"a": "x", "b": 1}),
        )
        .unwrap_err();
    assert_eq!(err.code, ERR_INVALID_ARGS);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(journal.events().iter().any(|e| matches!(
        e,
        ToolEvent::Denied {
            by: DeniedBy::Arguments,
            ..
        }
    )));
    assert_eq!(tb.metrics().denied, 1);
    // Declared arguments still run.
    assert!(tb
        .call(&Caller::new("tester"), "echo.run", json!({"a": "x"}))
        .is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn open_schemas_keep_current_acceptance() {
    for schema in [
        json!({"type": "object", "properties": {"a": {"type": "string"}}}),
        json!({"type": "object", "properties": {"a": {"type": "string"}}, "additionalProperties": true}),
        // Schema-object extras are accepted and their values stay unvalidated.
        json!({"type": "object", "properties": {"a": {"type": "string"}}, "additionalProperties": {"type": "string"}}),
    ] {
        let (tb, calls, _) = toolbox(schema);
        assert!(tb
            .call(
                &Caller::new("tester"),
                "echo.run",
                json!({"a": "x", "b": 1})
            )
            .is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    // Null arguments read as {}.
    let (tb, calls, _) = toolbox(json!({"type": "object", "additionalProperties": false}));
    assert!(tb
        .call(&Caller::new("tester"), "echo.run", Value::Null)
        .is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn gate_order_allowlist_then_schema_then_admission() {
    // Allowlist beats schema: an unlisted caller is refused before arguments are read.
    let (tb, calls, _) = toolbox(closed_schema());
    let err = tb
        .call(&Caller::new("stranger"), "echo.run", json!({"b": 1}))
        .unwrap_err();
    assert_eq!(err.code, ERR_NOT_ALLOWED);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Schema beats admission: invalid arguments never reach the admission hook.
    let (mut tb, calls, _) = toolbox(closed_schema());
    let approvals = Arc::new(AtomicUsize::new(0));
    tb.set_admission(Arc::new(DenyAll(approvals.clone())));
    let err = tb
        .call(
            &Caller::new("tester"),
            "echo.run",
            json!({"a": "x", "b": 1}),
        )
        .unwrap_err();
    assert_eq!(err.code, ERR_INVALID_ARGS);
    assert_eq!(approvals.load(Ordering::SeqCst), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Valid arguments reach admission, whose denial still executes nothing.
    let err = tb
        .call(&Caller::new("tester"), "echo.run", json!({"a": "x"}))
        .unwrap_err();
    assert_eq!(err.code, ERR_DENIED);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

const SEVEN: [&str; 7] = [
    "pointer",
    "set_viewport",
    "focus",
    "lease_acquire",
    "lease_release",
    "lease_reconcile",
    "lease_status",
];

/// `"name",` rows of the QA server's `METHODS` array, in order.
fn qa_methods(source: &str) -> Vec<String> {
    let start = source
        .find("pub const METHODS")
        .expect("QA METHODS table not found");
    let rest = &source[start..];
    let open = rest.find('[').unwrap();
    let close = rest.find("];").unwrap();
    rest[open..close]
        .lines()
        .filter_map(|line| {
            let t = line.trim().strip_prefix('"')?;
            t.strip_suffix("\",").map(str::to_string)
        })
        .collect()
}

#[test]
fn control_methods_match_the_qa_dispatch_table() {
    for m in SEVEN {
        assert!(CONTROL_METHODS.contains(&m), "{m}");
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rightkit-qa/src/embedded/mod.rs");
    let Ok(source) = std::fs::read_to_string(&path) else {
        eprintln!("rightkit-qa source not found at {path:?}; skipping the drift check");
        return;
    };
    let qa = qa_methods(&source);
    assert!(qa.len() >= 21, "parsed only {} QA methods", qa.len());
    assert_eq!(
        qa.iter().map(String::as_str).collect::<Vec<_>>(),
        CONTROL_METHODS.to_vec(),
        "control methods drifted from QA METHODS"
    );
}

#[test]
fn the_seven_methods_register_as_tools_with_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let suite = rightkit_daemon::Suite::at(dir.path().join("suite")).unwrap();
    let profile = AppProfile::standard("demo");
    let tools = control_tools(&suite, &profile);
    let spec = |name: &str| {
        tools
            .iter()
            .map(|t| t.spec())
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
    };
    for m in SEVEN {
        let s = spec(&format!("demo.{m}"));
        assert!(!s.description.is_empty(), "{m}");
        assert_eq!(s.input_schema["type"], "object", "{m}");
    }
    let required = |name: &str| -> Vec<String> {
        spec(name).input_schema["required"]
            .as_array()
            .map(|r| {
                r.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(required("demo.pointer"), ["phase", "x", "y"]);
    assert_eq!(required("demo.set_viewport"), ["width", "height"]);
    assert_eq!(required("demo.lease_release"), ["leaseEpoch"]);
    assert_eq!(required("demo.lease_reconcile"), ["leaseEpoch", "decision"]);
    assert_eq!(spec("demo.lease_status").effect, Effect::Observe);
    // The standard profile's allowlist grants exactly the registered tools.
    let mut tb = Toolbox::new();
    tb.register_all(tools).unwrap();
    tb.allow("tester", profile.allow_patterns());
    let listed: Vec<String> = tb.list_for("tester").into_iter().map(|s| s.name).collect();
    for m in SEVEN {
        assert!(listed.contains(&format!("demo.{m}")), "{m}");
    }
}
