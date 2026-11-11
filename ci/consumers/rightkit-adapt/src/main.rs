//! Registry consumer for rightkit-adapt 0.2.0 (Pulse's governed learning, `rightkit-adapt-taste` until
//! 0.1.2): builds against the published API only.
use rightkit_adapt::canonical::{
    derive_preference_id, sha256_canonical, to_canonical_json, PREFERENCE_ID_PREFIX,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        to_canonical_json(&serde_json::json!({"b": 2, "a": 1})) == r#"{"a":1,"b":2}"#,
        "canonical JSON sorts keys",
    );
    let a = derive_preference_id("global", "style", "Prefer short answers");
    check(
        a == derive_preference_id("global", "style", "Prefer short answers"),
        "preference ids are deterministic",
    );
    check(
        a.starts_with(PREFERENCE_ID_PREFIX),
        "preference ids carry the published prefix",
    );
    check(
        sha256_canonical(&serde_json::json!(1)).len() >= 64,
        "canonical digests are sha256 hex",
    );
    println!("rightkit-adapt consumer: all checks passed");
}
