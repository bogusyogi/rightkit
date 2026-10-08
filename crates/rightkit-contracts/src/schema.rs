use std::sync::OnceLock;

use jsonschema::{Draft, Validator};
use serde_json::Value;

use crate::{Contract, ContractError};

static VALIDATORS: [OnceLock<Validator>; 6] = [const { OnceLock::new() }; 6];

/// Compiles the embedded schema once. The schemas are part of this crate's
/// build inputs, and the test suite compiles every one, so a failure here is
/// a build defect, not a runtime input error.
fn compiled(contract: Contract) -> &'static Validator {
    VALIDATORS[contract.index()].get_or_init(|| {
        let schema: Value =
            serde_json::from_str(contract.schema_source()).unwrap_or_else(|error| {
                panic!("embedded schema {} is not JSON: {error}", contract.name())
            });
        jsonschema::options()
            .with_draft(Draft::Draft202012)
            .build(&schema)
            .unwrap_or_else(|error| {
                panic!(
                    "embedded schema {} does not compile: {error}",
                    contract.name()
                )
            })
    })
}

pub(crate) fn check(contract: Contract, document: &Value) -> Result<(), ContractError> {
    let errors: Vec<String> = compiled(contract)
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
        Err(ContractError::Schema { contract, errors })
    }
}
