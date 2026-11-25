//! End to end: a real `Service` (generated TLS identity, real listener on port 0, real trust file)
//! driven through the JSON command surface a host's webview uses (`commands::invoke`).
//!
//! It proves the snapshot names the service's own identity, that a trust decision and a label set
//! through the commands are listed back, that both survive a restart of the service on the same
//! trust file, that forgetting removes the device, and that an unknown command is refused.
//! Two services on one computer cannot exchange files: the crate ignores devices heard from its
//! own addresses, loopback included.

use rightkit_send::{Config, Identity, Policy, Service, commands};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rightkit-send-commands-e2e-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create scratch");
        Scratch(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(root: &Path) -> Config {
    let save_dir = root.join("save");
    std::fs::create_dir_all(&save_dir).expect("create save dir");
    Config {
        product_name: "rightkit-send-e2e".into(),
        alias: "e2e".into(),
        port: 0,
        device_type: "headless".into(),
        device_model: String::new(),
        save_dir,
        trust_path: root.join("trust.json"),
        user_agent: "rightkit-send-e2e".into(),
        quarantine_agent: "rightkit-send-e2e".into(),
        temp_suffix: ".part".into(),
        log_prefix: "e2e:".into(),
        policy: Policy::default(),
    }
}

fn start(root: &Path, identity: &Identity) -> Service {
    Service::start(config(root), identity.clone(), Arc::new(|_| {})).expect("service starts")
}

/// The trust entry for `fingerprint` from `trustList`, if listed.
fn listed(service: &Service, fingerprint: &str) -> Option<Value> {
    let list = commands::invoke(service, "trustList", &json!({})).expect("trustList");
    list.as_array()
        .expect("trustList is an array")
        .iter()
        .find(|e| {
            e["fingerprint"]
                .as_str()
                .is_some_and(|f| f.eq_ignore_ascii_case(fingerprint))
        })
        .cloned()
}

#[test]
fn trust_set_through_the_commands_survives_a_restart() {
    let scratch = Scratch::new();
    let me = Identity::generate("e2e").expect("own identity");
    let other = Identity::generate("other").expect("other identity");

    let service = start(&scratch.0, &me);
    assert_eq!(service.snapshot().fingerprint, me.fingerprint);
    assert_ne!(service.bound_port(), 0, "the listener is bound");

    commands::invoke(
        &service,
        "trustSet",
        &json!({ "fingerprint": other.fingerprint, "state": "allow" }),
    )
    .expect("trustSet");
    commands::invoke(
        &service,
        "trustLabel",
        &json!({ "fingerprint": other.fingerprint, "label": "Desk" }),
    )
    .expect("trustLabel");
    let entry = listed(&service, &other.fingerprint).expect("listed after trustSet");
    assert_eq!(entry["label"], "Desk");
    service.stop();
    assert!(
        scratch.0.join("trust.json").exists(),
        "the trust decision is written to the trust file"
    );

    let again = start(&scratch.0, &me);
    let entry = listed(&again, &other.fingerprint).expect("still listed after a restart");
    assert_eq!(entry["label"], "Desk");

    commands::invoke(
        &again,
        "trustForget",
        &json!({ "fingerprint": other.fingerprint }),
    )
    .expect("trustForget");
    assert!(listed(&again, &other.fingerprint).is_none(), "forgotten");

    assert!(
        commands::invoke(&again, "noSuchCommand", &json!({})).is_err(),
        "an unknown command is refused"
    );
    again.stop();
}
