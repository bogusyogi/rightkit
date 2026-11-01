use crate::types::Recipe;
use serde_json::Value;

/// A compiled `response_schema` for a recipe, ready to validate run output.
///
/// Draft selection mirrors `host app` bin's `--output-schema` gate
/// (`host-app/src/main_runtime_run_output_schema.rs`): the `jsonschema`
/// 0.18 crate's default draft negotiation (Draft 7 with `$schema`
/// auto-detection). This is the same dependency/version, reused here as a
/// library function so the `recipes` crate does not need its own runtime
/// caller to duplicate compilation/validation logic.
#[derive(Debug)]
pub struct CompiledResponseSchema {
    compiled: jsonschema::Validator,
    raw: Value,
}

impl CompiledResponseSchema {
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseSchemaError {
    pub message: String,
}

impl std::fmt::Display for ResponseSchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ResponseSchemaError {}

/// Compile a recipe's `response_schema` (if present) into a validator.
/// Returns `Ok(None)` when the recipe declares no response schema.
pub fn compile_response_schema(
    recipe: &Recipe,
) -> Result<Option<CompiledResponseSchema>, ResponseSchemaError> {
    let Some(raw) = recipe.response_schema.clone() else {
        return Ok(None);
    };
    if !raw.is_object() {
        return Err(ResponseSchemaError {
            message: "recipe response_schema must be a JSON object".to_string(),
        });
    }
    // jsonschema 0.50 renamed `JSONSchema` to `Validator` and replaced the
    // `compile` associated function with the free `validator_for`.
    let compiled = jsonschema::validator_for(&raw).map_err(|err| ResponseSchemaError {
        message: format!("recipe response_schema is not a valid JSON Schema: {err}"),
    })?;
    Ok(Some(CompiledResponseSchema { compiled, raw }))
}

/// Validate a recipe run's completion output against the recipe's compiled
/// `response_schema`. `output` is the raw text produced by the run; it must
/// parse as JSON and conform to the schema.
pub fn validate_response(
    schema: &CompiledResponseSchema,
    output: &str,
) -> Result<(), ResponseSchemaError> {
    let instance: Value = serde_json::from_str(output).map_err(|err| ResponseSchemaError {
        message: format!(
            "recipe run output is not valid JSON and cannot satisfy response_schema: {err}"
        ),
    })?;
    // `validate` now short-circuits on the first error; `iter_errors` is the
    // replacement for the old iterator-returning `validate`, so every schema
    // violation is still reported rather than only the first.
    let details: Vec<String> = schema
        .compiled
        .iter_errors(&instance)
        .map(|e| e.to_string())
        .collect();
    if !details.is_empty() {
        return Err(ResponseSchemaError {
            message: format!(
                "recipe run output failed response_schema validation:\n{}",
                details.join("\n")
            ),
        });
    }
    Ok(())
}

/// Convenience one-shot: compile the recipe's `response_schema` (if any) and
/// validate `output` against it. Returns `Ok(())` when the recipe has no
/// `response_schema` (nothing to enforce).
pub fn validate_recipe_run_output(
    recipe: &Recipe,
    output: &str,
) -> Result<(), ResponseSchemaError> {
    match compile_response_schema(recipe)? {
        Some(schema) => validate_response(&schema, output),
        None => Ok(()),
    }
}
