// rightkit-code-audit-engine consumed by the exact registry pin, outside the workspace.
// Proves: canonical digests are key-order independent, run ids are fresh, and minimize-lens
// guidance is host-supplied (first call wins). No scanners, network or repository access.
use rightkit_code_audit_engine::native_providers::reasoning::lens_plan::set_minimize_lens_guidance;
use rightkit_code_audit_engine::{canonical_bytes, digest, new_run_id};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let a = serde_json::json!({"b": 1, "a": 2});
    let b = serde_json::json!({"a": 2, "b": 1});
    check(
        digest(&a).ok() == digest(&b).ok() && digest(&a).is_ok(),
        "digest ignores key order",
    );
    check(
        canonical_bytes(&a)
            .map(|bytes| bytes == br#"{"a":2,"b":1}"#.to_vec())
            .unwrap_or(false),
        "canonical bytes sort keys",
    );
    check(new_run_id().ok() != new_run_id().ok(), "run ids are fresh");
    check(
        set_minimize_lens_guidance("host guidance") && !set_minimize_lens_guidance("again"),
        "guidance is set once",
    );
}
