//! Registry consumer for rightkit-context-provider-sdk 0.1.0: builds against the published API only.
use rightkit_context_provider_sdk::{canonicalize, operations_slice, ProviderRegistry};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(ProviderRegistry::new(Vec::new()).is_ok(), "an empty provider registry is valid");
    check(!operations_slice().is_empty(), "the operation index is re-exported");
    check(canonicalize(&serde_json::json!({"z": 0, "a": 1})) == r#"{"a":1,"z":0}"#, "canonical JSON sorts keys");
    println!("rightkit-context-provider-sdk consumer: all checks passed");
}
