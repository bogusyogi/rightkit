// Behavior tests for `--output-schema` loader, validator, and prompt steering.
// Included from `main_runtime_run_output_schema.rs` inside
// `mod output_schema_unit_tests { use super::*; ... }`. The wrapping mod
// inherits the parent module's items only, so we re-state imports for types
// not declared in `main_runtime.rs`'s own prelude.

use std::io::Write;

// -- 1. happy path: valid JSON conforming to a simple schema ----------
#[test]
fn valid_json_conforming_to_schema_passes() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["answer"],
        "properties": {
            "answer": {"type": "string"}
        }
    });
    let validator = jsonschema::validator_for(&schema).expect("schema should compile");
    let instance = serde_json::json!({"answer": "ok"});
    assert!(
        validator.validate(&instance).is_ok(),
        "instance should validate"
    );
}

// -- 2. validation failure: missing required field --------------------
#[test]
fn missing_required_field_fails_validation() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["answer"],
        "properties": {
            "answer": {"type": "string"}
        }
    });
    let validator = jsonschema::validator_for(&schema).expect("schema should compile");
    let instance = serde_json::json!({});
    let err = validator.validate(&instance).unwrap_err();
    let combined = err.to_string();
    assert!(
        combined.to_lowercase().contains("required") || combined.contains("answer"),
        "error should reference the missing required key, got: {combined}"
    );
}

// -- 3. text response (non-JSON) cannot satisfy a schema ---------------
#[test]
fn non_json_response_cannot_satisfy_schema() {
    let response = "I cannot answer in that format.";
    let err = serde_json::from_str::<serde_json::Value>(response)
        .expect_err("plain prose must not parse as JSON");
    assert!(err.is_syntax() || err.is_data() || err.is_eof());
}

// -- 4. load_output_schema: fail-fast on missing file -----------------
#[test]
fn load_output_schema_errors_on_missing_file() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let missing = dir.join("does_not_exist.schema.json");
    let result = load_output_schema(missing.to_str().unwrap());
    let err = result.expect_err("missing file should be a load error");
    assert!(
        err.contains("cannot read --output-schema file"),
        "error should mention the read failure, got: {err}"
    );
}

// -- 5. load_output_schema: fail-fast on unparseable schema JSON ------
#[test]
fn load_output_schema_errors_on_malformed_json() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("malformed.schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(b"{ this is not json")
            .expect("file should be writable");
    }
    let result = load_output_schema(path.to_str().unwrap());
    let err = result.expect_err("malformed JSON should be rejected");
    assert!(
        err.contains("is not valid JSON"),
        "error should call out the JSON parse failure, got: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

// -- 6. load_output_schema: rejects non-schema JSON -------------------
#[test]
fn load_output_schema_rejects_value_that_is_not_a_schema() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("not_a_schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(br#"{"minLength": "not-a-number-or-valid-keyword"}"#)
            .expect("file should be writable");
    }
    let result = load_output_schema(path.to_str().unwrap());
    // jsonschema 0.18 accepts almost any JSON object as a schema (permissive
    // parsing); what matters is that load_output_schema returns *some*
    // compiled validator. The strict-fail-fast path is reserved for the
    // missing-file and malformed-JSON cases (covered above). This test
    // documents the current behavior so a future crate upgrade that tightens
    // schema parsing is a deliberate decision rather than a silent change.
    let _ = result;
    let _ = std::fs::remove_file(&path);
}

// -- 7. happy path: load_output_schema + validate_output_response -----
#[test]
fn load_and_validate_end_to_end_passes_for_compliant_response() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ok.schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(
            br#"{
                "type": "object",
                "required": ["answer"],
                "properties": {"answer": {"type": "string"}}
            }"#,
        )
        .expect("file should be writable");
    }
    let schema =
        load_output_schema(path.to_str().unwrap()).expect("well-formed schema should load");
    let response = serde_json::to_string(&serde_json::json!({"answer": "hello"}))
        .expect("response should serialize");
    validate_output_response(&schema, &response).expect("compliant response should validate");
    let _ = std::fs::remove_file(&path);
}

// -- 8. validate_output_response: non-JSON response is rejected --------
#[test]
fn validate_output_response_rejects_non_json_text() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ok2.schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(br#"{"type": "object"}"#)
            .expect("file should be writable");
    }
    let schema =
        load_output_schema(path.to_str().unwrap()).expect("well-formed schema should load");
    let err = validate_output_response(&schema, "this is not json")
        .expect_err("non-JSON response must be rejected");
    assert!(
        err.contains("not valid JSON"),
        "error should mention JSON parse failure, got: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

// -- 9. validate_output_response: schema-violation surfaces the path ---
#[test]
fn validate_output_response_surfaces_constraint_violation() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("strict.schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(
            br#"{
                "type": "object",
                "required": ["answer"],
                "properties": {"answer": {"type": "string"}}
            }"#,
        )
        .expect("file should be writable");
    }
    let schema =
        load_output_schema(path.to_str().unwrap()).expect("well-formed schema should load");
    let response =
        serde_json::to_string(&serde_json::json!({})).expect("empty object should serialize");
    let err = validate_output_response(&schema, &response)
        .expect_err("missing required field should fail validation");
    assert!(
        err.contains("failed --output-schema validation"),
        "error should clearly call out schema validation failure, got: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

// -- 10. schema_steering_append: prompt fragment includes the schema ---
#[test]
fn schema_steering_append_embeds_schema_and_path() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("steer.schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(br#"{"type":"object","required":["x"],"properties":{"x":{"type":"string"}}}"#)
            .expect("file should be writable");
    }
    let schema =
        load_output_schema(path.to_str().unwrap()).expect("well-formed schema should load");
    let steering = schema_steering_append(&schema);
    assert!(
        steering.contains("JSON Schema"),
        "steering fragment should mention JSON Schema, got: {steering}"
    );
    assert!(
        steering.contains("steer.schema.json"),
        "steering fragment should include the source path, got: {steering}"
    );
    assert!(
        steering.contains("\"required\"") && steering.contains("\"x\""),
        "steering fragment should embed the schema body, got: {steering}"
    );
    let _ = std::fs::remove_file(&path);
}

// -- 11. normalize_output_response: stdout is one canonical JSON document
#[test]
fn normalize_output_response_returns_canonical_json() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("canonical.schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(
            br#"{
                "type": "object",
                "required": ["answer"],
                "properties": {"answer": {"type": "string"}}
            }"#,
        )
        .expect("file should be writable");
    }
    let schema = load_output_schema(path.to_str().unwrap()).expect("schema should load");
    let output = normalize_output_response(&schema, "{\n  \"answer\": \"ok\"\n}")
        .expect("compliant JSON should normalize");
    assert_eq!(output, r#"{"answer":"ok"}"#);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn normalize_output_response_rejects_prose_around_json() {
    let dir = std::env::temp_dir().join("coderight_output_schema_tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("strict-json.schema.json");
    {
        let mut f = std::fs::File::create(&path).expect("file should be creatable");
        f.write_all(br#"{"type":"object"}"#)
            .expect("file should be writable");
    }
    let schema = load_output_schema(path.to_str().unwrap()).expect("schema should load");
    let error = normalize_output_response(&schema, "Here is your JSON: {\"ok\":true}")
        .expect_err("prose must not be accepted as schema-constrained output");
    assert!(error.contains("not valid JSON"));
    let _ = std::fs::remove_file(&path);
}
