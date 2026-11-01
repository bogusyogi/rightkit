//! Registry consumer for rightkit-owner-approvals 0.1.0: builds against the published API only.
use rightkit_owner_approvals::approval_sig::{self, Signed};
use rightkit_owner_approvals::{audit, redact::redact};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let token = format!("gh{}{}", "p_", "b".repeat(36));
    let (clean, count) = redact(&format!("x {token} y"));
    check(
        count == 1 && clean == "x [redacted] y",
        "a token assembled at run time is masked",
    );
    let message = approval_sig::message("approve", "r", "a", "c", "d", "k", "n", 1, 2);
    check(
        message.starts_with(b"pulse.approval.v1\nkind:approve"),
        "the signed message keeps its wire layout",
    );
    check(
        Signed::from_value(&serde_json::json!({})).is_err(),
        "a signature without fields is refused",
    );
    check(
        audit::client("a b") == "ab" && audit::client("") == "unknown",
        "audit client names are made safe",
    );
    println!("rightkit-owner-approvals consumer: all checks passed");
}
