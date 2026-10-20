//! Negative tests for the hard exposure rules, through the public API only: a paste or
//! pasteboard tool cannot be registered or called, and HeardRight has no generic read channel
//! (`eval`, `dom`, `ax`, `screenshot`, `wait`, `batch`) under any profile that names it. None of
//! these calls reaches a service: every refusal happens in the gate, before any connection.

use rightkit_agent_tools::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Probe {
    spec: ToolSpec,
    calls: Arc<AtomicUsize>,
}

impl Tool for Probe {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    /// The inner verb, as `<app>.command` carries one: the `name` argument.
    fn subject(&self, args: &Value) -> Option<String> {
        args["name"].as_str().map(str::to_string)
    }
    fn call(&self, _ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(args)
    }
}

fn probe(name: &str, calls: &Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(Probe {
        spec: ToolSpec::new(
            name,
            "probe",
            Effect::Act,
            json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        ),
        calls: calls.clone(),
    })
}

#[test]
fn a_paste_or_pasteboard_tool_cannot_register_for_any_app() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tb = Toolbox::new();
    for name in [
        "paste",
        "desktop.paste",
        "genright.paste",
        "genright.pasteText",
        "genright.paste_text",
        "desktop.clipboard",
        "desktop.getClipboard",
        "browser.read-pasteboard",
        "app.readpasteboard",
        "app.pasting",
    ] {
        let err = tb.register(probe(name, &calls)).unwrap_err();
        assert_eq!(err.code, ERR_NOT_EXPOSED, "{name}");
    }
    assert!(
        tb.list_all().is_empty(),
        "a refused tool is not even listed"
    );
    // A dictation app refuses the dictation names as well, but only that app.
    tb.protect_dictation("heardright");
    for name in [
        "heardright.start_dictation",
        "heardright.history",
        "heardright.pushToTalk",
    ] {
        assert_eq!(
            tb.register(probe(name, &calls)).unwrap_err().code,
            ERR_NOT_EXPOSED,
            "{name}"
        );
    }
    tb.register(probe("genright.history", &calls)).unwrap();
}

#[test]
fn a_paste_command_is_refused_even_for_a_caller_allowed_everything() {
    let calls = Arc::new(AtomicUsize::new(0));
    let journal = MemoryJournal::new();
    let mut tb = Toolbox::new();
    tb.register(probe("genright.command", &calls)).unwrap();
    tb.allow("tester", ["*"]);
    tb.add_journal(journal.clone());
    for name in [
        "paste",
        "paste_text",
        "pasteText",
        "readPasteboard",
        "getclipboardtext",
    ] {
        let err = tb
            .call(
                &Caller::new("tester"),
                "genright.command",
                json!({"name": name}),
            )
            .unwrap_err();
        assert_eq!(err.code, ERR_NOT_EXPOSED, "{name}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a refused command never runs"
    );
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|e| matches!(
                e,
                ToolEvent::Denied {
                    by: DeniedBy::Exposure,
                    ..
                }
            ))
            .count(),
        5
    );
    // An ordinary command passes the same gate.
    assert!(tb
        .call(
            &Caller::new("tester"),
            "genright.command",
            json!({"name": "list_models"})
        )
        .is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

const GENERIC_READS: [&str; 6] = ["eval", "dom", "ax", "screenshot", "wait", "batch"];

fn suite() -> (tempfile::TempDir, rightkit_daemon::Suite) {
    let dir = tempfile::tempdir().unwrap();
    let suite = rightkit_daemon::Suite::at(dir.path().join("suite")).unwrap();
    (dir, suite)
}

fn assert_no_generic_reads(tb: &Toolbox) {
    let names: Vec<String> = tb.list_all().into_iter().map(|s| s.name).collect();
    for m in GENERIC_READS {
        let tool = format!("heardright.{m}");
        assert!(!names.contains(&tool), "{tool} must not exist: {names:?}");
        // Called by name, as the sole caller allowed everything, it is simply unknown.
        let err = tb
            .call(
                &Caller::new(CTL_CALLER),
                &tool,
                json!({"js": "1", "selector": "x"}),
            )
            .unwrap_err();
        assert_eq!(err.code, ERR_UNKNOWN_TOOL, "{tool}");
    }
}

/// A sensitive profile's only app command channel is its listed read-only commands; `eval` is not
/// one of them, and `heardright.command` refuses it at the allowlist, before any connection.
fn assert_commands_are_listed_only(tb: &Toolbox) {
    for command in [
        "eval",
        "dom",
        "screenshot",
        "transcribe_all",
        "start_dictation",
        "pasteText",
    ] {
        let err = tb
            .call(
                &Caller::new(CTL_CALLER),
                "heardright.command",
                json!({"name": command}),
            )
            .unwrap_err();
        assert!(
            err.code == ERR_NOT_ALLOWED || err.code == ERR_NOT_EXPOSED,
            "{command}: {err}"
        );
    }
}

#[test]
fn heardright_eval_and_every_generic_read_is_refused() {
    let (_dir, suite) = suite();
    // The built-in profile.
    let tb = control_toolbox(&suite, &["heardright"]).unwrap();
    assert_no_generic_reads(&tb);
    assert_commands_are_listed_only(&tb);
    // A sensitive profile that asks for the generic reads by name still gets none of them.
    let mut tb = Toolbox::new();
    let asks_for_reads = AppProfile::sensitive("heardright", &["health"])
        .with_dictation_rules()
        .with_methods(&[
            "health",
            "command",
            "eval",
            "dom",
            "ax",
            "screenshot",
            "wait",
            "batch",
        ]);
    add_app(&mut tb, &suite, asks_for_reads).unwrap();
    assert_no_generic_reads(&tb);
    assert_commands_are_listed_only(&tb);
}

#[test]
fn a_standard_profile_registered_as_heardright_is_as_closed_as_the_built_in_one() {
    let (_dir, suite) = suite();
    let mut tb = Toolbox::new();
    add_app(&mut tb, &suite, AppProfile::standard("heardright")).unwrap();
    assert_no_generic_reads(&tb);
    // The dictation layer applies although the profile did not ask for it.
    for command in ["start_dictation", "chatHistory", "delivery_status"] {
        let err = tb
            .call(
                &Caller::new(CTL_CALLER),
                "heardright.command",
                json!({"name": command}),
            )
            .unwrap_err();
        assert_eq!(err.code, ERR_NOT_EXPOSED, "{command}");
    }
    // Another app keeps the standard profile's methods and its own history command.
    let mut tb = Toolbox::new();
    add_app(&mut tb, &suite, AppProfile::standard("genright")).unwrap();
    let names: Vec<String> = tb.list_all().into_iter().map(|s| s.name).collect();
    for m in GENERIC_READS {
        assert!(names.contains(&format!("genright.{m}")), "{m}");
    }
}
