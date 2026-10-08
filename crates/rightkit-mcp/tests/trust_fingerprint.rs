//! Approval binds the launch configuration by value, and the store never holds env values.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rightkit_mcp::{fingerprint, ServerSpec, TrustDecision, TrustStore};

const SECRET: &str = "sk-live-canary-7f3a9c2e51";

fn stdio(env: &[(&str, &str)]) -> ServerSpec {
    ServerSpec::Stdio {
        command: "node".into(),
        args: vec!["server.js".into()],
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        cwd: Some(PathBuf::from("/proj")),
    }
}

#[test]
fn changed_env_value_invalidates_approval() {
    let mut store = TrustStore::new();
    let approved = stdio(&[("PATH", "/usr/bin"), ("NODE_OPTIONS", "")]);
    store.approve("/proj", "srv", &approved);
    assert_eq!(
        store.decide("/proj", "srv", &approved),
        TrustDecision::Approved
    );

    let path_changed = stdio(&[("PATH", "/tmp/evil"), ("NODE_OPTIONS", "")]);
    assert_eq!(
        store.decide("/proj", "srv", &path_changed),
        TrustDecision::Pending
    );

    let node_options_changed = stdio(&[
        ("PATH", "/usr/bin"),
        ("NODE_OPTIONS", "--require /tmp/x.js"),
    ]);
    assert_eq!(
        store.decide("/proj", "srv", &node_options_changed),
        TrustDecision::Pending
    );
    assert_ne!(fingerprint(&approved), fingerprint(&path_changed));
}

#[test]
fn identical_config_stays_approved_after_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trust.json");
    let spec = stdio(&[("PATH", "/usr/bin"), ("TOKEN", SECRET)]);
    let mut store = TrustStore::new();
    store.approve("/proj", "srv", &spec);
    store.save(&path).unwrap();

    let reloaded = TrustStore::load(&path);
    assert_eq!(
        reloaded.decide(
            "/proj",
            "srv",
            &stdio(&[("TOKEN", SECRET), ("PATH", "/usr/bin")])
        ),
        TrustDecision::Approved,
        "env map ordering must not affect identity"
    );
}

#[test]
fn stored_approval_contains_no_env_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trust.json");
    let mut store = TrustStore::new();
    store.approve(
        "/proj",
        "srv",
        &stdio(&[("TOKEN", SECRET), ("PATH", "/secret/bin")]),
    );
    store.save(&path).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains(SECRET),
        "env value leaked into approval file"
    );
    assert!(
        !text.contains("/secret/bin"),
        "env value leaked into approval file"
    );
    assert!(text.contains("sha256:"));
}

#[test]
fn fingerprint_is_value_sensitive_and_version_bound() {
    let a = fingerprint(&stdio(&[("K", "1")]));
    let b = fingerprint(&stdio(&[("K", "2")]));
    assert_ne!(a, b);
    assert_eq!(a, fingerprint(&stdio(&[("K", "1")])));
}
