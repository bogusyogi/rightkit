// rightkit-code-audit-qualification 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the pure digest and path rules of the assurance record:
// - sha256_digest returns the sha256:<64 lower-hex> form and is_digest accepts exactly that form;
// - is_digest refuses a bare hex string and an upper-case digest;
// - safe_relative_path accepts a plain relative path and refuses absolute, parent-escaping,
//   backslash and drive-style paths.
// No record is read or validated from disk, no keys are loaded, no attestation is verified.
// No I/O, no network, no process spawn. Exits 1 on the first failed check.
use rightkit_code_audit_qualification::assurance::{is_digest, safe_relative_path, sha256_digest};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let digest = sha256_digest(b"abc");
    check(
        digest == "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "sha256 of abc is the published vector in the sha256: form",
    );
    check(is_digest(&digest), "a sha256: digest is accepted");
    check(
        !is_digest(&digest["sha256:".len()..]),
        "a bare hex digest without the prefix is refused",
    );
    check(
        !is_digest("sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"),
        "an upper-case digest is refused",
    );
    check(
        safe_relative_path("qualification/provider-accuracy/index.json"),
        "a plain relative path is safe",
    );
    check(!safe_relative_path("/etc/passwd"), "an absolute path is refused");
    check(!safe_relative_path("a/../b"), "a parent-escaping path is refused");
    check(!safe_relative_path("a\\b"), "a backslash path is refused");
    check(!safe_relative_path("C:x"), "a drive-style path is refused");
    println!("OK rightkit-code-audit-qualification 0.1.0 (registry consumer)");
}
