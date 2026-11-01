//! Engine seam for prompt-cache break detection (atom `VG-001`).
//!
//! `prompt_cache_break` holds the donor-ported mechanism; this file feeds it
//! the engine's own request snapshot before every provider call and turns a
//! detected break into a durable `promptCacheBreak` event after the response.

use super::*;
use crate::prompt_cache_break::{Fnv, PromptStateInput, ResponseCacheUsage};

/// The per-turn volatile goal overlay rides the very end of the request and is
/// never part of a cacheable prefix.
const GOAL_REMINDER_ID: &str = "goal-reminder";

fn is_volatile_tail(message: &TranscriptMessage) -> bool {
    matches!(message, TranscriptMessage::System { id, .. } if id == GOAL_REMINDER_ID)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl SessionEngine {
    /// Phase 1: snapshot the provider request about to be sent. `wire` is the
    /// exact provider-visible message list (system tier first), `tool_defs`
    /// the tool block serialized for this request, `routing_key` the
    /// provider-side cache routing key.
    pub(super) fn record_prompt_cache_request(
        &mut self,
        wire: &[TranscriptMessage],
        tool_defs: &[serde_json::Value],
        routing_key: Option<&str>,
        promoted_tools: &std::collections::HashSet<String>,
    ) {
        let visible: Vec<&TranscriptMessage> = wire
            .iter()
            .filter(|message| !is_volatile_tail(message))
            .collect();
        let system_len = visible
            .iter()
            .take_while(|message| matches!(message, TranscriptMessage::System { .. }))
            .count();
        let system: Vec<&str> = visible[..system_len]
            .iter()
            .filter_map(|message| match message {
                TranscriptMessage::System { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        let message_hashes: Vec<u64> = visible[system_len..]
            .iter()
            .map(|message| provider_visible_message_hash(message))
            .collect();

        // Placement signature: the provider marks the end of the stable system
        // tier and the tool block, so those boundaries moving is a placement
        // change even when no byte of content changed.
        let mut placement = Fnv::default();
        placement.write(&(system_len as u64).to_le_bytes());
        placement.write(&[u8::from(!tool_defs.is_empty())]);

        // Routing key + anything else the provider keys the cache on that the
        // engine does not model individually.
        let mut extra = Fnv::default();
        extra.write_field(routing_key.unwrap_or_default().as_bytes());

        let mut promoted: Vec<&str> = promoted_tools.iter().map(String::as_str).collect();
        promoted.sort_unstable();
        let mut promotion = Fnv::default();
        for name in promoted {
            promotion.write_field(name.as_bytes());
        }

        let model = self
            .llm
            .active_route_label()
            .or_else(|| {
                self.repair_attribution
                    .as_ref()
                    .and_then(|(_, model)| model.clone())
            })
            .unwrap_or_default();
        let cache_eligible = provider_cache_eligible(&model);

        let input = PromptStateInput::new(model)
            .with_system(&system)
            .with_tool_defs(tool_defs)
            .with_message_hashes(message_hashes)
            .with_effort(self.effort.clone().unwrap_or_default())
            .with_extra_body_hash(extra.finish())
            .with_cache_control_hash(placement.finish())
            .with_compaction_epoch(self.compaction_epoch)
            .with_tool_promotion_hash(promotion.finish())
            .with_cache_coverage(
                cache_eligible,
                provider_cache_coverage_tokens(&visible, tool_defs),
            );
        self.prompt_cache_break.record_request(input, now_ms());
    }

    /// Phase 2: classify the response's cache usage and, on a break, publish
    /// the durable `promptCacheBreak` event and a tracing line.
    pub(super) fn check_prompt_cache_break(
        &mut self,
        sid: &str,
        prompt_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    ) {
        let Some(found) = self.prompt_cache_break.check_response(ResponseCacheUsage {
            input_tokens: u64::from(prompt_tokens),
            cache_read_tokens: u64::from(cache_read_tokens),
            cache_creation_tokens: u64::from(cache_creation_tokens),
        }) else {
            return;
        };
        let cost = self
            .usage
            .estimate_cache_miss_cost_usd(found.tokens_lost, found.cache_creation_tokens);
        if found.expected {
            tracing::info!(
                session_id = %sid,
                reason = found.reason.as_str(),
                tokens_lost = found.tokens_lost,
                turn = found.turn,
                detail = %found.detail,
                "[PROMPT CACHE BREAK] expected"
            );
        } else {
            tracing::warn!(
                session_id = %sid,
                reason = found.reason.as_str(),
                tokens_lost = found.tokens_lost,
                estimated_cost_lost_usd = cost,
                turn = found.turn,
                detail = %found.detail,
                "[PROMPT CACHE BREAK] unexpected: provider prompt cache was not reused"
            );
        }
        let session_id = sid.to_string();
        self.emit(|seq| ProtocolEvent::PromptCacheBreak {
            session_id,
            seq,
            created_at: now_iso(),
            reason: found.reason.as_str().to_string(),
            expected: found.expected,
            tokens_lost: i64::try_from(found.tokens_lost).unwrap_or(i64::MAX),
            estimated_cost_lost: cost,
            turn: i64::try_from(found.turn).unwrap_or(i64::MAX),
            detail: found.detail.clone(),
            previous_cache_read_tokens: i64::try_from(found.previous_cache_read_tokens)
                .unwrap_or(i64::MAX),
            cache_read_tokens: i64::try_from(found.cache_read_tokens).unwrap_or(i64::MAX),
            cache_creation_tokens: i64::try_from(found.cache_creation_tokens).unwrap_or(i64::MAX),
        });
    }
}

fn provider_visible_message_hash(message: &TranscriptMessage) -> u64 {
    let mut hash = Fnv::default();
    match message {
        TranscriptMessage::System { content, .. } => {
            hash.write_field(b"system").write_field(content.as_bytes());
        }
        TranscriptMessage::User { content, .. } => {
            hash.write_field(b"user").write_field(content.as_bytes());
        }
        TranscriptMessage::Assistant {
            content,
            reasoning_content,
            tool_calls,
            ..
        } => {
            hash.write_field(b"assistant")
                .write_field(content.as_bytes());
            if let Some(reasoning) = reasoning_content {
                hash.write_field(reasoning.as_bytes());
            }
            if let Ok(bytes) = serde_json::to_vec(tool_calls) {
                hash.write_field(&bytes);
            }
        }
        TranscriptMessage::Tool { summary, .. } => {
            hash.write_field(b"tool").write_field(summary.as_bytes());
        }
        TranscriptMessage::Compact { summary, .. } | TranscriptMessage::Fold { summary, .. } => {
            hash.write_field(b"summary").write_field(summary.as_bytes());
        }
    }
    hash.finish()
}

fn provider_cache_eligible(route: &str) -> bool {
    let route = route.to_ascii_lowercase();
    ["claude", "anthropic", "gpt-", "openai", "deepseek"]
        .iter()
        .any(|provider| route.contains(provider))
}

fn provider_cache_coverage_tokens(
    messages: &[&TranscriptMessage],
    tool_defs: &[serde_json::Value],
) -> Option<u64> {
    let message_bytes = messages
        .iter()
        .map(|message| match message {
            TranscriptMessage::System { content, .. }
            | TranscriptMessage::User { content, .. }
            | TranscriptMessage::Assistant { content, .. } => content.len() as u64,
            TranscriptMessage::Tool { summary, .. }
            | TranscriptMessage::Compact { summary, .. }
            | TranscriptMessage::Fold { summary, .. } => summary.len() as u64,
        })
        .sum::<u64>();
    let tool_bytes = serde_json::to_vec(tool_defs).ok()?.len() as u64;
    let bytes = message_bytes.saturating_add(tool_bytes);
    (bytes > 0).then_some(bytes.saturating_add(3) / 4)
}
