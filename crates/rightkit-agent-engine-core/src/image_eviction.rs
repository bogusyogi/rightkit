//! Request-copy image-history eviction with high/low-water hysteresis.
//!
//! Portions of the algorithm and test corpus are adapted and modified from
//! xai-org/grok-build at commit `8adf9013a0929e5c7f1d4e849492d2387837a28d`,
//! `crates/codegen/xai-chat-state/src/actor/request_builder.rs`, under Apache-2.0.
//! the host app modifications operate on `TranscriptMessage`, prioritize pinned
//! messages, and expose non-content evidence through the daemon event stream.

use rightkit_agent_engine_protocol::TranscriptMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;

const MIB: usize = 1024 * 1024;
pub const IMAGE_HISTORY_TRIGGER_BYTES: usize = 8 * MIB;
pub const IMAGE_HISTORY_RECLAIM_TARGET_BYTES: usize = 4 * MIB;
const _: () = assert!(IMAGE_HISTORY_RECLAIM_TARGET_BYTES < IMAGE_HISTORY_TRIGGER_BYTES);

const IMAGE_EVICTION_PLACEHOLDER: &str = "[An earlier image was removed to keep the model request within its size limit and is no longer visible. Do not describe or reason about its contents from memory; ask the user to re-share it if you need to see it again.]";

/// Route family used to select an operational request budget. These are
/// the host app safety budgets, not claims about undocumented provider limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageProviderProfile {
    OpenAi,
    OpenRouter,
    MiniMax,
    Xai,
    Custom,
    Unknown,
}

impl ImageProviderProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::MiniMax => "minimax",
            Self::Xai => "xai",
            Self::Custom => "custom",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageAccountingBasis {
    /// Exact bytes produced by the OpenAI-compatible chat-completions request
    /// serializer, including tools and provider extensions.
    OpenAiChatCompletionsJsonV1,
    /// Conservative engine-only fallback when a client cannot expose its wire
    /// request. This never receives a known-provider budget.
    TranscriptJsonFallback,
}

impl ImageAccountingBasis {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletionsJsonV1 => "openai_chat_completions_json_v1",
            Self::TranscriptJsonFallback => "transcript_json_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageBudgetPolicy {
    pub provider_profile: ImageProviderProfile,
    pub trigger_bytes: usize,
    pub reclaim_target_bytes: usize,
    pub source: &'static str,
}

impl ImageBudgetPolicy {
    pub const fn conservative_unknown() -> Self {
        Self {
            provider_profile: ImageProviderProfile::Unknown,
            trigger_bytes: IMAGE_HISTORY_TRIGGER_BYTES,
            reclaim_target_bytes: IMAGE_HISTORY_RECLAIM_TARGET_BYTES,
            source: "builtin-conservative",
        }
    }

    /// Provider-specific operational defaults. Operators can override these
    /// through [`Self::with_overrides`]; no value here represents a published
    /// provider maximum.
    pub const fn for_profile(provider_profile: ImageProviderProfile) -> Self {
        let (trigger_bytes, reclaim_target_bytes) = match provider_profile {
            ImageProviderProfile::OpenAi | ImageProviderProfile::Xai => (32 * MIB, 16 * MIB),
            ImageProviderProfile::OpenRouter => (24 * MIB, 12 * MIB),
            ImageProviderProfile::MiniMax => (16 * MIB, 8 * MIB),
            ImageProviderProfile::Custom | ImageProviderProfile::Unknown => (8 * MIB, 4 * MIB),
        };
        Self {
            provider_profile,
            trigger_bytes,
            reclaim_target_bytes,
            source: "builtin-conservative",
        }
    }

    /// Apply configured high/low water marks only when they preserve a valid
    /// hysteresis interval. Invalid configuration fails closed to defaults.
    pub const fn with_overrides(self, high: Option<usize>, low: Option<usize>) -> Self {
        let trigger_bytes = match high {
            Some(value) if value > 0 => value,
            _ => self.trigger_bytes,
        };
        let reclaim_target_bytes = match low {
            Some(value) => value,
            None => self.reclaim_target_bytes,
        };
        if reclaim_target_bytes >= trigger_bytes {
            return self;
        }
        Self {
            provider_profile: self.provider_profile,
            trigger_bytes,
            reclaim_target_bytes,
            source: if high.is_some() || low.is_some() {
                "configured-override"
            } else {
                self.source
            },
        }
    }

    pub const fn from_provider_metadata(
        provider_profile: ImageProviderProfile,
        trigger_bytes: usize,
        reclaim_target_bytes: usize,
    ) -> Self {
        let fallback = Self::for_profile(provider_profile);
        if trigger_bytes == 0 || reclaim_target_bytes >= trigger_bytes {
            return fallback;
        }
        Self {
            provider_profile,
            trigger_bytes,
            reclaim_target_bytes,
            source: "provider-catalog",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRequestAccounting {
    pub body_bytes: usize,
    pub basis: ImageAccountingBasis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageBudgetEvidence {
    pub body_bytes_before: usize,
    pub body_bytes_after: usize,
    pub trigger_bytes: usize,
    pub reclaim_target_bytes: usize,
    pub inline_images: usize,
    pub evicted: usize,
    pub pinned_evicted: usize,
    pub triggered: bool,
    pub provider_profile: ImageProviderProfile,
    pub policy_source: &'static str,
    pub accounting_basis: ImageAccountingBasis,
}

#[derive(Debug, Clone)]
pub struct PreparedImageHistory {
    /// Present only when eviction produced a request-only history copy.
    pub messages: Option<Vec<TranscriptMessage>>,
    pub evidence: ImageBudgetEvidence,
}

/// CRA-10 (semantic-contracts.md "Model-aware capacity & final insertion"):
/// projection-only copy of the surviving request history for final capacity
/// accounting, plus how many inline images survive in it. Capacity
/// accounting never changes eviction's trigger, candidates, prepared copy,
/// or transmitted request — this reads whatever `PreparedImageHistory`
/// already selected (`messages`, falling back to the original `original`
/// slice when eviction did not trigger) and produces a *separate* copy with
/// each surviving image URL replaced by the exact marker
/// `data:image/placeholder;base64,<omitted>`. Base64/data-URI bytes are
/// excluded from token accounting by construction; the caller (bin crate,
/// which owns the `host_providers` `o200k_base` measurement +
/// `IMAGE_TOKEN_COST` heuristic — see `provider_client.rs`) counts only this
/// projection, never the transmitted request.
pub struct FinalImageProjection {
    /// Serialized JSON array of the surviving request's user-content parts,
    /// per message, with every surviving image URL replaced by the
    /// placeholder marker. Callers concatenate/serialize this alongside the
    /// rest of the prepared provider body per their own wire format; this
    /// type intentionally does not assume OpenAI-compatible vs. Anthropic
    /// shape beyond the shared multipart `image_url` convention already used
    /// by eviction above.
    pub projected_parts_json: Vec<String>,
    pub surviving_inline_image_count: u64,
}

impl PreparedImageHistory {
    /// Build the CRA-10 final-image projection over whichever message set
    /// this prepared history actually selected: the eviction-produced copy
    /// when `messages` is `Some`, otherwise `original` unchanged (eviction
    /// did not trigger, so the transmitted request is the original history).
    pub fn final_image_projection(&self, original: &[TranscriptMessage]) -> FinalImageProjection {
        let source: &[TranscriptMessage] = self.messages.as_deref().unwrap_or(original);
        let mut projected_parts_json = Vec::new();
        let mut surviving_inline_image_count: u64 = 0;
        for message in source {
            let Some((content, _pinned)) = user_content_and_pin(message) else {
                continue;
            };
            let Some(mut parts) = multipart_parts(content) else {
                continue;
            };
            for part in parts.iter_mut() {
                if image_url(part).is_some() {
                    if let Some(image) = part.get_mut("image_url").and_then(|v| v.get_mut("url")) {
                        *image = serde_json::Value::String(
                            "data:image/placeholder;base64,<omitted>".to_string(),
                        );
                        surviving_inline_image_count =
                            surviving_inline_image_count.saturating_add(1);
                    }
                }
            }
            if let Ok(json) = serde_json::to_string(&parts) {
                projected_parts_json.push(json);
            }
        }
        FinalImageProjection {
            projected_parts_json,
            surviving_inline_image_count,
        }
    }
}

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_json_bytes<T: Serialize + ?Sized>(value: &T) -> usize {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value).map_or(0, |_| counter.0)
}

fn multipart_parts(content: &str) -> Option<Vec<Value>> {
    let parts = serde_json::from_str::<Vec<Value>>(content).ok()?;
    parts
        .iter()
        .any(|part| image_url(part).is_some())
        .then_some(parts)
}

fn image_url(part: &Value) -> Option<&str> {
    (part.get("type").and_then(Value::as_str) == Some("image_url"))
        .then(|| {
            part.get("image_url")
                .and_then(|image| image.get("url"))
                .and_then(Value::as_str)
        })
        .flatten()
        .filter(|url| url.starts_with("data:image/") && url.contains(";base64,"))
}

fn user_content_and_pin(message: &TranscriptMessage) -> Option<(&str, bool)> {
    match message {
        TranscriptMessage::User {
            content, pinned, ..
        } => Some((content, *pinned)),
        _ => None,
    }
}

fn set_user_content(message: &mut TranscriptMessage, content: String) {
    if let TranscriptMessage::User {
        content: current, ..
    } = message
    {
        *current = content;
    }
}

/// Exact JSON byte count of assembled transcript history without repeatedly
/// scanning multi-megabyte base64 strings during serialization.
fn assembled_history_bytes(messages: &[TranscriptMessage]) -> (usize, usize) {
    let mut blanked = messages.to_vec();
    let mut image_url_bytes = 0usize;
    let mut image_count = 0usize;
    for message in &mut blanked {
        let Some((content, _)) = user_content_and_pin(message) else {
            continue;
        };
        let Some(mut parts) = multipart_parts(content) else {
            continue;
        };
        for part in &mut parts {
            let Some(url) = image_url(part) else {
                continue;
            };
            image_url_bytes = image_url_bytes.saturating_add(url.len());
            image_count = image_count.saturating_add(1);
            if let Some(url) = part
                .get_mut("image_url")
                .and_then(Value::as_object_mut)
                .and_then(|image| image.get_mut("url"))
            {
                *url = Value::String(String::new());
            }
        }
        if let Ok(content) = serde_json::to_string(&parts) {
            set_user_content(message, content);
        }
    }
    (
        serialized_json_bytes(&blanked).saturating_add(image_url_bytes),
        image_count,
    )
}

/// Count inline (`data:image/...;base64,`) attachments currently present in
/// the model-visible transcript. Used by the live compaction trigger to
/// project image-payload token cost into the context-fit decision so an
/// image-heavy session trips `AutoCompact`/`CriticalSync` on projected cost.
pub fn inline_image_count(messages: &[TranscriptMessage]) -> u64 {
    let mut count = 0usize;
    for message in messages {
        let Some((content, _)) = user_content_and_pin(message) else {
            continue;
        };
        let Some(parts) = multipart_parts(content) else {
            continue;
        };
        for part in &parts {
            if image_url(part).is_some() {
                count = count.saturating_add(1);
            }
        }
    }
    count as u64
}

#[derive(Debug)]
struct MultipartMessage {
    message_index: usize,
    pinned: bool,
    parts: Vec<Value>,
}

#[derive(Debug, Clone, Copy)]
struct ImageSlot {
    multipart_index: usize,
    part_index: usize,
    pinned: bool,
}

fn multipart_state(messages: &[TranscriptMessage]) -> (Vec<MultipartMessage>, Vec<ImageSlot>) {
    let mut multipart = Vec::new();
    let mut slots = Vec::new();

    for (message_index, message) in messages.iter().enumerate() {
        let Some((content, pinned)) = user_content_and_pin(message) else {
            continue;
        };
        let Some(parts) = multipart_parts(content) else {
            continue;
        };
        let multipart_index = multipart.len();
        for (part_index, part) in parts.iter().enumerate() {
            if image_url(part).is_none() {
                continue;
            }
            slots.push(ImageSlot {
                multipart_index,
                part_index,
                pinned,
            });
        }
        multipart.push(MultipartMessage {
            message_index,
            pinned,
            parts,
        });
    }
    (multipart, slots)
}

pub fn prepare_image_history(messages: &[TranscriptMessage]) -> PreparedImageHistory {
    prepare_image_history_with_accounting(
        messages,
        ImageBudgetPolicy::conservative_unknown(),
        |messages| ImageRequestAccounting {
            body_bytes: assembled_history_bytes(messages).0,
            basis: ImageAccountingBasis::TranscriptJsonFallback,
        },
    )
}

/// Prepare an ephemeral request copy using the active provider's exact request
/// serializer. The stored transcript is never mutated.
pub fn prepare_image_history_with_accounting(
    messages: &[TranscriptMessage],
    policy: ImageBudgetPolicy,
    account: impl Fn(&[TranscriptMessage]) -> ImageRequestAccounting,
) -> PreparedImageHistory {
    prepare_image_history_with_policy(messages, policy, account)
}

pub(crate) fn image_budget_observation(
    session_id: &str,
    iteration: usize,
    evidence: ImageBudgetEvidence,
    created_at: String,
) -> rightkit_agent_engine_protocol::OrchestrationObservation {
    rightkit_agent_engine_protocol::OrchestrationObservation {
        id: format!("image-budget-{iteration}"),
        kind: "image_history_budget".to_string(),
        subject: session_id.to_string(),
        summary: format!(
            "multimodal request: {} image(s), {} evicted, {} → {} bytes",
            evidence.inline_images,
            evidence.evicted,
            evidence.body_bytes_before,
            evidence.body_bytes_after
        ),
        fields: std::collections::HashMap::from([
            (
                "bodyBytesBefore".to_string(),
                evidence.body_bytes_before.to_string(),
            ),
            (
                "bodyBytesAfter".to_string(),
                evidence.body_bytes_after.to_string(),
            ),
            (
                "triggerBytes".to_string(),
                evidence.trigger_bytes.to_string(),
            ),
            (
                "reclaimTargetBytes".to_string(),
                evidence.reclaim_target_bytes.to_string(),
            ),
            (
                "inlineImages".to_string(),
                evidence.inline_images.to_string(),
            ),
            ("evicted".to_string(), evidence.evicted.to_string()),
            (
                "pinnedEvicted".to_string(),
                evidence.pinned_evicted.to_string(),
            ),
            ("triggered".to_string(), evidence.triggered.to_string()),
            (
                "providerProfile".to_string(),
                evidence.provider_profile.as_str().to_string(),
            ),
            (
                "policySource".to_string(),
                evidence.policy_source.to_string(),
            ),
            (
                "accountingBasis".to_string(),
                evidence.accounting_basis.as_str().to_string(),
            ),
        ]),
        created_at,
    }
}

fn prepare_image_history_with_policy(
    messages: &[TranscriptMessage],
    policy: ImageBudgetPolicy,
    account: impl Fn(&[TranscriptMessage]) -> ImageRequestAccounting,
) -> PreparedImageHistory {
    let inline_images = assembled_history_bytes(messages).1;
    let initial_accounting = account(messages);
    let body_bytes_before = initial_accounting.body_bytes;
    let triggered = inline_images > 0 && body_bytes_before >= policy.trigger_bytes;
    let mut evidence = ImageBudgetEvidence {
        body_bytes_before,
        body_bytes_after: body_bytes_before,
        trigger_bytes: policy.trigger_bytes,
        reclaim_target_bytes: policy.reclaim_target_bytes,
        inline_images,
        evicted: 0,
        pinned_evicted: 0,
        triggered,
        provider_profile: policy.provider_profile,
        policy_source: policy.source,
        accounting_basis: initial_accounting.basis,
    };
    if !triggered {
        return PreparedImageHistory {
            messages: None,
            evidence,
        };
    }

    let (mut multipart, slots) = multipart_state(messages);
    // Pinned messages are preserved ahead of ordinary history. If ordinary
    // images cannot satisfy the hard transport budget, pinned images are then
    // evicted oldest-first and the evidence explicitly records that fallback.
    let candidates = slots
        .iter()
        .filter(|slot| !slot.pinned)
        .chain(slots.iter().filter(|slot| slot.pinned));
    let placeholder = serde_json::json!({
        "type": "text",
        "text": IMAGE_EVICTION_PLACEHOLDER,
    });
    let mut running = body_bytes_before;
    let mut prepared = messages.to_vec();
    for slot in candidates {
        if running <= policy.reclaim_target_bytes {
            break;
        }
        multipart[slot.multipart_index].parts[slot.part_index] = placeholder.clone();
        let item = &multipart[slot.multipart_index];
        if let Ok(content) = serde_json::to_string(&item.parts) {
            set_user_content(&mut prepared[item.message_index], content);
        }
        // The exact provider serializer remains authoritative. The savings
        // estimate only avoids a redundant full serialization when it is safe;
        // every changed request is remeasured to detect provider drift.
        running = account(&prepared).body_bytes;
        evidence.evicted = evidence.evicted.saturating_add(1);
        if multipart[slot.multipart_index].pinned {
            evidence.pinned_evicted = evidence.pinned_evicted.saturating_add(1);
        }
    }

    evidence.body_bytes_after = running;
    PreparedImageHistory {
        messages: Some(prepared),
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_with_image(label: &str, url_bytes: usize, pinned: bool) -> TranscriptMessage {
        const PREFIX: &str = "data:image/png;base64,";
        let url = format!(
            "{PREFIX}{}",
            "A".repeat(url_bytes.saturating_sub(PREFIX.len()))
        );
        TranscriptMessage::User {
            id: label.to_string(),
            created_at: "2026-07-17T00:00:00Z".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": label},
                {"type": "image_url", "image_url": {"url": url}}
            ])
            .to_string(),
            trust: None,
            pinned,
            attachments: Vec::new(),
        }
    }

    fn contains_image(message: &TranscriptMessage) -> bool {
        user_content_and_pin(message)
            .and_then(|(content, _)| serde_json::from_str::<Vec<Value>>(content).ok())
            .is_some_and(|parts| parts.iter().any(|part| image_url(part).is_some()))
    }

    fn contains_placeholder(message: &TranscriptMessage) -> bool {
        user_content_and_pin(message)
            .is_some_and(|(content, _)| content.contains(IMAGE_EVICTION_PLACEHOLDER))
    }

    fn prepare_with_limits(
        messages: &[TranscriptMessage],
        trigger_bytes: usize,
        reclaim_target_bytes: usize,
    ) -> PreparedImageHistory {
        prepare_image_history_with_policy(
            messages,
            ImageBudgetPolicy::conservative_unknown()
                .with_overrides(Some(trigger_bytes), Some(reclaim_target_bytes)),
            |messages| ImageRequestAccounting {
                body_bytes: assembled_history_bytes(messages).0,
                basis: ImageAccountingBasis::TranscriptJsonFallback,
            },
        )
    }

    #[test]
    fn byte_measurement_matches_serde_exactly() {
        let messages = vec![
            user_with_image("quoted \\\" unicode 🚀", 50_000, false),
            user_with_image("second", 80_000, true),
        ];
        assert_eq!(
            assembled_history_bytes(&messages).0,
            serde_json::to_vec(&messages).unwrap().len()
        );
    }

    #[test]
    fn below_high_water_is_byte_stable_and_all_images_remain() {
        let messages = vec![
            user_with_image("old", 100_000, false),
            user_with_image("new", 100_000, false),
        ];
        let before = serde_json::to_vec(&messages).unwrap();
        let prepared = prepare_with_limits(&messages, before.len() + 1, 10);
        assert!(prepared.messages.is_none());
        assert_eq!(prepared.evidence.evicted, 0);
        assert_eq!(serde_json::to_vec(&messages).unwrap(), before);
    }

    #[test]
    fn high_water_boundary_is_inclusive_for_provider_wire_bytes() {
        let messages = vec![user_with_image("boundary", 10_000, false)];
        let wire_bytes = assembled_history_bytes(&messages).0 + 777;
        let policy = ImageBudgetPolicy::for_profile(ImageProviderProfile::OpenRouter)
            .with_overrides(Some(wire_bytes), Some(wire_bytes / 2));
        let prepared = prepare_image_history_with_policy(&messages, policy, |candidate| {
            ImageRequestAccounting {
                body_bytes: assembled_history_bytes(candidate).0 + 777,
                basis: ImageAccountingBasis::OpenAiChatCompletionsJsonV1,
            }
        });

        assert!(prepared.evidence.triggered);
        assert_eq!(prepared.evidence.body_bytes_before, wire_bytes);
        assert_eq!(
            prepared.evidence.accounting_basis,
            ImageAccountingBasis::OpenAiChatCompletionsJsonV1
        );
    }

    #[test]
    fn invalid_configured_hysteresis_falls_back_to_safe_profile_defaults() {
        let default = ImageBudgetPolicy::for_profile(ImageProviderProfile::MiniMax);
        let invalid = default.with_overrides(Some(1024), Some(2048));
        assert_eq!(invalid, default);
        assert_eq!(invalid.source, "builtin-conservative");
    }

    #[test]
    fn crossing_high_water_evicts_oldest_to_low_water() {
        let messages = vec![
            user_with_image("oldest", 100_000, false),
            user_with_image("middle", 100_000, false),
            user_with_image("newest", 100_000, false),
        ];
        let current = assembled_history_bytes(&messages).0;
        let prepared = prepare_with_limits(&messages, current, current - 150_000);
        let request = prepared.messages.as_ref().unwrap();
        assert!(contains_placeholder(&request[0]));
        assert!(contains_placeholder(&request[1]));
        assert!(contains_image(&request[2]));
        assert!(
            messages.iter().all(contains_image),
            "stored history unchanged"
        );
        assert!(prepared.evidence.body_bytes_after <= current - 150_000);
    }

    #[test]
    fn final_evidence_remeasures_wire_bytes_after_each_replacement() {
        let messages = vec![
            user_with_image("old", 100_000, false),
            user_with_image("new", 100_000, false),
        ];
        let overhead = 321usize;
        let current = assembled_history_bytes(&messages).0 + overhead;
        let policy = ImageBudgetPolicy::for_profile(ImageProviderProfile::Xai)
            .with_overrides(Some(current), Some(current - 50_000));
        let prepared = prepare_image_history_with_policy(&messages, policy, |candidate| {
            ImageRequestAccounting {
                body_bytes: assembled_history_bytes(candidate).0 + overhead,
                basis: ImageAccountingBasis::OpenAiChatCompletionsJsonV1,
            }
        });
        let request = prepared.messages.as_ref().unwrap();

        assert_eq!(
            prepared.evidence.body_bytes_after,
            assembled_history_bytes(request).0 + overhead
        );
        assert!(prepared.evidence.body_bytes_after <= policy.reclaim_target_bytes);
    }

    #[test]
    fn pinned_images_are_evicted_only_after_unpinned_images() {
        let messages = vec![
            user_with_image("pinned-old", 100_000, true),
            user_with_image("ordinary", 100_000, false),
            user_with_image("pinned-new", 100_000, true),
        ];
        let current = assembled_history_bytes(&messages).0;
        let prepared = prepare_with_limits(&messages, current, current - 50_000);
        let request = prepared.messages.as_ref().unwrap();
        assert!(contains_image(&request[0]));
        assert!(contains_placeholder(&request[1]));
        assert!(contains_image(&request[2]));
        assert_eq!(prepared.evidence.pinned_evicted, 0);
    }

    #[test]
    fn repeated_preparation_is_deterministic_and_keeps_text_order() {
        let messages = vec![
            user_with_image("first", 100_000, false),
            user_with_image("second", 100_000, false),
            user_with_image("third", 100_000, false),
        ];
        let current = assembled_history_bytes(&messages).0;
        let first = prepare_with_limits(&messages, current, current - 50_000)
            .messages
            .unwrap();
        let second = prepare_with_limits(&messages, current, current - 50_000)
            .messages
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert!(user_content_and_pin(&first[0]).unwrap().0.contains("first"));
        assert!(user_content_and_pin(&first[1])
            .unwrap()
            .0
            .contains("second"));
        assert!(user_content_and_pin(&first[2]).unwrap().0.contains("third"));
    }

    #[test]
    fn eviction_evidence_is_typed_content_free_and_ui_safe() {
        let observation = image_budget_observation(
            "session-1",
            3,
            ImageBudgetEvidence {
                body_bytes_before: 50_000_000,
                body_bytes_after: 25_000_000,
                trigger_bytes: 47_000_000,
                reclaim_target_bytes: 25_000_000,
                inline_images: 4,
                evicted: 2,
                pinned_evicted: 0,
                triggered: true,
                provider_profile: ImageProviderProfile::OpenRouter,
                policy_source: "builtin-conservative",
                accounting_basis: ImageAccountingBasis::OpenAiChatCompletionsJsonV1,
            },
            "2026-07-17T00:00:00Z".into(),
        );

        assert_eq!(observation.kind, "image_history_budget");
        assert_eq!(observation.subject, "session-1");
        assert_eq!(observation.fields["evicted"], "2");
        assert_eq!(observation.fields["triggered"], "true");
        assert_eq!(observation.fields["providerProfile"], "openrouter");
        assert_eq!(
            observation.fields["accountingBasis"],
            "openai_chat_completions_json_v1"
        );
        assert!(!serde_json::to_string(&observation)
            .unwrap()
            .contains("data:image"));
    }
}
