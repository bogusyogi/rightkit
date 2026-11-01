//! Registry consumer for rightkit-context-protocol 0.1.0: builds against the published API only.
use rightkit_context_protocol::{canonicalize, digest_str, operations::operations_slice, SHAPES};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        !SHAPES.is_empty() && SHAPES.iter().all(|s| !s.schema.is_empty() && !s.fixture.is_empty()),
        "every contract shape embeds its schema and golden fixture",
    );
    check(!operations_slice().is_empty(), "the operation index is populated");
    let value = serde_json::json!({"b": 1, "a": [true, null]});
    check(canonicalize(&value) == r#"{"a":[true,null],"b":1}"#, "canonical JSON sorts keys and drops whitespace");
    check(digest_str("x").starts_with("sha256:") && digest_str("x").len() == 71, "digests are sha256-prefixed hex");
    println!("rightkit-context-protocol consumer: all checks passed");
}
