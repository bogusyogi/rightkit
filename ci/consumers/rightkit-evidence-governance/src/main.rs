// rightkit-evidence-governance consumed by the exact registry pin, outside the workspace.
// Proves: neutral key ring identity, explicit key dir precedence, workspace state root layout.
use rightkit_evidence_governance::key_ring::{key_ring_identity, resolve_key_dir, KeyDirSource};
use rightkit_evidence_governance::state_root;
use std::path::Path;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        key_ring_identity().env_var == "RIGHTKIT_KEY_DIR",
        "neutral key dir variable",
    );
    let dir = std::env::temp_dir().join("rightkit-governance-consumer-keys");
    let resolved = resolve_key_dir(Some(&dir)).expect("explicit key dir");
    check(
        resolved == (dir.clone(), KeyDirSource::Explicit),
        "explicit key dir wins",
    );
    let root = state_root(Path::new("ws"));
    check(
        root.starts_with("ws") && root.ends_with("arcane"),
        "workspace state root layout",
    );
}
