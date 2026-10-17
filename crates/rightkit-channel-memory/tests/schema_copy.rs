//! The crate embeds its own copy of the schema (`schemas/`) so `cargo package` ships it. The
//! repository file under `contracts/` (also embedded by `rightkit-contracts`) stays the shared
//! source; this test fails when they differ.

use std::path::PathBuf;

#[test]
fn embedded_schema_equals_repository_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let file = "channel-memory.v1.schema.json";
    let copy = std::fs::read(root.join("schemas").join(file)).expect("crate schema copy");
    let source = std::fs::read(root.join("../../contracts").join(file)).expect("contracts/ schema");
    assert!(
        copy == source,
        "schemas/{file} differs from contracts/{file}; copy it over"
    );
}
