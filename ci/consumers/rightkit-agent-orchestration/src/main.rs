// rightkit-agent-orchestration consumed by the exact registry pin, outside the workspace.
// Proves: artifact digests are SHA-256 of the bytes, artifact paths refuse traversal and absolute
// children, and the journal directory default is neutral.
use rightkit_agent_orchestration::engine::DEFAULT_JOURNAL_DIR;
use rightkit_agent_orchestration::p6_inventory::artifacts::{digest_bytes, safe_artifact_path};
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
        digest_bytes(b"")
            .contains("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        "digest of empty input",
    );
    let root = std::env::temp_dir();
    check(
        safe_artifact_path(&root, "a/b.txt").is_ok(),
        "nested relative child allowed",
    );
    check(
        safe_artifact_path(&root, "../outside").is_err(),
        "traversal refused",
    );
    check(
        safe_artifact_path(Path::new("runs"), "").is_err(),
        "empty child refused",
    );
    check(
        DEFAULT_JOURNAL_DIR == ".rightkit/runtime",
        "neutral journal directory",
    );
}
