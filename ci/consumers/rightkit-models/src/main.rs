// rightkit-models 0.5.2 consumed by exact registry pin, outside the workspace.
//
// Proves sha256_file, the lowercase hex digest a model install checks an artifact against: the
// FIPS 180-4 vector for "abc" is returned exactly, changed bytes give a different digest, and a
// missing file is refused with an error. Public API only.
// Does not prove: downloads, installs, the store layout, suite entitlements, or probes.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no real user data.
use rightkit_models::hash::sha256_file;
use std::fs;
use std::path::PathBuf;

const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn main() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rk-models-consumer-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create consumer temp dir");
    let file = dir.join("fixture.bin");

    fs::write(&file, b"abc").expect("write fixture");
    assert_eq!(
        sha256_file(&file).expect("digest of fixture"),
        SHA256_ABC,
        "SHA-256 of abc"
    );

    fs::write(&file, b"abd").expect("rewrite fixture");
    assert_ne!(
        sha256_file(&file).expect("digest after change"),
        SHA256_ABC,
        "changed bytes give a different digest"
    );

    assert!(
        sha256_file(&dir.join("missing.bin")).is_err(),
        "missing file is refused"
    );

    fs::remove_dir_all(&dir).expect("remove consumer temp dir");
    println!("OK rightkit-models 0.5.2 (registry consumer)");
}
