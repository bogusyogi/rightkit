// rightkit-agent-tools 0.3.0 consumed by exact registry pin, outside the workspace.
//
// Proves the M22 applicability record that an app's rightkit-mandatory.json carries: a client
// record parses and declares an agent surface with its entrypoint kept; an absent record declares
// no surface; a manifest without an M22 key is valid and yields None; non-JSON text and an absent
// record that lists entrypoints are refused. Public API only.
// Does not prove: the toolbox, backends, journal, exposure rules, or the control toolbox.
// State: in-memory strings only. No files, no network, no process spawn, no user data.
use rightkit_agent_tools::{AgentRole, ApplicabilityRecord};

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

    println!("OK rightkit-agent-tools 0.3.0 (registry consumer)");
}
