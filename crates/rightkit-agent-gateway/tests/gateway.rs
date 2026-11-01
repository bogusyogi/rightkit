//! Behaviour that needs only an in-memory SQLite database and no host.
use rightkit_agent_gateway::content::{untrusted, BodyBudget};
use rightkit_agent_gateway::outcome::{from_db_error, to_json, AgentError, Profile};
use rightkit_agent_gateway::redact::find_secret;
use rightkit_agent_gateway::registry::{self, Verified};
use rightkit_db::rusqlite::Connection;
use serde_json::json;

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE accounts (id TEXT PRIMARY KEY);
         CREATE TABLE agents (
           id TEXT PRIMARY KEY, name TEXT NOT NULL, key_hash TEXT NOT NULL,
           profile TEXT NOT NULL CHECK(profile IN ('read_only','draft','send_with_approval')),
           created_at INTEGER NOT NULL, revoked_at INTEGER);
         CREATE TABLE agent_accounts (
           agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
           account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
           PRIMARY KEY (agent_id, account_id));",
    )
    .unwrap();
    c
}

#[test]
fn a_new_agent_is_read_only_and_its_key_verifies_until_revoked() {
    let c = db();
    let (id, key) = registry::create_agent(&c, "Claude").unwrap();
    match registry::verify_key(&c, &key).unwrap() {
        Verified::Agent(a) => {
            assert_eq!(a.id, id);
            assert_eq!(a.profile, Profile::ReadOnly);
            assert!(a.accounts.is_empty());
        }
        other => panic!("expected an agent, got {other:?}"),
    }
    assert!(matches!(
        registry::verify_key(&c, "not-the-key").unwrap(),
        Verified::Unknown
    ));
    registry::revoke(&c, &id).unwrap();
    assert!(matches!(
        registry::verify_key(&c, &key).unwrap(),
        Verified::Revoked { .. }
    ));
}

#[test]
fn an_agent_name_with_control_characters_is_refused() {
    let c = db();
    assert!(registry::create_agent(&c, "bad\u{202e}name").is_err());
    assert!(registry::create_agent(&c, "   ").is_err());
}

#[test]
fn sender_text_is_wrapped_and_capped() {
    let v = untrusted(&"x".repeat(5_000));
    assert_eq!(v["untrusted"], json!(true));
    assert_eq!(v["truncated"], json!(true));
    let mut budget = BodyBudget::with_limit(10);
    let body = budget.body("0123456789abcdef", false);
    assert_eq!(body["text"], json!("0123456789"));
    assert_eq!(body["truncated"], json!(true));
}

#[test]
fn errors_serialise_with_a_stable_code_and_the_locked_text_maps_to_app_locked() {
    let v = to_json(&Err(AgentError::Unauthorised));
    assert_eq!(v["error"]["code"], json!("unauthorised"));
    assert_eq!(
        from_db_error(rightkit_agent_gateway::access::LOCKED_MESSAGE),
        AgentError::AppLocked
    );
    assert_eq!(from_db_error("disk on fire"), AgentError::Internal);
}

#[test]
fn the_redaction_check_finds_a_planted_token() {
    let leak = format!("{}{}", "Bear", "er abc");
    assert!(find_secret(&json!({"a": [leak]}), &[]).is_some());
    assert!(find_secret(&json!({"a": "plain"}), &[]).is_none());
}
