//! Trust gate before spawn, caller-supplied fingerprints and legacy-store migration
//! (rightkit#550).

use std::collections::BTreeMap;

use rightkit_mcp::{ImportedTrust, ServerSource, ServerSpec, TrustDecision, TrustStore};
use serde_json::{json, Value};

fn stdio(arg: &str) -> ServerSpec {
    ServerSpec::Stdio {
        command: "node".into(),
        args: vec![arg.into()],
        env: BTreeMap::new(),
        cwd: None,
    }
}

fn http() -> ServerSpec {
    ServerSpec::Http {
        url: "https://example.test/mcp".into(),
    }
}

fn ws(name: &str) -> ServerSource {
    ServerSource::Workspace(name.into())
}

#[test]
fn gate_is_pending_then_approved_then_pending_after_a_change() {
    let mut store = TrustStore::new();
    let spec = stdio("a.js");
    assert_eq!(store.gate_stdio("/p", "s", &spec), TrustDecision::Pending);
    store.approve("/p", "s", &spec);
    assert_eq!(store.gate_stdio("/p", "s", &spec), TrustDecision::Approved);
    assert_eq!(
        store.gate_stdio("/p", "s", &stdio("b.js")),
        TrustDecision::Pending
    );
    assert_eq!(
        store.gate_stdio("/other", "s", &spec),
        TrustDecision::Pending
    );
    store.deny("/p", "s", &spec);
    assert_eq!(store.gate_stdio("/p", "s", &spec), TrustDecision::Denied);
}

#[test]
fn gate_stdio_never_approves_an_http_spec() {
    let mut store = TrustStore::new();
    store.approve("/p", "s", &http());
    assert_eq!(store.gate_stdio("/p", "s", &http()), TrustDecision::Pending);
    assert_eq!(
        store.gate(&ws("/p"), "s", &http()),
        TrustDecision::Approved,
        "the general gate still honours it"
    );
}

#[test]
fn gate_exempts_user_servers_and_gates_workspace_servers() {
    let store = TrustStore::new();
    let spec = stdio("a.js");
    assert_eq!(
        store.gate(&ServerSource::User, "s", &spec),
        TrustDecision::Approved
    );
    assert_eq!(store.gate(&ws("/p"), "s", &spec), TrustDecision::Pending);
}

#[test]
fn credential_write_requires_an_exact_approval() {
    let mut store = TrustStore::new();
    let spec = stdio("a.js");
    assert!(store.credential_write_allowed(&ServerSource::User, "s", &spec));
    assert!(!store.credential_write_allowed(&ws("/p"), "s", &spec));
    store.approve("/p", "s", &spec);
    assert!(store.credential_write_allowed(&ws("/p"), "s", &spec));
    assert!(!store.credential_write_allowed(&ws("/p"), "s", &stdio("b.js")));
    assert!(!store.credential_write_allowed(&ws("/q"), "s", &spec));
    store.deny("/p", "s", &spec);
    assert!(!store.credential_write_allowed(&ws("/p"), "s", &spec));
}

#[test]
fn pending_approval_names_the_fingerprint_and_no_env_values() {
    let spec = ServerSpec::Stdio {
        command: "node".into(),
        args: vec!["a.js".into()],
        env: BTreeMap::from([("TOKEN".to_string(), "sk-secret-value".to_string())]),
        cwd: None,
    };
    let pending = TrustStore::new().pending_approval("/p", "s", &spec);
    assert_eq!(pending.env_keys, vec!["TOKEN"]);
    assert!(pending.fingerprint.starts_with("sha256:"));
    assert!(!format!("{pending:?}").contains("sk-secret-value"));
}

#[test]
fn approve_with_fingerprint_matches_only_that_fingerprint() {
    let mut store = TrustStore::new();
    assert!(store.approve_with_fingerprint("/p", "s", "legacy:abc"));
    assert_eq!(
        store.decide_with_fingerprint("/p", "s", "legacy:abc"),
        TrustDecision::Approved
    );
    assert_eq!(
        store.decide_with_fingerprint("/p", "s", "legacy:abd"),
        TrustDecision::Pending
    );
    assert_eq!(
        store.decide("/p", "s", &stdio("a.js")),
        TrustDecision::Pending,
        "the RightKit fingerprint does not match a foreign one"
    );
    assert!(store.deny_with_fingerprint("/p", "s", "legacy:abc"));
    assert_eq!(
        store.decide_with_fingerprint("/p", "s", "legacy:abc"),
        TrustDecision::Denied
    );
}

#[test]
fn an_empty_fingerprint_is_never_recorded_or_matched() {
    let mut store = TrustStore::new();
    assert!(!store.approve_with_fingerprint("/p", "s", ""));
    assert!(!store.deny_with_fingerprint("/p", "s", ""));
    assert_eq!(
        store.decide_with_fingerprint("/p", "s", ""),
        TrustDecision::Pending
    );
}

#[test]
fn a_foreign_fingerprint_does_not_override_an_existing_decision_silently() {
    let mut store = TrustStore::new();
    let spec = stdio("a.js");
    store.approve("/p", "s", &spec);
    store.approve_with_fingerprint("/p", "s", "legacy:abc");
    assert_eq!(
        store.decide("/p", "s", &spec),
        TrustDecision::Pending,
        "one record per workspace and server: the latest decision replaces the earlier"
    );
}

#[test]
fn fingerprint_decisions_survive_an_atomic_save_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trust.json");
    let mut store = TrustStore::new();
    store.approve_with_fingerprint("/p", "s", "legacy:abc");
    store.save(&path).unwrap();
    let reloaded = TrustStore::try_load(&path).unwrap();
    assert_eq!(
        reloaded.decide_with_fingerprint("/p", "s", "legacy:abc"),
        TrustDecision::Approved
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

fn legacy_doc() -> Value {
    json!({"approved": {"/p::s": "legacy:abc"}, "denied": {"/p::t": "legacy:def"}})
}

fn migrate_legacy(doc: &Value) -> Option<Vec<ImportedTrust>> {
    let mut out = Vec::new();
    for (key, approved) in [("approved", true), ("denied", false)] {
        for (k, v) in doc.get(key)?.as_object()? {
            let (workspace, server) = k.split_once("::")?;
            out.push(ImportedTrust {
                workspace: workspace.into(),
                server: server.into(),
                fingerprint: v.as_str()?.into(),
                approved,
            });
        }
    }
    Some(out)
}

#[test]
fn legacy_file_loads_through_the_migration_hook_without_being_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trust.json");
    let text = legacy_doc().to_string();
    std::fs::write(&path, &text).unwrap();
    let store = TrustStore::try_load_migrating(&path, migrate_legacy).unwrap();
    assert_eq!(
        store.decide_with_fingerprint("/p", "s", "legacy:abc"),
        TrustDecision::Approved
    );
    assert_eq!(
        store.decide_with_fingerprint("/p", "t", "legacy:def"),
        TrustDecision::Denied
    );
    assert_eq!(
        store.decide("/p", "s", &stdio("a.js")),
        TrustDecision::Pending
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        text,
        "load writes nothing"
    );
}

#[test]
fn migrated_store_saves_in_the_native_format_and_reloads_without_the_hook() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trust.json");
    std::fs::write(&path, legacy_doc().to_string()).unwrap();
    TrustStore::try_load_migrating(&path, migrate_legacy)
        .unwrap()
        .save(&path)
        .unwrap();
    let reloaded = TrustStore::try_load_migrating(&path, |_| panic!("native: no hook")).unwrap();
    assert_eq!(
        reloaded.decide_with_fingerprint("/p", "s", "legacy:abc"),
        TrustDecision::Approved
    );
}

#[test]
fn unrecognised_file_is_invalid_data_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trust.json");
    std::fs::write(&path, r#"{"something":"else"}"#).unwrap();
    let error = TrustStore::try_load_migrating(&path, |_| None).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read(&path).unwrap(), br#"{"something":"else"}"#);
}

#[test]
fn native_file_skips_the_hook_and_a_bad_native_record_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trust.json");
    let mut store = TrustStore::new();
    store.approve("/p", "s", &stdio("a.js"));
    store.save(&path).unwrap();
    let loaded = TrustStore::try_load_migrating(&path, |_| panic!("native: no hook")).unwrap();
    assert_eq!(
        loaded.decide("/p", "s", &stdio("a.js")),
        TrustDecision::Approved
    );

    std::fs::write(&path, r#"{"records":[{"workspace":1}]}"#).unwrap();
    let error = TrustStore::try_load_migrating(&path, |_| panic!("native: no hook")).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn migrating_load_keeps_missing_corrupt_and_unreadable_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent.json");
    assert!(TrustStore::try_load_migrating(&missing, |_| None).is_ok());
    let corrupt = dir.path().join("corrupt.json");
    std::fs::write(&corrupt, b"{ damaged").unwrap();
    let error = TrustStore::try_load_migrating(&corrupt, |_| Some(vec![])).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
