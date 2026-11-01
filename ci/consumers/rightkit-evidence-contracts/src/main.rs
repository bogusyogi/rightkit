// rightkit-evidence-contracts consumed by the exact registry pin, outside the workspace.
// Proves: canonical JSON is key-order independent and digests are stable; parse_location splits
// path:line:col; the state identity defaults to the neutral RightKit names.
use rightkit_evidence_contracts::report::parse_location;
use rightkit_evidence_contracts::state_root::{state_identity, user_state_receipts_root};
use rightkit_evidence_contracts::{canonical_digest, canonical_equal, canonical_json_bytes};
use std::path::PathBuf;
use std::process::exit;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        exit(1);
    }
}

fn main() {
    let a = serde_json::json!({"b": 1, "a": [1, 2]});
    let b = serde_json::json!({"a": [1, 2], "b": 1});
    check(
        canonical_equal(&a, &b).unwrap_or(false),
        "canonical_equal ignores key order",
    );
    let bytes = canonical_json_bytes(&a).expect("canonical bytes");
    check(
        bytes == br#"{"a":[1,2],"b":1}"#.to_vec(),
        "canonical_json_bytes sorts keys",
    );
    let d1 = canonical_digest(&a).expect("digest");
    let d2 = canonical_digest(&b).expect("digest");
    check(
        d1 == d2 && d1.starts_with("sha256:"),
        "canonical_digest is stable and prefixed",
    );
    check(
        parse_location("src/lib.rs:12:3").map(|(p, l, _)| (p, l))
            == Some(("src/lib.rs".to_string(), 12)),
        "parse_location splits path and line",
    );
    check(
        state_identity().env_var == "RIGHTKIT_STATE_ROOT",
        "neutral default state identity",
    );
    let root =
        user_state_receipts_root(false, false, Some(PathBuf::from("home")), None, None, None);
    check(
        root == Some(
            PathBuf::from("home")
                .join(".local")
                .join("state")
                .join("rightkit")
                .join("receipts"),
        ),
        "per-user receipts root under the neutral directory",
    );
}
