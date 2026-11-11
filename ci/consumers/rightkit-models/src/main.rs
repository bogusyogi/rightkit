// rightkit-models 0.7.0 consumed by exact registry pin, outside the workspace.
//
// Proves:
// - sha256_file, the lowercase hex digest a model install checks an artifact against: the
//   FIPS 180-4 vector for "abc" is returned exactly, changed bytes give a different digest, and a
//   missing file is refused with an error;
// - one store: a small two-file model installed through `Store` (adopted from a flat folder,
//   every file pinned) lands in the suite layout, `<root>/<id>/<files>` plus the suite
//   `manifest.json` whose version is the revision key, with no `state.json` or `revisions/`; the
//   suite reader verifies that entry at full depth; `Store::resolve` serves the entry itself;
// - an entry left by the pre-0.7 revisioned store (`state.json` + `revisions/<key>/`) is still
//   found and is migrated to the suite layout on first resolve;
// - uninstall removes the entry.
// Public API only.
// Does not prove: downloads, archive extraction, suite entitlements, or probes.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no real user data.
use rightkit_models::hash::sha256_file;
use rightkit_models::suite::manifest::{read_manifest, verify_dir};
use rightkit_models::suite::VerifyMode as SuiteVerify;
use rightkit_models::{FileSpec, InstallOptions, ModelSpec, RevisionSpec, Store, VerifyMode};
use std::fs;
use std::path::{Path, PathBuf};

const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn pinned(path: &str, file: &Path) -> FileSpec {
    FileSpec {
        path: path.into(),
        url: format!("https://models.example.invalid/demo/{path}"),
        bytes: Some(fs::metadata(file).expect("stat fixture").len()),
        sha256: Some(sha256_file(file).expect("digest fixture")),
        ..Default::default()
    }
}

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

    // One store: install through `Store`, find a suite entry.
    let flat = dir.join("flat");
    fs::create_dir_all(&flat).expect("create flat folder");
    fs::write(flat.join("model.bin"), vec![7u8; 4096]).expect("write model");
    fs::write(flat.join("tokenizer.json"), b"{\"v\":1}").expect("write tokenizer");
    let revision = RevisionSpec {
        revision: "1".into(),
        quant: Some("q4".into()),
        files: vec![
            pinned("model.bin", &flat.join("model.bin")),
            pinned("tokenizer.json", &flat.join("tokenizer.json")),
        ],
        ..Default::default()
    };
    let spec = ModelSpec::new("demo-model", vec![revision]);
    let root = dir.join("models");
    let store = Store::new(&root, "rk-consumer");
    let out = store
        .adopt_flat_dir(&spec, &InstallOptions::default(), &flat)
        .expect("install through Store");
    let entry = root.join("demo-model");
    assert_eq!(out.dir, entry, "the install is the suite entry <root>/<id>");
    assert_eq!(out.key, "1@q4");
    assert!(entry.join("model.bin").is_file() && entry.join("tokenizer.json").is_file());
    assert!(
        !entry.join("state.json").exists() && !entry.join("revisions").exists(),
        "no revisioned layout is written"
    );
    let manifest = read_manifest(&entry)
        .expect("read suite manifest")
        .expect("suite manifest present");
    assert_eq!(manifest.id, "demo-model");
    assert_eq!(
        manifest.version, "1@q4",
        "manifest version is the revision key"
    );
    assert_eq!(manifest.files.len(), 2);
    assert!(
        verify_dir(&entry, &manifest, SuiteVerify::Full).is_ready(),
        "the suite reader verifies the Store entry"
    );
    let resolved = store.resolve("demo-model").expect("resolve the entry");
    assert_eq!(
        (resolved.key.as_str(), resolved.dir.as_path()),
        ("1@q4", entry.as_path())
    );
    assert!(store.verify("demo-model", "1@q4", VerifyMode::Full));
    assert!(
        store.rollback("demo-model", None).is_err(),
        "one revision per model: nothing to roll back to"
    );

    // An entry the pre-0.7 store wrote is found and migrated in place.
    let old = root.join("old-model");
    fs::create_dir_all(old.join("revisions").join("2")).expect("create old revision");
    fs::write(old.join("revisions").join("2").join("weights.bin"), b"abc").expect("old file");
    let state = format!(
        "{{\"id\":\"old-model\",\"active\":\"2\",\"history\":[],\"revisions\":{{\"2\":{{\"revision\":\"2\",\"quant\":null,\"installed_at_unix\":1,\"files\":[{{\"path\":\"weights.bin\",\"bytes\":3,\"sha256\":\"{SHA256_ABC}\"}}]}}}}}}"
    );
    fs::write(old.join("state.json"), state).expect("write old state");
    assert_eq!(
        store
            .state("old-model")
            .expect("old state")
            .active
            .as_deref(),
        Some("2"),
        "the old layout is read"
    );
    let migrated = store
        .resolve("old-model")
        .expect("old entry still resolves");
    assert_eq!(migrated.dir, old, "migrated onto the suite entry");
    assert!(old.join("weights.bin").is_file() && !old.join("state.json").exists());
    let old_manifest = read_manifest(&old)
        .expect("read migrated manifest")
        .expect("migrated manifest present");
    assert_eq!(old_manifest.version, "2");
    assert!(verify_dir(&old, &old_manifest, SuiteVerify::Full).is_ready());

    store.uninstall("demo-model").expect("uninstall");
    assert!(!entry.exists(), "uninstall removes the entry");

    fs::remove_dir_all(&dir).expect("remove consumer temp dir");
    println!("OK rightkit-models 0.7.0 (registry consumer)");
}
