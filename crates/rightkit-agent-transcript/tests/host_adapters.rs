use rightkit_agent_transcript::adapters::{claude_events, codex_events};
use serde_json::json;

#[test]
fn named_hosts_filter_non_user_origins() {
    let claude_sdk = json!({
        "type":"user", "promptSource":"sdk", "entrypoint":"sdk-cli",
        "message":{"role":"user","content":"injected"}
    });
    assert!(claude_events(&claude_sdk).is_empty());
    let claude_human = json!({
        "type":"user", "origin":{"kind":"human"},
        "message":{"role":"user","content":"typed"}
    });
    assert_eq!(claude_events(&claude_human)[0].text, "typed");

    let codex_control = json!({"type":"response_item","payload":{
        "type":"message","role":"user","content":[{"type":"input_text","text":
        "<heartbeat>injected</heartbeat>"}]}});
    assert!(codex_events(&codex_control).is_empty());
}
