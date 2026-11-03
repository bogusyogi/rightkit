use super::*;

pub(crate) fn untrusted_content_item(
    (index, message): (usize, &TranscriptMessage),
) -> Option<serde_json::Value> {
    match message {
        TranscriptMessage::User {
            id,
            created_at,
            content,
            trust: Some(trust),
            ..
        } if trust.wraps_untrusted_content() => Some(serde_json::json!({
            "index": index,
            "messageId": id,
            "messageKind": "user",
            "createdAt": created_at,
            "trust": trust,
            "policyAction": untrusted_policy_action(trust, content),
            "preview": summarize_text(content, 220),
        })),
        TranscriptMessage::Tool {
            id,
            created_at,
            tool_call_id,
            summary,
            trust,
            is_error,
            ..
        } => {
            let fallback_trust;
            let trust = match trust.as_ref() {
                Some(trust) if trust.wraps_untrusted_content() => trust,
                Some(_) => return None,
                None => {
                    fallback_trust = trust_envelope(
                        ContentTrust::ToolUntrusted,
                        format!("tool:{tool_call_id}:missing-trust"),
                        summary,
                    );
                    &fallback_trust
                }
            };
            Some(serde_json::json!({
                "index": index,
                "messageId": id,
                "messageKind": "tool",
                "createdAt": created_at,
                "toolCallId": tool_call_id,
                "isError": is_error,
                "trust": trust,
                "policyAction": untrusted_policy_action(trust, summary),
                "preview": summarize_text(summary, 220),
            }))
        }
        _ => None,
    }
}
