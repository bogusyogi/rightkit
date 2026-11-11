//! Registry consumer for rightkit-owner-approvals 0.1.0: builds against the published API only.
use rightkit_owner_approvals::approval_sig::{self, Signed};
use rightkit_owner_approvals::{audit, Refusal};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        Refusal::new("approval_unknown", "no such approval").code == "approval_unknown",
        "refusals carry a stable code (the approvals core's type)",
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
