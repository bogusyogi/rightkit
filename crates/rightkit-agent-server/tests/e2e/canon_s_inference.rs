//! S:H13 / S:M6 inference credentials and selectable catalog runtime surface.

use crate::support::e2e_api::*;
use serde_json::Value;

#[test]
fn inference_catalog_is_a_typed_runtime_projection() {
    let rig = Rig::start(vec![]);
    let (status, catalog) = rig.get("/inference/catalog");
    assert_eq!(status, 200, "inference catalog: {catalog}");
    let groups = catalog["groups"].as_array().expect("catalog groups");
    for group in groups {
        assert!(group["authority"].is_string(), "{group}");
        assert!(group["connectionId"].is_string(), "{group}");
        assert!(group["state"].is_string(), "{group}");
        assert!(group["models"].is_array(), "{group}");
    }
}

#[test]
fn provider_key_status_exposes_source_without_credential_bytes() {
    let rig = Rig::start(vec![]);
    let (status, result): (u16, Value) = rig.get("/provider-keys/openai");
    assert_eq!(status, 200, "provider credential status: {result}");
    let serialized = result.to_string();
    assert!(
        !serialized.to_ascii_lowercase().contains("api_key"),
        "credential response leaked key field: {serialized}"
    );
    assert!(
        !serialized.to_ascii_lowercase().contains("apikey"),
        "credential response leaked key field: {serialized}"
    );
}
