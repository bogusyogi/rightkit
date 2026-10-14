use std::{fs, path::Path};

#[test]
fn package_contains_both_declared_license_texts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();

    assert!(manifest.contains("license = \"MIT OR Apache-2.0\""));
    assert!(manifest.contains("\"LICENSE-MIT\""));
    assert!(manifest.contains("\"LICENSE-APACHE\""));
    assert!(fs::read_to_string(root.join("LICENSE-MIT"))
        .unwrap()
        .contains("MIT License"));
    assert!(fs::read_to_string(root.join("LICENSE-APACHE"))
        .unwrap()
        .contains("Apache License"));
}
