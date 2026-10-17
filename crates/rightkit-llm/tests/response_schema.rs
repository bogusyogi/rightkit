//! Request-level response schema (Orthic-Labs/rightkit#584): body shapes for both wires and
//! the local rejections. No network: the body builders are checked directly.

use rightkit_llm::anthropic;
use rightkit_llm::openai::{self, MaxTokensField};
use rightkit_llm::{ChatRequest, LlmError, SchemaConstraint};
use serde_json::json;

fn schema() -> SchemaConstraint {
    SchemaConstraint::new(
        "verdict",
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]}),
    )
}

#[test]
fn openai_sends_json_schema_response_format() {
    let req = ChatRequest::simple("m", "sys", "hi").with_response_schema(schema());
    let body = openai::build_body(&req, false, MaxTokensField::MaxTokens);
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["name"], "verdict");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["required"],
        json!(["ok"])
    );
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
}

#[test]
fn anthropic_forces_a_synthetic_tool_carrying_the_schema() {
    let req = ChatRequest::simple("m", "", "hi").with_response_schema(schema().strict(false));
    let body = anthropic::build_body(&req, false);
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "verdict");
    assert_eq!(tools[0]["input_schema"]["required"], json!(["ok"]));
    assert!(tools[0].get("strict").is_none());
    assert_eq!(
        body["tool_choice"],
        json!({"type": "tool", "name": "verdict"})
    );
}

#[test]
fn schema_output_reads_tool_input_or_json_text() {
    let from_tool = openai::parse_response(&json!({
        "id": "1", "model": "m",
        "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
            "id": "t", "type": "function",
            "function": {"name": "verdict", "arguments": "{\"ok\":true}"}}]},
            "finish_reason": "tool_calls"}]
    }))
    .unwrap();
    assert_eq!(from_tool.schema_output().unwrap(), json!({"ok": true}));

    let from_text = openai::parse_response(&json!({
        "id": "2", "model": "m",
        "choices": [{"message": {"role": "assistant", "content": "{\"ok\": false}"},
            "finish_reason": "stop"}]
    }))
    .unwrap();
    assert_eq!(from_text.schema_output().unwrap(), json!({"ok": false}));

    let not_json = openai::parse_response(&json!({
        "id": "3", "model": "m",
        "choices": [{"message": {"role": "assistant", "content": "sorry"},
            "finish_reason": "stop"}]
    }))
    .unwrap();
    assert!(matches!(
        not_json.schema_output(),
        Err(LlmError::Malformed(_))
    ));
}

#[test]
fn bad_schema_or_mixed_tools_are_invalid_requests() {
    let bad_name = ChatRequest::simple("m", "", "hi").with_response_schema(SchemaConstraint::new(
        "has space",
        json!({"type": "object"}),
    ));
    assert!(matches!(
        bad_name.check_schema(),
        Err(LlmError::InvalidRequest(_))
    ));

    let not_object = ChatRequest::simple("m", "", "hi")
        .with_response_schema(SchemaConstraint::new("ok", json!([1, 2])));
    assert!(matches!(
        not_object.check_schema(),
        Err(LlmError::InvalidRequest(_))
    ));

    let plain = ChatRequest::simple("m", "", "hi").with_response_schema(schema());
    assert!(plain.check_schema().is_ok());
    let mut with_tool = ChatRequest::simple("m", "", "hi").with_response_schema(schema());
    with_tool.force_tool = Some("verdict".into());
    assert!(matches!(
        with_tool.check_schema(),
        Err(LlmError::InvalidRequest(_))
    ));
}
