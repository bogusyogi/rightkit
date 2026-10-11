use rightkit_llm::{anthropic, openai, ChatRequest, ToolSpec};
use serde_json::json;

fn req(strict: bool) -> ChatRequest {
    let mut r = ChatRequest::simple("m", "sys", "hi");
    r.tools
        .push(ToolSpec::new("pick", "Pick one", json!({"type": "object"})).strict(strict));
    r
}

#[test]
fn anthropic_sends_strict_only_when_set() {
    let on = anthropic::build_body(&req(true), false);
    assert_eq!(on["tools"][0]["strict"], json!(true));
    let off = anthropic::build_body(&req(false), false);
    assert!(off["tools"][0].get("strict").is_none());
}

#[test]
fn openai_sends_strict_on_the_function_only_when_set() {
    let on = openai::build_body(&req(true), false, openai::MaxTokensField::MaxTokens);
    assert_eq!(on["tools"][0]["function"]["strict"], json!(true));
    let off = openai::build_body(&req(false), false, openai::MaxTokensField::MaxTokens);
    assert!(off["tools"][0]["function"].get("strict").is_none());
}

#[test]
fn tool_spec_serde_omits_false_and_defaults_missing() {
    let off = serde_json::to_value(ToolSpec::new("a", "b", json!({}))).unwrap();
    assert!(off.get("strict").is_none());
    let back: ToolSpec = serde_json::from_value(off).unwrap();
    assert!(!back.strict);
}
