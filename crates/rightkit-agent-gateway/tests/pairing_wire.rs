//! The agents settings wire carries the 0.2 field names next to the 0.3 ones for one release.
#![cfg(feature = "sql")]

use rightkit_agent_gateway::pairing::{AgentView, AuditView};
use serde_json::json;

fn agent() -> AgentView {
    AgentView {
        id: "a1".into(),
        name: "Engine".into(),
        profile: "read".into(),
        scopes: vec!["acct-1".into(), "acct-2".into()],
        created_at: 10,
        revoked_at: None,
    }
}

fn audit() -> AuditView {
    AuditView {
        id: 7,
        at: 11,
        agent_name: Some("Engine".into()),
        scope: Some("acct-1".into()),
        method: "drafts.list".into(),
        outcome: "ok".into(),
        detail: None,
    }
}

#[test]
fn agent_view_serializes_scopes_and_accounts() {
    let value = serde_json::to_value(agent()).unwrap();
    assert_eq!(value["scopes"], json!(["acct-1", "acct-2"]));
    assert_eq!(value["accounts"], json!(["acct-1", "acct-2"]));
}

#[test]
fn agent_view_reads_either_name() {
    let new: AgentView = serde_json::from_value(json!({
        "id": "a1", "name": "Engine", "profile": "read", "scopes": ["acct-1", "acct-2"],
        "created_at": 10, "revoked_at": null
    }))
    .unwrap();
    let old: AgentView = serde_json::from_value(json!({
        "id": "a1", "name": "Engine", "profile": "read", "accounts": ["acct-1", "acct-2"],
        "created_at": 10, "revoked_at": null
    }))
    .unwrap();
    assert_eq!(new, agent());
    assert_eq!(old, agent());
    // This release's own output carries both names and reads back.
    let round: AgentView = serde_json::from_value(serde_json::to_value(agent()).unwrap()).unwrap();
    assert_eq!(round, agent());
    let missing = serde_json::from_value::<AgentView>(json!({
        "id": "a1", "name": "Engine", "profile": "read", "created_at": 10, "revoked_at": null
    }));
    assert!(missing.is_err());
}

#[test]
fn audit_view_serializes_scope_and_account_id() {
    let value = serde_json::to_value(audit()).unwrap();
    assert_eq!(value["scope"], json!("acct-1"));
    assert_eq!(value["account_id"], json!("acct-1"));
}

#[test]
fn audit_view_reads_either_name() {
    let old: AuditView = serde_json::from_value(json!({
        "id": 7, "at": 11, "agent_name": "Engine", "account_id": "acct-1",
        "method": "drafts.list", "outcome": "ok", "detail": null
    }))
    .unwrap();
    assert_eq!(old, audit());
    let round: AuditView = serde_json::from_value(serde_json::to_value(audit()).unwrap()).unwrap();
    assert_eq!(round, audit());
}
