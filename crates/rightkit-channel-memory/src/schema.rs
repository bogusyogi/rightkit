use std::sync::OnceLock;

use jsonschema::{Draft, Validator};
use serde_json::{json, Value};

use crate::MemoryError;

/// The channel-memory.v1 schema, embedded so every consumer validates the same bytes.
pub const SCHEMA_SOURCE: &str = include_str!("../schemas/channel-memory.v1.schema.json");

static VALIDATOR: OnceLock<Validator> = OnceLock::new();
static PROOF_RECORD: OnceLock<Validator> = OnceLock::new();
static SCOPE_RECORD: OnceLock<Validator> = OnceLock::new();

fn build(schema: &Value) -> Validator {
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(schema)
        .expect("embedded channel-memory schema compiles")
}

fn source() -> Value {
    serde_json::from_str(SCHEMA_SOURCE).expect("embedded channel-memory schema is JSON")
}

fn compiled() -> &'static Validator {
    VALIDATOR.get_or_init(|| build(&source()))
}

/// A validator for one `$defs` entry. It keeps the same definitions, so a
/// record is checked against the same bytes as a whole document.
fn def_validator(name: &str) -> Validator {
    let schema = source();
    let wrapper = json!({
        "$defs": schema["$defs"].clone(),
        "$ref": format!("#/$defs/{name}"),
    });
    build(&wrapper)
}

fn report(validator: &Validator, document: &Value) -> Result<(), MemoryError> {
    let errors: Vec<String> = validator
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

pub(crate) fn check(document: &Value) -> Result<(), MemoryError> {
    report(compiled(), document)
}

/// One proof record against the `proof` definition.
pub(crate) fn check_proof_schema(document: &Value) -> Result<(), MemoryError> {
    report(
        PROOF_RECORD.get_or_init(|| def_validator("proof")),
        document,
    )
}

/// One scope against the `scope` definition.
pub(crate) fn check_scope(document: &Value) -> Result<(), MemoryError> {
    report(
        SCOPE_RECORD.get_or_init(|| def_validator("scope")),
        document,
    )
}
