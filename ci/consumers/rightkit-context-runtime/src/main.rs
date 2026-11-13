//! Registry consumer for rightkit-context-runtime 0.1.2: builds against the published API only.
//! It does not start the resident or open any store.
use rightkit_context_runtime::{digest, release_identity, PRODUCT_DIR_NAME};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        release_identity::release_generation().starts_with("sha256:"),
        "a release generation is reported",
    );
    println!(
        "ok: release identity source is {}",
        release_identity::source_kind()
    );
    check(
        !PRODUCT_DIR_NAME.is_empty(),
        "the on-disk product folder name is published",
    );
    check(
        digest::digest_str("x") == digest::digest_bytes(b"x"),
        "string and byte digests agree",
    );
    println!("rightkit-context-runtime consumer: all checks passed");
}
