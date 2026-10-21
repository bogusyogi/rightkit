// rightkit-agent-tools 0.3.0 consumed by exact registry pin, outside the workspace.
//
// Proves two public surfaces:
// (1) The M22 applicability record that an app's rightkit-mandatory.json carries: a client
// record parses and declares an agent surface with its entrypoint kept; an absent record declares
// no surface; a manifest without an M22 key is valid and yields None; non-JSON text and an absent
// record that lists entrypoints are refused.
// (2) Hosted admission (PK15): a hosted `Toolbox` whose `ToolAdmission` hook denies the call
// executes nothing — the tool body never runs, the error is `denied_by_admission`, and the
// journal records `Denied{by: Admission}`. A caller outside the allowlist is refused with
// `tool_not_allowed` before admission, and a paste-named tool cannot even register
// (`not_exposed`), in camelCase exactly as in snake_case. Public API only.
// Does not prove: the backends, the control toolbox, or any daemon transport.
// State: in-memory values only. No files, no network, no process spawn, no user data.
use rightkit_agent_tools::{
    AgentRole, ApplicabilityRecord, CallCtx, Caller, DeniedBy, Effect, MemoryJournal, Tool,
    ToolAdmission, ToolError, ToolEvent, ToolRequest, ToolSpec, Toolbox, Verdict, ERR_DENIED,
    ERR_NOT_ALLOWED, ERR_NOT_EXPOSED,
};
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
    fn call(&self, _ctx: &CallCtx, _args: Value) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ran": true}))
    }
}

fn probe(name: &str, calls: &Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(Probe {
        spec: ToolSpec::new(
            name,
            "hosted probe",
            Effect::Act,
            json!({"type": "object", "properties": {}}),
        ),
        calls: calls.clone(),
    })
}

/// The host's admission hook: denies every call (a refused approval).
struct DenyAll;
impl ToolAdmission for DenyAll {
    fn approve(&self, _req: &ToolRequest) -> Verdict {
        Verdict::Deny("host refused approval".into())
    }
}

fn hosted_admission() {
    let calls = Arc::new(AtomicUsize::new(0));
    let journal = MemoryJournal::new();
    let mut tb = Toolbox::new();
    tb.register(probe("hosted.act", &calls))
        .expect("ordinary tool registers");
    tb.allow("host", ["hosted.act"]);
    tb.set_admission(Arc::new(DenyAll));
    tb.add_journal(journal.clone());

    // A paste-named tool cannot register, in camelCase exactly as in snake_case.
    let err = tb
        .register(probe("hosted.pasteText", &calls))
        .expect_err("paste tool registration is refused");
    assert_eq!(err.code, ERR_NOT_EXPOSED, "paste tool is not exposed");

    // A caller outside the allowlist is refused before admission.
    let err = tb
        .call(&Caller::new("stranger"), "hosted.act", json!({}))
        .expect_err("unlisted caller is refused");
    assert_eq!(err.code, ERR_NOT_ALLOWED, "allowlist refusal code");

    // The allowed caller still executes nothing when admission denies.
    let err = tb
        .call(&Caller::new("host"), "hosted.act", json!({}))
        .expect_err("denied approval is refused");
    assert_eq!(err.code, ERR_DENIED, "admission denial code");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "denied approval causes no effect: the tool body never ran"
    );

    let events = journal.events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            ToolEvent::Denied { by: DeniedBy::Admission, tool, .. } if tool == "hosted.act"
        )),
        "journal records the admission denial"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ToolEvent::Started { .. })),
        "no call ever started"
    );
    assert_eq!(tb.metrics().denied, 2, "both refusals are counted");
    assert_eq!(tb.metrics().ok, 0, "nothing succeeded");
}

fn main() {
    let client = ApplicabilityRecord::from_manifest(
        r#"{"M22":{"role":"client","entrypoints":["src/agent.rs#register_tools"]}}"#,
    )
    .expect("client record is valid")
    .expect("M22 record is present");
    assert_eq!(client.role, AgentRole::Client, "role parses as client");
    assert!(client.has_agent_surface(), "a client declares an agent surface");
    assert_eq!(
        client.entrypoints,
        vec!["src/agent.rs#register_tools".to_string()],
        "entrypoint is kept"
    );

    let absent = ApplicabilityRecord::from_manifest(r#"{"M22":{"role":"absent","entrypoints":[]}}"#)
        .expect("absent record is valid")
        .expect("M22 record is present");
    assert_eq!(absent.role, AgentRole::Absent, "role parses as absent");
    assert!(!absent.has_agent_surface(), "an absent app declares no agent surface");

    assert!(
        ApplicabilityRecord::from_manifest("{}")
            .expect("manifest without M22 is valid")
            .is_none(),
        "the M22 record is optional"
    );
    assert!(
        ApplicabilityRecord::from_manifest("not json").is_err(),
        "non-JSON manifest is refused"
    );
    assert!(
        ApplicabilityRecord::from_manifest(
            r#"{"M22":{"role":"absent","entrypoints":["src/agent.rs#register_tools"]}}"#,
        )
        .is_err(),
        "an absent record with entrypoints is refused"
    );

    hosted_admission();

    println!("OK rightkit-agent-tools 0.3.0 (registry consumer)");
}
