// `--output-schema` loader / validator / prompt-steering fragment for
// `coderight run` (FEATURE-MATRIX par sect#214).
//
// Included into the `main_runtime` module via `include!`. Defines a
// `pub(crate)` API consumed by `main_runtime_run_session.rs`:
//   - `OutputSchema`              : compiled validator + cached JSON for steering
//   - `validate_output_response`  : parse final text as JSON, validate
//   - `schema_steering_append`    : system-prompt fragment for active steering
//   - `prepare_output_schema`     : steer `append_system_prompt` + load validator
//   - `validate_run_output`       : post-engine output gate
//
// Draft selection: the existing `coderight-recipes` crate does not
// standardize on a specific JSON Schema draft and does not currently depend
// on a JSON Schema validator crate (verified via ripgrep over the
// workspace). We adopt the `jsonschema` 0.18 crate's default draft
// negotiation (Draft 7 with auto-detection of `$schema`), which is the
// most interoperable baseline for tool-side schema definitions matching
// Codex / Claude Code CLI behavior. Bumping the draft is a single call-site
// change once the recipes crate picks a canonical draft.

#[derive(Debug)]
pub(crate) struct OutputSchema {
    compiled: jsonschema::Validator,
    raw: serde_json::Value,
    source_path: String,
}

impl OutputSchema {
    pub(crate) fn source_path(&self) -> &str {
        &self.source_path
    }
}

fn load_output_schema(path: &str) -> Result<OutputSchema, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| format!("[run] cannot read --output-schema file at {path}: {err}"))?;
    let raw: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|err| format!("[run] --output-schema file at {path} is not valid JSON: {err}"))?;
    let compiled = jsonschema::validator_for(&raw).map_err(|err| {
        format!("[run] --output-schema at {path} is not a valid JSON Schema: {err}")
    })?;
    Ok(OutputSchema {
        compiled,
        raw,
        source_path: path.to_string(),
    })
}

fn validate_output_response(schema: &OutputSchema, response: &str) -> Result<(), String> {
    parse_validated_output(schema, response).map(|_| ())
}

/// Parse & validate one complete assistant response, returning its JSON value.
/// Keeping parsing, validation, & canonical rendering on one path prevents a
/// response that validates only after lossy text manipulation from reaching
/// stdout.
fn parse_validated_output(
    schema: &OutputSchema,
    response: &str,
) -> Result<serde_json::Value, String> {
    let instance: serde_json::Value = serde_json::from_str(response).map_err(|err| {
        format!(
            "[run] final response is not valid JSON and cannot satisfy --output-schema {path}: {err}",
            path = schema.source_path()
        )
    })?;
    // `validate` stops at first error; `iter_errors` reports every violation
    // so callers receive one stable, actionable schema failure.
    let details: Vec<String> = schema
        .compiled
        .iter_errors(&instance)
        .map(|error| error.to_string())
        .collect();
    if !details.is_empty() {
        return Err(format!(
            "[run] final response failed --output-schema validation ({path}):\n{}",
            details.join("\n"),
            path = schema.source_path()
        ));
    }
    Ok(instance)
}

/// Return one canonical JSON document for stdout after schema validation.
/// Whitespace, indentation, & key order from model output are intentionally
/// not part of this machine contract.
fn normalize_output_response(schema: &OutputSchema, response: &str) -> Result<String, String> {
    let instance = parse_validated_output(schema, response)?;
    serde_json::to_string(&instance)
        .map_err(|error| format!("[run] cannot serialize schema-constrained output: {error}"))
}

/// Extract final assistant text from a daemon protocol event. Only assistant
/// transcript content is eligible for schema validation; tool summaries,
/// reasoning, & control payloads must never be mistaken for final output.
fn assistant_message_text(event: &rightkit_agent_engine_protocol::ProtocolEvent) -> Option<&str> {
    match event {
        rightkit_agent_engine_protocol::ProtocolEvent::AssistantMessageCompleted {
            message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant { content, .. },
            ..
        } => Some(content.as_str()),
        _ => None,
    }
}

fn schema_steering_append(schema: &OutputSchema) -> String {
    let serialized =
        serde_json::to_string_pretty(&schema.raw).unwrap_or_else(|_| schema.raw.to_string());
    format!(
        "You MUST respond with a single JSON document that strictly conforms to the JSON Schema below. \
Output only the JSON document, with no surrounding prose, code fences, or commentary. \
If the request cannot be expressed under the schema, return a JSON value that still satisfies the schema (e.g. a `null` or an empty object for the documented required keys).\n\n\
Schema source: {path}\n\n\
```json\n{schema}\n```",
        path = schema.source_path(),
        schema = serialized,
    )
}

fn prepare_output_schema(
    output_schema: Option<String>,
    append_system_prompt: Option<String>,
) -> Result<(Option<String>, Option<OutputSchema>), (i32, String)> {
    let Some(path) = output_schema else {
        return Ok((append_system_prompt, None));
    };
    let compiled = match load_output_schema(&path) {
        Ok(schema) => schema,
        Err(message) => return Err((RunExitCode::Usage.as_i32(), message)),
    };
    let fragment = schema_steering_append(&compiled);
    let steered = match append_system_prompt {
        Some(existing) if !existing.trim().is_empty() => Some(format!("{existing}\n\n{fragment}")),
        _ => Some(fragment),
    };
    Ok((steered, Some(compiled)))
}

// FM-109: keep schema-constrained output contract — this is the live
// mechanism site (steer the model via `schema_steering_append`, then gate
// the final response through `validate_output_response`/`iter_errors`,
// rejecting non-conformant output with `RunExitCode::SchemaViolation`
// rather than passing it through silently). Covered by
// `output_schema_unit_tests` below.
fn validate_run_output<E>(
    result: &Result<String, E>,
    schema: Option<&OutputSchema>,
) -> Result<(), (i32, String)> {
    let Some(schema) = schema else {
        return Ok(());
    };
    let Ok(text) = result else {
        return Ok(());
    };
    validate_output_response(schema, text)
        .map_err(|message| (RunExitCode::SchemaViolation.as_i32(), message))
}

#[cfg(test)]
mod output_schema_unit_tests {
    use super::*;
    include!("main_runtime_run_output_schema_tests.rs");
}
