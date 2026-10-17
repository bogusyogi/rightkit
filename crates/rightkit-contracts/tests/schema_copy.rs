//! The crate embeds its own copy of each schema (`schemas/`) so `cargo package` ships them. The
//! repository-root `contracts/` directory stays the shared source; this test fails when the two differ.

use std::path::PathBuf;

#[test]
fn embedded_schemas_equal_repository_contracts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for name in [
        "content-brief",
        "delivery-envelope",
        "record-session",
        "record-event",
        "demo-plan",
        "daemon-api",
        "channel-memory",
    ] {
        let file = format!("{name}.v1.schema.json");
        let copy = std::fs::read(root.join("schemas").join(&file)).expect("crate schema copy");
        let source =
            std::fs::read(root.join("../../contracts").join(&file)).expect("contracts/ schema");
        assert!(
            copy == source,
            "schemas/{file} differs from contracts/{file}; copy it over"
        );
    }
}
