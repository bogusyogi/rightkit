// rightkit-code-audit-engine 0.2.0 consumed by exact registry pin (`=0.2.0`), outside the workspace.
//
// Proves, through the public API only, the pure ordering and digest helpers:
// - topological over an empty provider list is an empty order (Ok, no providers run);
// - digest returns the sha256:<64 hex> form of the canonical JSON, and is deterministic for the
//   same value;
// - canonical_bytes serialises a string to its canonical JSON bytes.
// No audit is planned or run: no provider, worktree, browser, child process or network is used.
// No I/O. Exits 1 on the first failed check.
use rightkit_code_audit_engine::{canonical_bytes, digest, topological};

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
        matches!(topological(&[]), Ok(order) if order.is_empty()),
        "an empty provider list orders to an empty plan",
    );

    let first = digest(&"rightkit");
    check(
        matches!(&first, Ok(text) if text.starts_with("sha256:") && text.len() == 71),
        "digest is sha256: followed by 64 hex digits",
    );
    check(
        first.as_ref().ok() == digest(&"rightkit").as_ref().ok(),
        "digest is deterministic for the same value",
    );
    check(
        first.as_ref().ok() != digest(&"other").as_ref().ok(),
        "digest differs for a different value",
    );
    check(
        matches!(canonical_bytes(&"x"), Ok(bytes) if bytes == b"\"x\""),
        "canonical_bytes writes a string as its JSON form",
    );
    println!("OK rightkit-code-audit-engine 0.2.0 (registry consumer)");
}
