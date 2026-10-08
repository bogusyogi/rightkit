use std::sync::OnceLock;

use jsonschema::{Draft, Validator};
use serde_json::Value;

use crate::MemoryError;

/// The channel-memory.v1 schema, embedded so every consumer validates the same bytes.
pub const SCHEMA_SOURCE: &str =
    include_str!("../../../contracts-pending/channel-memory/channel-memory.v1.schema.json");

static VALIDATOR: OnceLock<Validator> = OnceLock::new();

fn compiled() -> &'static Validator {
    VALIDATOR.get_or_init(|| {
        let schema: Value =
            serde_json::from_str(SCHEMA_SOURCE).expect("embedded channel-memory schema is JSON");
        jsonschema::options()
            .with_draft(Draft::Draft202012)
            .build(&schema)
            .expect("embedded channel-memory schema compiles")
    })
}

pub(crate) fn check(document: &Value) -> Result<(), MemoryError> {
    let errors: Vec<String> = compiled()
        .iter_errors(document)
        .map(|error| {
            let path = error.instance_path().to_string();
            let path = if path.is_empty() {
                "(root)".to_string()
            } else {
                path
            };
            format!("{path}: {error}")
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(MemoryError::Schema { errors })
    }
}
