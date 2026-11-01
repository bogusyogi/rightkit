#![cfg(all(windows, feature = "agent_providers-os-keyring"))]

use rightkit_llm::agent_providers::{
    credential_broker::{CredentialBroker, ProviderCredentialConfig},
    subscription_auth::{open_subscription_store, subscription_leg, AuthSignal},
    OpenAiCompatClient, OsKeyStore, ProviderRequestMetadata, OPENAI_CODEX_BASE_URL,
};
use std::sync::Arc;

#[tokio::test]
#[ignore = "manual live probe; uses existing OpenAI subscription login"]
async fn native_openai_gpt_61_sol_uses_stored_subscription_auth() {
    let store = open_subscription_store().expect("open the subscription credential store");
    let leg = subscription_leg(
        "openai",
        store,
        std::env::temp_dir().join("rightkit-agent-probe-oauth-locks"),
    )
    .expect("openai subscription leg");
    assert!(leg.eligibility.enabled_in_core());
    let mut broker = CredentialBroker::new(Arc::new(OsKeyStore), "rightkit-agent");
    broker.register(ProviderCredentialConfig::new("openai").with_subscription(leg));
    let resolved_auth = broker
        .resolve("openai", AuthSignal::default())
        .await
        .unwrap_or_else(|_| panic!("resolve stored OpenAI OAuth failed"));
    let auth = resolved_auth
        .unwrap_or_else(|| panic!("OpenAI subscription login must exist"))
        .auth;
    let token = auth
        .api_key
        .as_deref()
        .expect("resolved OpenAI OAuth must supply an access token");
    let auth_header = ("Authorization".to_string(), format!("Bearer {token}"));
    let public_headers = auth.headers.unwrap_or_default();
    let base_url = auth
        .base_url
        .unwrap_or_else(|| OPENAI_CODEX_BASE_URL.to_string());

    let addresses: Vec<_> = tokio::net::lookup_host(("chatgpt.com", 443))
        .await
        .expect("resolve ChatGPT endpoint")
        .collect();
    assert!(!addresses.is_empty(), "ChatGPT endpoint must resolve");
    let client = OpenAiCompatClient::new_direct(
        "gpt-6.1-sol",
        base_url,
        Some(auth_header),
        public_headers,
        &addresses,
    );
    let prompt = serde_json::json!({
        "role":"user",
        "content":"Call ping_status once with status ready. After receiving its result, reply with only OPENAI_CONTINUATION_READY."
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        client.complete_with_tools_metadata(
            vec![prompt.clone()],
            vec![serde_json::json!({
                "type":"function",
                "function":{
                    "name":"ping_status",
                    "description":"Report the requested probe status.",
                    "parameters":{
                        "type":"object",
                        "properties":{"status":{"type":"string","enum":["ready"]}},
                        "required":["status"],
                        "additionalProperties":false
                    }
                }
            })],
            &ProviderRequestMetadata::default(),
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("live OpenAI probe timed out"));
    let turn = result.expect("live OpenAI probe request failed");
    let call = turn
        .tool_calls
        .iter()
        .find(|call| call.name == "ping_status")
        .expect("OpenAI probe must return the requested function call");
    let arguments: serde_json::Value =
        serde_json::from_str(&call.arguments_json).expect("function arguments must be JSON");
    assert_eq!(arguments["status"], "ready");

    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        client.complete_with_tools_metadata(
            vec![
                prompt,
                serde_json::json!({
                    "role":"assistant","content":null,
                    "tool_calls":[{"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments_json}}]
                }),
                serde_json::json!({"role":"tool","tool_call_id":call.id,"content":"status ready"}),
            ],
            Vec::new(),
            &ProviderRequestMetadata::default(),
        ),
    )
    .await
    .expect("live OpenAI continuation timed out")
    .expect("live OpenAI continuation failed");
    assert!(completed.text.contains("OPENAI_CONTINUATION_READY"));
}
