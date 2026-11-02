#![allow(clippy::result_large_err)]

//! Real HTTP streaming client for OpenAI-compatible endpoints.
//!
//! `OpenAiCompatClient` makes a single streaming chat-completion request and
//! returns a fully assembled `CompletedTurn`. It is provider-agnostic — point
//! it at OpenRouter or a custom OpenAI-compatible endpoint via `base_url`.
//!
//! `ra-018` decomposition: this file is now a facade. Wire DTOs/stream
//! accumulation live in `wire`, HTTP request/response handling in
//! `transport`, retry/backoff/rate-limit parsing in `retry`, the per-host
//! circuit breaker in `breaker`, API-key resolution in `credentials`, and
//! the known-provider catalog in `catalog`. `OpenAiCompatClient`'s public
//! API, module path (`crate::agent_providers::client::OpenAiCompatClient`), and wire format
//! are unchanged — see
//! `.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`.
//!
//! RIGHTKIT-SERIOUS: rightkit-llm Provider is blocking; no async cancellable stream, request authority or submission receipts for the OpenAI-compatible and Codex clients (report P-1)

mod breaker;
mod catalog;
mod credentials;
mod retry;
mod transport;
mod wire;

use crate::agent_providers::transport::{ProtocolFamily, ProviderError, RateLimitMetadata};
use rightkit_agent_engine_protocol::ToolCall;
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use wire::{ChatRequest, ReasoningRequest, StreamOptions};

// Re-exports that keep every pre-decomposition `crate::agent_providers::client::…` path
// working unchanged for `anthropic_client.rs`, `lib.rs`'s public re-export
// list, and this file's own `#[cfg(test)]` modules.
pub use catalog::{
    find_provider, find_provider_by_base_url, is_subscription_billed_route, provider_model_id,
    token_plan_provider_for_base_url, ProviderInfo, PROVIDERS,
};
pub use credentials::{
    account_key_name, account_key_present_with, delete_account_key_with, load_account_key_with,
    store_account_key_with, ACCOUNT_KEY_SERVICE,
};
#[cfg(feature = "agent_providers-os-keyring")]
pub use credentials::{
    account_key_present, delete_account_key, load_account_key, store_account_key,
};
pub use credentials::{env_api_key, load_api_key};
pub use wire::{strip_think_tags, transcript_to_wire};

pub(crate) use breaker::{
    breaker_allows_request, breaker_record_outcome, is_breaker_relevant_failure,
};
pub(crate) use retry::{
    observe_stream_breaker_outcome, record_stream_establishment_outcome, retry_after_secs,
};

/// True only for a failure before the request left (name lookup, TCP connect, TLS): the published
/// `rightkit-http` `AttemptDelivery::NotSent` cases. Any later transport failure or timeout may
/// have been delivered and billed, so a paid POST is never re-sent for it.
pub(crate) fn is_connect_phase_failure(error: &rightkit_http::HttpError) -> bool {
    matches!(
        error.class(),
        rightkit_http::ErrorClass::Transport | rightkit_http::ErrorClass::Timeout
    ) && error.phase() == Some(rightkit_http::Phase::Connect)
}

/// Announce and wait out one connect-phase retry (the only replay a paid POST gets).
pub(crate) async fn wait_connection_retry(
    notifier: &rightkit_agent_engine_protocol::RetryNotifier,
    base_url: &str,
    attempt: u32,
    max_retries: u32,
) {
    retry::notify_retry(
        notifier,
        base_url,
        "connection error",
        attempt,
        max_retries,
        retry::connection_backoff_duration(attempt),
    );
    retry::connection_backoff(attempt).await;
}

pub(crate) use wire::{
    can_continue_incomplete, continuation_messages, incomplete_output_error,
    is_incomplete_finish_reason, merge_continuation, validate_complete_tool_calls,
};

/// Assembled result of one streaming chat completion.
#[derive(Debug, Clone, Default)]
pub struct CompletedTurn {
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    /// Reasoning/thinking tokens itemized inside `completion_tokens`
    /// (OpenAI `completion_tokens_details.reasoning_tokens`, Responses
    /// `output_tokens_details.reasoning_tokens`). Zero when not itemized;
    /// never additive to `completion_tokens`.
    pub reasoning_tokens: u32,
    /// `S:M2`/`S:M3` amendment: which wire protocol produced this turn,
    /// independent of auth source, so downstream code never has to assume
    /// OpenAI-compatibility. `None` on turns built before this field
    /// existed (e.g. replay fixtures) — additive, not required.
    pub protocol_family: Option<ProtocolFamily>,
    /// `S:M2`/`S:M3` amendment: rate-limit metadata captured from the wire
    /// response headers for this turn (remaining budget + reset timing),
    /// carried on the general turn contract rather than only appearing
    /// once a `RateLimited` error is hit.
    pub rate_limit: Option<RateLimitMetadata>,
}

/// Incremental provider output consumed by the engine streaming seam.
#[derive(Debug, Clone)]
pub enum ProviderStreamEvent {
    Text(String),
    Reasoning(String),
    ToolCall(ToolCall),
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    },
    /// Itemized detail for the preceding `Usage` event of the same response.
    /// `reasoning_tokens` is a subset of `completion_tokens`.
    UsageDetail {
        reasoning_tokens: u32,
    },
    Finished(Option<String>),
}

pub type ProviderResponseStream =
    Pin<Box<dyn futures::Stream<Item = Result<ProviderStreamEvent, ProviderError>> + Send>>;

/// CRA-10: prepared-provider-request capacity types shared by every wire
/// producer/consumer (`client/transport.rs::build_chat_request`,
/// `anthropic_client.rs::build_message_body`, `client/wire.rs::continuation_messages`,
/// `host-app/src/provider_client.rs`, `crates/engine/src/image_eviction.rs`).
///
/// Binding formula — verbatim from `semantic-contracts.md` "Model-aware
/// capacity & final insertion" (packet authority for CRA-10; **never
/// substitute bytes/4**):
///
/// - Basis is always `o200k_base/1`, labeled **Estimated**, never
///   provider-reported, for text and existing inline-image normal routes.
/// - `PreparedProviderRequestV1` carries the deterministic prepared body
///   bytes, route identity, serializer/tool-catalog generation, modality
///   recipe, content type, and a SHA-256 digest of those exact bytes. That
///   same immutable byte sequence is what gets measured with
///   `tiktoken-rs` `o200k_base().encode_ordinary`, gated, and sent with
///   `.body(prepared.bytes)` plus its declared content type — never
///   `.json(...)` reserialization.
/// - `PreparedRequestGateV1` carries the H8 capacity profile
///   (`unit=o200k_base/1`, `window_tokens=context_window_tokens`,
///   `reserve_tokens=compaction_reserve_tokens`, model/route id, catalog
///   revision). `context_window_tokens` is the sole effective limit; a
///   trigger percentage or `compaction_max_output_tokens` never substitute
///   for it. Missing limit/reservation returns `CapacityUnavailable` before
///   normal provider dispatch, and an over-budget prepared body returns
///   `FinalInsertionOverBudget` rather than being silently truncated.
/// - Every continuation (`client/wire.rs::continuation_messages`, OpenAI
///   transport continuation, Anthropic continuation) must obtain fresh
///   Membrane/host authorization for the changed body or receive typed
///   `ContinuationReauthorizationRequired` — no hidden continuation may send
///   without one of those two outcomes.
/// - For the existing inline-image eviction policy
///   (`prepare_image_history_with_accounting`, unchanged trigger/candidates/
///   prepared copy), final accounting counts a *projection-only* copy of the
///   final request JSON with each surviving image URL replaced by the exact
///   marker `data:image/placeholder;base64,<omitted>`, then adds the legacy
///   `IMAGE_TOKEN_COST` (765) once per surviving inline image. Base64/data-URI
///   bytes are excluded from token accounting; that image-URL exclusion is
///   applied by `PreparedRequestGateV1::admit` too (audit C8), so base64
///   bytes are never tokenized as text.
pub mod capacity {

    /// CRA-10 mandatory `EstimatorBasisV1` label. Never any other basis for
    /// a normal provider route, and never presented as provider-reported.
    /// Wire forms carry `id`/`version` as separate fields; this is their
    /// joined display form (`{id}/{version}`).
    pub const CRA10_ESTIMATOR_BASIS: &str = "o200k_base/1";

    /// CRA-10 `EstimatorBasisV1.id` wire field — the tokenizer identity only.
    /// Membrane's exact packet measurement accepts exactly this pair.
    pub const CRA10_ESTIMATOR_BASIS_ID: &str = "o200k_base";

    /// CRA-10 `EstimatorBasisV1.version` wire field.
    pub const CRA10_ESTIMATOR_BASIS_VERSION: &str = "1";

    /// Legacy per-surviving-inline-image heuristic (semantic-contracts.md
    /// §2). Cross-model variable; never provider-equivalent image accounting
    /// or a safe bound.
    pub const IMAGE_TOKEN_COST: u64 = 765;

    /// Placeholder marker substituted for each surviving inline image's
    /// data-URI before the projection-only measurement copy is counted.
    pub const IMAGE_PLACEHOLDER_MARKER: &str = "data:image/placeholder;base64,<omitted>";

    /// Modality recipe carried on a prepared body — admission requires
    /// resolved route capability `vision=true` plus multipart serializer
    /// support for `TextPlusInlineImages`; a route lacking that capability
    /// is typed-unsupported handling, never silent downgrade.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PreparedModalityV1 {
        TextOnly,
        TextPlusInlineImages,
    }

    /// Immutable final provider request bytes plus the identity needed to
    /// prove measurement/digest/route/content-type equality end to end.
    /// Construct this *after* deterministic body serialization
    /// (`build_chat_request` / `build_message_body`) — never before it —
    /// so `body_digest` covers exactly what is sent.
    #[derive(Debug, Clone)]
    pub struct PreparedProviderRequestV1 {
        pub bytes: Vec<u8>,
        pub content_type: &'static str,
        pub route_fingerprint: String,
        pub model_id: String,
        pub serializer_generation: u64,
        pub tool_catalog_generation: Option<u64>,
        pub modality: PreparedModalityV1,
        pub body_digest: String,
        pub request_identity: Option<super::ProviderRequestIdentityV1>,
        pub authorization_id: Option<String>,
        pub authority_state: super::RequestAuthorityStateV1,
        /// Admission measurement, reused unchanged for every physical send.
        pub measured_tokens: Option<u64>,
    }

    impl PreparedProviderRequestV1 {
        pub fn new(
            bytes: Vec<u8>,
            content_type: &'static str,
            route_fingerprint: impl Into<String>,
            model_id: impl Into<String>,
            serializer_generation: u64,
            tool_catalog_generation: Option<u64>,
            modality: PreparedModalityV1,
        ) -> Self {
            let body_digest = rightkit_fs::digest::sha256_bytes(&bytes);
            Self {
                bytes,
                content_type,
                route_fingerprint: route_fingerprint.into(),
                model_id: model_id.into(),
                serializer_generation,
                tool_catalog_generation,
                modality,
                body_digest,
                request_identity: None,
                authorization_id: None,
                authority_state: super::RequestAuthorityStateV1::Unavailable,
                measured_tokens: None,
            }
        }
    }

    /// H8 capacity profile for one route/model. `window_tokens` is the sole
    /// effective limit; `reserve_tokens` is sole heuristic headroom margin.
    /// There is no hidden bytes/4 conversion or derived provider guarantee.
    #[derive(Debug, Clone)]
    pub struct CapacityProfileV1 {
        pub unit: &'static str,
        pub window_tokens: u64,
        pub reserve_tokens: u64,
        pub model_id: String,
        pub route_id: String,
        pub catalog_revision: Option<String>,
    }

    impl CapacityProfileV1 {
        pub fn new(
            window_tokens: u64,
            reserve_tokens: u64,
            model_id: impl Into<String>,
            route_id: impl Into<String>,
            catalog_revision: Option<String>,
        ) -> Self {
            Self {
                unit: CRA10_ESTIMATOR_BASIS,
                window_tokens,
                reserve_tokens,
                model_id: model_id.into(),
                route_id: route_id.into(),
                catalog_revision,
            }
        }
    }

    /// CRA-10 typed failure modes. None of these are silently downgraded to
    /// a best-effort send.
    #[derive(Debug, Clone, thiserror::Error)]
    pub enum CapacityError {
        #[error("CRA-10 capacity unavailable for route {route_id}: {reason}")]
        CapacityUnavailable { route_id: String, reason: String },
        #[error(
            "CRA-10 final insertion over budget: measured {measured_tokens} tokens exceeds effective budget {effective_budget_tokens} tokens"
        )]
        FinalInsertionOverBudget {
            measured_tokens: u64,
            effective_budget_tokens: u64,
        },
        #[error("CRA-10 continuation requires fresh host/Membrane reauthorization before send")]
        ContinuationReauthorizationRequired,
    }

    /// Measures `text` under the mandatory `o200k_base/1` Estimated basis.
    /// This is the **only** sanctioned measurement path for CRA-10 —
    /// `bytes.len() / 4` or any other heuristic conversion must never be
    /// substituted here.
    ///
    /// The BPE rank table is built once per process: `o200k_base()` parses
    /// the bundled ranks on every call, and admission measures each request.
    pub fn measure_o200k_base(text: &str) -> u64 {
        static BPE: std::sync::OnceLock<tiktoken_rs::CoreBPE> = std::sync::OnceLock::new();
        let bpe = BPE.get_or_init(|| {
            tiktoken_rs::o200k_base()
                .expect("o200k_base BPE ranks are statically bundled by tiktoken-rs")
        });
        bpe.encode_ordinary(text).len() as u64
    }

    /// CRA-10 image-eviction final accounting: counts only the
    /// projection-only copy (surviving image URLs already replaced with
    /// [`IMAGE_PLACEHOLDER_MARKER`]) with `measure_o200k_base`, then adds
    /// [`IMAGE_TOKEN_COST`] once per surviving inline image. Base64/data-URI
    /// bytes are excluded by construction of that projection.
    pub fn measure_image_projection(
        projected_body_json: &str,
        surviving_inline_image_count: u64,
    ) -> u64 {
        measure_o200k_base(projected_body_json)
            + IMAGE_TOKEN_COST.saturating_mul(surviving_inline_image_count)
    }

    /// Replaces every inline base64 image payload in a JSON request body with
    /// [`IMAGE_PLACEHOLDER_MARKER`] and returns the projected body text plus
    /// the number of images replaced. Handles OpenAI-style `data:` URIs and
    /// Anthropic `{"type":"base64","data":...}` sources. A body that is not
    /// JSON is returned unchanged with a zero count.
    pub fn project_inline_images(body: &str) -> (String, u64) {
        fn walk(value: &mut serde_json::Value, count: &mut u64) {
            match value {
                serde_json::Value::String(text) => {
                    if text.starts_with("data:") && text.contains(";base64,") {
                        *text = IMAGE_PLACEHOLDER_MARKER.to_string();
                        *count += 1;
                    }
                }
                serde_json::Value::Array(items) => {
                    items.iter_mut().for_each(|item| walk(item, count));
                }
                serde_json::Value::Object(map) => {
                    let is_base64_source = map.get("type").and_then(|v| v.as_str())
                        == Some("base64")
                        && map.get("data").is_some_and(|v| v.is_string());
                    if is_base64_source {
                        map.insert(
                            "data".to_string(),
                            serde_json::Value::String("<omitted>".to_string()),
                        );
                        *count += 1;
                    }
                    map.values_mut().for_each(|item| walk(item, count));
                }
                _ => {}
            }
        }
        if !body.contains("base64") {
            return (body.to_string(), 0);
        }
        match serde_json::from_str::<serde_json::Value>(body) {
            Ok(mut value) => {
                let mut count = 0;
                walk(&mut value, &mut count);
                (value.to_string(), count)
            }
            Err(_) => (body.to_string(), 0),
        }
    }

    /// Gate bound to one route/model's [`CapacityProfileV1`]. Owns the sole
    /// sanctioned admission path for a [`PreparedProviderRequestV1`].
    #[derive(Debug, Clone)]
    pub struct PreparedRequestGateV1 {
        pub profile: CapacityProfileV1,
    }

    impl PreparedRequestGateV1 {
        pub fn new(profile: CapacityProfileV1) -> Self {
            Self { profile }
        }

        /// `effective_limit - reserve - measured_base_wire_tokens`: measures
        /// `prepared.bytes` (the immutable sent body, decoded as UTF-8 —
        /// never `bytes.len() / 4`) with `measure_o200k_base`, and admits it
        /// only when the result fits `window_tokens - reserve_tokens`.
        /// Returns `FinalInsertionOverBudget` instead of truncating, and
        /// `CapacityUnavailable` when the profile itself cannot express a
        /// positive budget (missing/zero limit, or reserve >= window).
        pub fn admit(&self, prepared: &PreparedProviderRequestV1) -> Result<u64, CapacityError> {
            let limit = self.profile.window_tokens;
            let reserve = self.profile.reserve_tokens;
            if limit == 0 || reserve >= limit {
                return Err(CapacityError::CapacityUnavailable {
                    route_id: self.profile.route_id.clone(),
                    reason: format!(
                        "window_tokens={limit} reserve_tokens={reserve}: no positive effective budget"
                    ),
                });
            }
            let budget = limit - reserve;
            let body_text = String::from_utf8_lossy(&prepared.bytes);
            // Audit C8: base64 image payloads are not billed per byte; measure
            // a placeholder projection plus IMAGE_TOKEN_COST per image.
            let (projected, image_count) = project_inline_images(&body_text);
            let measured = measure_image_projection(&projected, image_count);
            if measured > budget {
                return Err(CapacityError::FinalInsertionOverBudget {
                    measured_tokens: measured,
                    effective_budget_tokens: budget,
                });
            }
            Ok(measured)
        }

        /// CRA-10 continuation rule: `client/wire.rs::continuation_messages`,
        /// OpenAI transport continuation, and Anthropic continuation must
        /// each call this (never send a changed continuation body directly).
        /// `fresh_authorization` is the caller's proof — an opaque
        /// host/Membrane selection/receipt token for *this* continuation —
        /// that fresh authorization was obtained; its absence is
        /// `ContinuationReauthorizationRequired`, never a silent send.
        /// Identical-byte retries (same `prepared.body_digest`) may reuse
        /// the prior `fresh_authorization` the caller already holds.
        pub fn admit_continuation(
            &self,
            prepared: &PreparedProviderRequestV1,
            fresh_authorization: Option<&str>,
        ) -> Result<u64, CapacityError> {
            if fresh_authorization.is_none() {
                return Err(CapacityError::ContinuationReauthorizationRequired);
            }
            self.admit(prepared)
        }
    }
}

pub use capacity::{
    CapacityError, CapacityProfileV1, PreparedModalityV1, PreparedProviderRequestV1,
    PreparedRequestGateV1, CRA10_ESTIMATOR_BASIS, CRA10_ESTIMATOR_BASIS_ID,
    CRA10_ESTIMATOR_BASIS_VERSION, IMAGE_PLACEHOLDER_MARKER, IMAGE_TOKEN_COST,
};

/// Host-supplied causal identity. A changed draft advances generation; retries
/// retain this identity together with the exact prepared bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestIdentityV1 {
    pub session_id: String,
    pub task_id: String,
    pub request_id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestAuthorityStateV1 {
    Authorized,
    /// Provider-only operation is not a completed federated authorization.
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct RequestAuthorityInputV1 {
    pub identity: ProviderRequestIdentityV1,
    pub draft_body: serde_json::Value,
    pub capacity_profile: CapacityProfileV1,
    pub continuation: bool,
}

#[derive(Debug, Clone)]
pub struct RequestAuthorityResponseV1 {
    /// Opaque host authorization; never synthesized by a provider adapter.
    pub authorization_id: String,
    /// Sole body serialized, measured, admitted & sent after authorization.
    pub final_body: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestAuthorityErrorV1 {
    #[error("request authority unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("request authority denied: {reason}")]
    Denied { reason: String },
    #[error("changed continuation requires fresh request authority")]
    ContinuationReauthorizationRequired,
    #[error("invalid request authority response: {reason}")]
    InvalidResponse { reason: String },
}

/// Provider-neutral injection point; implementation owns context federation.
/// It must return only after authorization of this identity, route & draft.
pub trait RequestAuthorityV1: Send + Sync {
    fn authorize(
        &self,
        input: RequestAuthorityInputV1,
    ) -> Result<RequestAuthorityResponseV1, RequestAuthorityErrorV1>;
}

/// Bind once per logical request, sharing this binding across route fallback.
/// Clones share generation allocation & reject reuse of authorization IDs.
#[derive(Clone)]
pub struct RequestAuthorityBindingV1 {
    identity: ProviderRequestIdentityV1,
    authority: Arc<dyn RequestAuthorityV1>,
    state: Arc<Mutex<(u64, std::collections::BTreeSet<String>)>>,
}

impl RequestAuthorityBindingV1 {
    pub fn new(
        identity: ProviderRequestIdentityV1,
        authority: Arc<dyn RequestAuthorityV1>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new((identity.generation, Default::default()))),
            identity,
            authority,
        }
    }

    fn authorize(
        &self,
        draft_body: serde_json::Value,
        capacity_profile: CapacityProfileV1,
        continuation: bool,
    ) -> Result<(ProviderRequestIdentityV1, RequestAuthorityResponseV1), RequestAuthorityErrorV1>
    {
        if [
            &self.identity.session_id,
            &self.identity.task_id,
            &self.identity.request_id,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        {
            return Err(RequestAuthorityErrorV1::InvalidResponse {
                reason: "session, task & request identities must be nonempty".into(),
            });
        }
        let invalid = || RequestAuthorityErrorV1::InvalidResponse {
            reason: "request authority binding state unavailable".into(),
        };
        let identity = {
            let mut state = self.state.lock().map_err(|_| invalid())?;
            let mut identity = self.identity.clone();
            identity.generation = state.0;
            state.0 = state.0.checked_add(1).ok_or_else(invalid)?;
            identity
        };
        let response = self.authority.authorize(RequestAuthorityInputV1 {
            identity: identity.clone(),
            draft_body,
            capacity_profile,
            continuation,
        })?;
        if response.authorization_id.trim().is_empty() || !response.final_body.is_object() {
            return Err(RequestAuthorityErrorV1::InvalidResponse {
                reason: "authorization identity must be nonempty & final body must be an object"
                    .into(),
            });
        }
        if !self
            .state
            .lock()
            .map_err(|_| invalid())?
            .1
            .insert(response.authorization_id.clone())
        {
            return Err(RequestAuthorityErrorV1::InvalidResponse {
                reason: "authorization identity was reused for a new draft".into(),
            });
        }
        Ok((identity, response))
    }
}

/// Final provider boundary shared by both protocols. Profile comes from loaded
/// route/model capabilities; authority cannot replace it or bypass admission.
pub fn prepare_authorized_request(
    draft_body: serde_json::Value,
    profile: CapacityProfileV1,
    serializer_generation: u64,
    tool_catalog_generation: Option<u64>,
    authority: Option<&RequestAuthorityBindingV1>,
    continuation: bool,
) -> Result<PreparedProviderRequestV1, ProviderError> {
    let draft_model = draft_body.get("model").cloned();
    let (body, identity, authorization_id) = if let Some(authority) = authority {
        let (identity, response) =
            authority.authorize(draft_body, profile.clone(), continuation)?;
        (
            response.final_body,
            Some(identity),
            Some(response.authorization_id),
        )
    } else {
        if continuation {
            return Err(RequestAuthorityErrorV1::ContinuationReauthorizationRequired.into());
        }
        (draft_body, None, None)
    };
    if body.get("model").cloned() != draft_model {
        return Err(RequestAuthorityErrorV1::InvalidResponse {
            reason: "authority changed model without a new route capacity profile".into(),
        }
        .into());
    }
    let model_id = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&profile.model_id)
        .to_owned();
    let modality = if body.to_string().contains("image_url")
        || body.to_string().contains("\"type\":\"image\"")
    {
        PreparedModalityV1::TextPlusInlineImages
    } else {
        PreparedModalityV1::TextOnly
    };
    let bytes =
        serde_json::to_vec(&body).map_err(|error| RequestAuthorityErrorV1::InvalidResponse {
            reason: error.to_string(),
        })?;
    let mut prepared = PreparedProviderRequestV1::new(
        bytes,
        "application/json",
        profile.route_id.clone(),
        model_id,
        serializer_generation,
        tool_catalog_generation,
        modality,
    );
    prepared.measured_tokens = Some(PreparedRequestGateV1::new(profile).admit(&prepared)?);
    prepared.authority_state = if authorization_id.is_some() {
        RequestAuthorityStateV1::Authorized
    } else {
        RequestAuthorityStateV1::Unavailable
    };
    prepared.request_identity = identity;
    prepared.authorization_id = authorization_id;
    Ok(prepared)
}

#[cfg(test)]
mod request_authority_tests {
    use super::*;

    struct FederatingAuthority {
        inputs: Mutex<Vec<RequestAuthorityInputV1>>,
        reuse_authorization: bool,
    }

    impl RequestAuthorityV1 for FederatingAuthority {
        fn authorize(
            &self,
            input: RequestAuthorityInputV1,
        ) -> Result<RequestAuthorityResponseV1, RequestAuthorityErrorV1> {
            let mut final_body = input.draft_body.clone();
            final_body["context"] =
                serde_json::json!({"federated": "héllo", "generation": input.identity.generation});
            let authorization_id = if self.reuse_authorization {
                "fixed".into()
            } else {
                format!("authority-{}", input.identity.generation)
            };
            self.inputs.lock().unwrap().push(input);
            Ok(RequestAuthorityResponseV1 {
                authorization_id,
                final_body,
            })
        }
    }

    fn binding(reuse_authorization: bool) -> (RequestAuthorityBindingV1, Arc<FederatingAuthority>) {
        let authority = Arc::new(FederatingAuthority {
            inputs: Mutex::new(Vec::new()),
            reuse_authorization,
        });
        let identity = ProviderRequestIdentityV1 {
            session_id: "session".into(),
            task_id: "task".into(),
            request_id: "request".into(),
            generation: 4,
        };
        (
            RequestAuthorityBindingV1::new(identity, authority.clone()),
            authority,
        )
    }

    fn profile(route: &str) -> CapacityProfileV1 {
        CapacityProfileV1::new(8192, 128, "model", route, Some("catalog-v1".into()))
    }

    #[test]
    fn federated_final_body_is_exact_measured_digest_source() {
        let (binding, authority) = binding(false);
        let draft = serde_json::json!({"model": "model", "messages": [{"role": "user", "content": "draft"}]});
        let prepared = prepare_authorized_request(
            draft.clone(),
            profile("route"),
            1,
            Some(9),
            Some(&binding),
            false,
        )
        .unwrap();
        let mut final_body = draft.clone();
        final_body["context"] = serde_json::json!({"federated": "héllo", "generation": 4});
        assert_eq!(prepared.bytes, serde_json::to_vec(&final_body).unwrap());
        assert_ne!(prepared.bytes, serde_json::to_vec(&draft).unwrap());
        assert_eq!(
            prepared.body_digest,
            rightkit_fs::digest::sha256_bytes(&prepared.bytes)
        );
        assert_eq!(
            prepared.measured_tokens,
            Some(capacity::measure_o200k_base(
                std::str::from_utf8(&prepared.bytes).unwrap()
            ))
        );
        assert_eq!(prepared.authorization_id.as_deref(), Some("authority-4"));
        assert_eq!(
            prepared.authority_state,
            RequestAuthorityStateV1::Authorized
        );
        let inputs = authority.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].capacity_profile.window_tokens, 8192);
        assert_eq!(inputs[0].capacity_profile.reserve_tokens, 128);
        assert_eq!(
            inputs[0].capacity_profile.catalog_revision.as_deref(),
            Some("catalog-v1")
        );
        assert_eq!(inputs[0].identity, prepared.request_identity.unwrap());
        assert_eq!(inputs[0].draft_body, draft);
    }

    #[test]
    fn changed_continuation_and_fallback_get_fresh_authority_and_profile() {
        let (binding, authority) = binding(false);
        let draft = serde_json::json!({"model": "model"});
        let first = prepare_authorized_request(
            draft.clone(),
            profile("primary"),
            1,
            None,
            Some(&binding),
            false,
        )
        .unwrap();
        let mut changed = draft.clone();
        changed["messages"] = serde_json::json!([{"role": "user", "content": "continue"}]);
        let continuation =
            prepare_authorized_request(changed, profile("primary"), 1, None, Some(&binding), true)
                .unwrap();
        let fallback =
            prepare_authorized_request(draft, profile("fallback"), 1, None, Some(&binding), false)
                .unwrap();
        assert_ne!(first.authorization_id, continuation.authorization_id);
        assert_ne!(first.body_digest, continuation.body_digest);
        assert_ne!(continuation.authorization_id, fallback.authorization_id);
        let inputs = authority.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 3);
        assert!(inputs[1].continuation);
        assert_eq!(inputs[1].identity.generation, 5);
        assert_eq!(inputs[2].capacity_profile.route_id, "fallback");
    }

    #[test]
    fn changed_continuation_without_authority_is_typed_denial() {
        let draft = serde_json::json!({"model": "model"});
        let ordinary =
            prepare_authorized_request(draft.clone(), profile("route"), 1, None, None, false)
                .unwrap();
        assert_eq!(
            ordinary.authority_state,
            RequestAuthorityStateV1::Unavailable
        );
        assert!(ordinary.authorization_id.is_none());
        assert!(ordinary.request_identity.is_none());
        assert!(matches!(
            prepare_authorized_request(draft, profile("route"), 1, None, None, true),
            Err(ProviderError::Authority(
                RequestAuthorityErrorV1::ContinuationReauthorizationRequired
            ))
        ));
    }

    #[test]
    fn changed_draft_rejects_reused_authorization_identity() {
        let (binding, _) = binding(true);
        let draft = serde_json::json!({"model": "model"});
        prepare_authorized_request(
            draft.clone(),
            profile("route"),
            1,
            None,
            Some(&binding),
            false,
        )
        .unwrap();
        assert!(matches!(
            prepare_authorized_request(draft, profile("route"), 1, None, Some(&binding), true),
            Err(ProviderError::Authority(
                RequestAuthorityErrorV1::InvalidResponse { .. }
            ))
        ));
    }

    #[test]
    fn authority_denial_and_final_insertion_over_budget_stop_preparation() {
        struct Deny;
        impl RequestAuthorityV1 for Deny {
            fn authorize(
                &self,
                _: RequestAuthorityInputV1,
            ) -> Result<RequestAuthorityResponseV1, RequestAuthorityErrorV1> {
                Err(RequestAuthorityErrorV1::Denied {
                    reason: "context changed".into(),
                })
            }
        }
        let (binding, _) = binding(false);
        let denied = RequestAuthorityBindingV1::new(binding.identity.clone(), Arc::new(Deny));
        let draft = serde_json::json!({"model": "model"});
        assert!(matches!(
            prepare_authorized_request(
                draft.clone(),
                profile("route"),
                1,
                None,
                Some(&denied),
                false
            ),
            Err(ProviderError::Authority(
                RequestAuthorityErrorV1::Denied { .. }
            ))
        ));
        let tiny = CapacityProfileV1::new(8, 0, "model", "route", None);
        let draft_only =
            prepare_authorized_request(draft.clone(), tiny.clone(), 1, None, None, false);
        assert!(
            draft_only.is_ok(),
            "negative control: draft fits before federation"
        );
        assert!(matches!(
            prepare_authorized_request(draft, tiny, 1, None, Some(&binding), false),
            Err(ProviderError::Capacity(
                CapacityError::FinalInsertionOverBudget { .. }
            ))
        ));
    }
}

/// Stable request metadata supplied by the engine. These values belong on the
/// provider wire request, never in the persisted transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRequestMetadata {
    pub stable_prefix_key: Option<String>,
    pub max_output_tokens: Option<u32>,
    /// HCTX-002: host-state boundary generation used for this request.
    pub provider_boundary_generation: Option<u64>,
    /// HCTX-002: prompt/context/world-state projection fingerprint.
    pub context_fingerprint: Option<String>,
    /// HCTX-003: immutable identity of the tool/schema vector captured for
    /// this exact provider request. Provider adapters carry this metadata for
    /// diagnostics/attribution; it is not serialized as provider content.
    pub tool_catalog_generation: Option<u64>,
    pub tool_catalog_digest: Option<String>,
    /// Receives one notice per transient-failure retry/backoff.
    pub retry_notifier: rightkit_agent_engine_protocol::RetryNotifier,
}

/// OpenAI-compatible streaming client.
#[derive(Clone)]
pub struct OpenAiCompatClient {
    http: rightkit_http::AsyncClient,
    api_key: String,
    direct_auth_header: Option<(String, String)>,
    public_headers: BTreeMap<String, String>,
    direct_transport: bool,
    base_url: String,
    model_id: String,
    /// Reasoning effort hint ("low" | "medium" | "high") for OpenAI-compatible
    /// providers that support a `reasoning` request object.
    effort: Option<String>,
    /// Explicit reasoning-token ceiling. When set, it takes precedence over
    /// effort because it is the operator's concrete budget.
    reasoning_budget_tokens: Option<u32>,
    /// Disable provider-side reasoning for this request.
    reasoning_disabled: bool,
    submission_observer: crate::agent_providers::transport::SubmissionObserver,
    request_authority: Option<RequestAuthorityBindingV1>,
    /// Direct (degraded-context) dispatch: no per-turn authority exists, so an
    /// output-limit continuation is admitted by the capacity gate alone.
    direct_dispatch: bool,
    /// Test seam for the semantic-progress deadline; `None` uses the default.
    semantic_progress_limit: Option<std::time::Duration>,
}

/// Text marker for a failed tool result on wire shapes without an error flag.
pub(crate) const TOOL_ERROR_MARKER: &str = "[tool error] ";

/// Convert the internal `is_error` flag on tool wire messages into the text
/// marker and remove the key (strict chat endpoints reject unknown fields).
/// Per-description byte budget enforced when tool definitions are serialized
/// onto the wire (audit C16). MCP and promoted tools can carry arbitrarily
/// long descriptions; the aggregate cap alone lets one tool crowd the rest.
pub(crate) const MAX_TOOL_DESCRIPTION_BYTES: usize = 2_048;

/// Returns the tool description bounded to [`MAX_TOOL_DESCRIPTION_BYTES`] on a
/// char boundary, with a marker stating the original length. Non-string or
/// missing descriptions become an empty string.
pub(crate) fn bounded_tool_description(function: &serde_json::Value) -> serde_json::Value {
    let text = function
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if text.len() <= MAX_TOOL_DESCRIPTION_BYTES {
        return serde_json::Value::String(text.to_string());
    }
    let mut end = MAX_TOOL_DESCRIPTION_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::Value::String(format!(
        "{} [description truncated: {} bytes total]",
        &text[..end],
        text.len()
    ))
}

pub(crate) fn mark_failed_tool_messages(messages: &mut [serde_json::Value]) {
    for message in messages {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        if object.get("role").and_then(serde_json::Value::as_str) != Some("tool") {
            continue;
        }
        let failed = object.remove("is_error").and_then(|v| v.as_bool()) == Some(true);
        if failed {
            if let Some(text) = object.get("content").and_then(serde_json::Value::as_str) {
                let marked = format!("{TOOL_ERROR_MARKER}{text}");
                object.insert("content".into(), serde_json::Value::String(marked));
            }
        }
    }
}

/// Decodes a JWT segment with the owner's `rightkit_crypto::base64url_decode`, accepting everything the retired
/// hand-rolled decoder accepted: trailing `=` padding or none, either alphabet (`+`/`/` or `-`/`_`), and a last
/// character whose unused low bits are not zero. `None` for a character outside both alphabets or a length that
/// no base64 text can have.
///
/// The owner decoder is strict (canonical form: unused trailing bits must be zero). A short final group is
/// completed with `A` (zero bits) so that the last real character becomes a full group and passes that check,
/// and the bytes that came from the added zeros are dropped again, which is exactly what a lenient decoder
/// returns for the final partial group.
fn decode_jwt_segment(segment: &str) -> Option<Vec<u8>> {
    let mut text = segment
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    let added = match text.len() % 4 {
        0 => 0,
        1 => return None,
        2 => 2,
        _ => 1,
    };
    text.extend(std::iter::repeat_n('A', added));
    let mut bytes = rightkit_crypto::base64url_decode(&text).ok()?;
    bytes.truncate(bytes.len().checked_sub(added)?);
    Some(bytes)
}

/// Extract Codex's non-secret account routing claim (`chatgpt_account_id`) from an access
/// token's JWT payload, without exposing token material to diagnostics or serialized
/// records. The signature is not verified: the value only routes the request.
pub fn openai_account_id_from_token(access: &str) -> Option<String> {
    let payload = access.split('.').nth(1)?;
    let bytes = decode_jwt_segment(payload)?;
    let payload: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    payload
        .get(crate::agent_providers::subscription_auth::OPENAI_JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

impl OpenAiCompatClient {
    pub fn new(
        api_key: impl Into<String>,
        model_id: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let api_key = api_key.into();
        let base_url = base_url.into();
        let mut public_headers = BTreeMap::new();
        if crate::agent_providers::http::base_url_host(&base_url)
            .is_some_and(|host| host.eq_ignore_ascii_case("chatgpt.com"))
        {
            public_headers.insert("originator".into(), "pi".into());
            if let Some(account_id) = openai_account_id_from_token(&api_key) {
                public_headers.insert("chatgpt-account-id".into(), account_id);
            }
        }
        // Without a timeout a hung/slow provider call blocks the engine
        // forever — and for a sub-agent that hangs the whole conductor (the
        // orchestrator waits on the sub's conduit for a terminal event). The
        // shared provider client carries a generous per-request ceiling.
        let http = crate::agent_providers::http::provider_client(
            &base_url,
            crate::agent_providers::http::Route::Ambient,
        );
        Self {
            http,
            api_key,
            direct_auth_header: None,
            public_headers,
            direct_transport: false,
            base_url,
            model_id: model_id.into(),
            effort: None,
            reasoning_budget_tokens: None,
            reasoning_disabled: false,
            submission_observer: Arc::new(|_| {}),
            request_authority: None,
            direct_dispatch: false,
            semantic_progress_limit: None,
        }
    }

    /// Identify one OpenCode conversation without impersonating its client.
    pub fn with_opencode_session(mut self, session_id: &str) -> Self {
        self.public_headers
            .insert("x-opencode-session".into(), session_id.into());
        self
    }

    /// Construct a fail-closed Direct connection. Header names & public
    /// values have already passed daemon policy validation; credentials stay
    /// in this process-memory-only client.
    pub fn new_direct(
        model_id: impl Into<String>,
        base_url: impl Into<String>,
        auth_header: Option<(String, String)>,
        public_headers: BTreeMap<String, String>,
        resolved_addresses: &[std::net::SocketAddr],
    ) -> Self {
        let base_url = base_url.into();
        let mut public_headers = public_headers;
        if crate::agent_providers::http::base_url_host(&base_url)
            .is_some_and(|host| host.eq_ignore_ascii_case("chatgpt.com"))
        {
            public_headers.insert("originator".into(), "pi".into());
            if let Some((name, value)) = &auth_header {
                if name.eq_ignore_ascii_case("authorization") {
                    if let Some(token) = value.strip_prefix("Bearer ") {
                        if let Some(account_id) = openai_account_id_from_token(token) {
                            public_headers.insert("chatgpt-account-id".into(), account_id);
                        }
                    }
                }
            }
        }
        // No proxy, no redirects; the host resolves only to the addresses the
        // daemon already validated.
        let http = crate::agent_providers::http::provider_client(
            &base_url,
            crate::agent_providers::http::Route::Pinned(resolved_addresses),
        );
        Self {
            http,
            api_key: String::new(),
            direct_auth_header: auth_header,
            public_headers,
            direct_transport: true,
            base_url,
            model_id: model_id.into(),
            effort: None,
            reasoning_budget_tokens: None,
            reasoning_disabled: false,
            submission_observer: Arc::new(|_| {}),
            request_authority: None,
            direct_dispatch: false,
            semantic_progress_limit: None,
        }
    }

    /// Set the reasoning effort hint for this client ("low" | "medium" | "high").
    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        let effort = effort.into();
        self.effort = (!effort.trim().is_empty()).then(|| effort.trim().to_ascii_lowercase());
        self
    }

    /// Attach a provider-local sink for truthful physical submission states.
    /// Sink runs after each HTTP attempt, never before admission.
    pub fn with_submission_observer(
        mut self,
        observer: crate::agent_providers::transport::SubmissionObserver,
    ) -> Self {
        self.submission_observer = observer;
        self
    }

    pub fn with_request_authority(mut self, binding: RequestAuthorityBindingV1) -> Self {
        self.request_authority = Some(binding);
        self
    }

    /// Mark this client as direct (degraded-context) dispatch: there is no
    /// per-turn authority, so a continuation is admitted by capacity alone.
    #[cfg(test)]
    pub(crate) fn with_semantic_progress_limit(mut self, limit: std::time::Duration) -> Self {
        self.semantic_progress_limit = Some(limit);
        self
    }

    pub fn with_direct_dispatch(mut self) -> Self {
        self.direct_dispatch = true;
        self
    }

    /// Set an explicit provider-side reasoning token budget.
    pub fn with_reasoning_budget_tokens(mut self, tokens: u32) -> Self {
        self.reasoning_budget_tokens = (tokens > 0).then_some(tokens);
        self
    }

    /// Disable provider-side reasoning/thinking for this client.
    pub fn with_reasoning_disabled(mut self) -> Self {
        self.reasoning_disabled = true;
        self
    }

    fn build_chat_request(
        &self,
        mut wire_messages: Vec<serde_json::Value>,
        mut tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> ChatRequest {
        // Chat-completions has no error flag on tool messages: keep the
        // failure visible as a text marker and drop the non-standard key.
        mark_failed_tool_messages(&mut wire_messages);
        if self.supports_prompt_cache_control() {
            // Wrap system message content as a cacheable array block.
            for msg in wire_messages.iter_mut() {
                if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                    if let Some(text) = msg
                        .get("content")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_string())
                    {
                        msg["content"] = serde_json::json!([{
                            "type": "text",
                            "text": text,
                            "cache_control": {"type": "ephemeral"}
                        }]);
                    }
                }
            }

            // Add cache_control to the last tool definition so the whole tools block is cached.
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = serde_json::json!({"type": "ephemeral"});
            }
        }
        if !self.supports_reasoning_request() {
            for msg in wire_messages.iter_mut() {
                if let Some(obj) = msg.as_object_mut() {
                    obj.remove("reasoning");
                    obj.remove("reasoning_content");
                }
            }
        }

        let parallel_tool_calls =
            (!tools.is_empty() && self.supports_parallel_tool_calls()).then_some(true);

        ChatRequest {
            model: self.model_id.clone(),
            messages: wire_messages,
            tools,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            max_completion_tokens: metadata
                .max_output_tokens
                .or_else(|| self.max_completion_tokens()),
            reasoning: self.reasoning_request(),
            prompt_cache_key: self
                .is_official_openai_api()
                .then(|| metadata.stable_prefix_key.clone())
                .flatten(),
            parallel_tool_calls,
        }
    }

    /// Exact JSON body byte count for the same request builder used by
    /// [`Self::complete_with_tools_metadata`]. This keeps engine-side image
    /// eviction aligned with provider wire serialization as fields evolve.
    pub fn request_body_bytes(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> Result<usize, serde_json::Error> {
        let body = if self.is_codex_responses_route() {
            self.build_codex_request_body(wire_messages, tools, metadata)
        } else {
            serde_json::to_value(self.build_chat_request(wire_messages, tools, metadata))?
        };
        serde_json::to_vec(&body).map(|body| body.len())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// ChatGPT OAuth speaks OpenAI Responses over `/codex/responses`; it is
    /// intentionally kept distinct from ordinary `/chat/completions` routes.
    pub(crate) fn is_codex_responses_route(&self) -> bool {
        url::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| {
                Some(
                    url.host_str()?.eq_ignore_ascii_case("chatgpt.com")
                        && url.path().contains("backend-api"),
                )
            })
            .unwrap_or(false)
    }

    pub(crate) fn build_codex_request_body(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> serde_json::Value {
        let mut instructions = Vec::new();
        let mut input = Vec::new();
        let mut history_started = false;
        for message in wire_messages {
            let role = message.get("role").and_then(serde_json::Value::as_str);
            if role != Some("system") {
                history_started = true;
            }
            if role == Some("system") {
                let mut texts = Vec::new();
                if let Some(content) = message.get("content") {
                    if let Some(text) = content.as_str() {
                        texts.push(text.to_string());
                    } else if let Some(parts) = content.as_array() {
                        texts.extend(parts.iter().filter_map(|part| {
                            part.get("text")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        }));
                    }
                }
                if history_started {
                    // A system message AFTER conversation history (for example a
                    // late skill hint) must stay where it occurred. Hoisting it
                    // into `instructions` rewrites the very start of the prompt
                    // and invalidates the entire provider prompt cache for every
                    // later turn.
                    for text in texts {
                        input.push(serde_json::json!({
                            "role": "developer",
                            "content": [{"type": "input_text", "text": text}],
                        }));
                    }
                } else {
                    instructions.extend(texts);
                }
                continue;
            }
            if role == Some("tool") {
                let mut output = message
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                if message.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
                    if let Some(text) = output.as_str() {
                        output = serde_json::Value::String(format!("{TOOL_ERROR_MARKER}{text}"));
                    }
                }
                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": message.get("tool_call_id").cloned().unwrap_or_default(),
                    "output": output,
                }));
                continue;
            }
            // Chat-shaped assistant tool calls are represented as explicit
            // Responses input items. Sending the original `tool_calls`
            // property to `/codex/responses` is not a compatible wire shape.
            if role == Some("assistant") {
                if let Some(tool_calls) = message
                    .get("tool_calls")
                    .and_then(serde_json::Value::as_array)
                {
                    if message
                        .get("content")
                        .is_some_and(|content| !content.is_null())
                    {
                        input.push(serde_json::json!({
                            "role": "assistant",
                            "content": message.get("content").cloned().unwrap_or_default(),
                        }));
                    }
                    for call in tool_calls {
                        let function = call.get("function");
                        let mut function_call = serde_json::json!({
                            "type": "function_call",
                            "call_id": call.get("id").cloned().unwrap_or_default(),
                            "name": function.and_then(|value| value.get("name")).cloned().unwrap_or_default(),
                            "arguments": function.and_then(|value| value.get("arguments")).cloned().unwrap_or_else(|| serde_json::Value::String("{}".to_string())),
                        });
                        if let Some(item_id) = call.get("item_id").cloned() {
                            function_call["id"] = item_id;
                        }
                        input.push(function_call);
                    }
                    continue;
                }
            }
            if role == Some("user") {
                let content = message.get("content").cloned().unwrap_or_default();
                let content = if let Some(text) = content.as_str() {
                    serde_json::json!([{"type":"input_text","text":text}])
                } else if let Some(parts) = content.as_array() {
                    serde_json::Value::Array(
                        parts
                            .iter()
                            .filter_map(|part| {
                                if let Some(text) =
                                    part.get("text").and_then(serde_json::Value::as_str)
                                {
                                    return Some(
                                        serde_json::json!({"type":"input_text","text":text}),
                                    );
                                }
                                let image = part.get("image_url")?;
                                Some(serde_json::json!({
                                    "type":"input_image",
                                    "detail":"auto",
                                    "image_url": image.get("url").cloned().unwrap_or_default(),
                                }))
                            })
                            .collect(),
                    )
                } else {
                    serde_json::json!([])
                };
                if content.as_array().is_some_and(|items| !items.is_empty()) {
                    input.push(serde_json::json!({"role":"user","content":content}));
                }
                continue;
            }
            if role == Some("assistant") {
                let content = message.get("content").cloned().unwrap_or_default();
                if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
                    input.push(serde_json::json!({
                        "type":"message",
                        "role":"assistant",
                        "content":[{"type":"output_text","text":text,"annotations":[]}],
                        "status":"completed",
                    }));
                }
                continue;
            }
            input.push(message);
        }
        let mut body = serde_json::json!({
            "model": self.model_id,
            "store": false,
            "stream": true,
            "instructions": if instructions.is_empty() { "You are a helpful assistant.".to_string() } else { instructions.join("\n\n") },
            "input": input,
            "text": {"verbosity": "low"},
            "include": ["reasoning.encrypted_content"],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
        });
        if let Some(key) = &metadata.stable_prefix_key {
            body["prompt_cache_key"] = serde_json::Value::String(key.clone());
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(
                tools
                    .into_iter()
                    .filter_map(|tool| {
                        let function = tool.get("function")?;
                        Some(serde_json::json!({
                            "type": "function",
                            "name": function.get("name").cloned().unwrap_or_default(),
                            "description": bounded_tool_description(function),
                            "parameters": function.get("parameters").cloned().unwrap_or(serde_json::json!({})),
                            "strict": function.get("strict").cloned().unwrap_or(serde_json::Value::Null),
                        }))
                    })
                    .collect(),
            );
        }
        body["reasoning"] = match self.effort.as_deref() {
            Some(effort) => serde_json::json!({"effort": effort, "summary": "auto"}),
            // Reasoning models still stream summaries when only they are asked for.
            None => serde_json::json!({"summary": "auto"}),
        };
        body
    }

    fn is_official_openai_api(&self) -> bool {
        crate::agent_providers::http::base_url_host(&self.base_url)
            .is_some_and(|host| host.eq_ignore_ascii_case("api.openai.com"))
    }

    fn max_completion_tokens(&self) -> Option<u32> {
        None
    }

    /// `cache_control` is an Anthropic-style vendor extension honoured by
    /// OpenRouter and similar gateways. First-party OpenAI caches prefixes
    /// automatically and strictly validates request bodies, so the marker is
    /// omitted there (it would be an unknown parameter).
    fn supports_prompt_cache_control(&self) -> bool {
        !self.is_official_openai_api()
    }

    fn reasoning_request(&self) -> Option<ReasoningRequest> {
        if !self.supports_reasoning_request() {
            return None;
        }
        if self.reasoning_disabled {
            return Some(ReasoningRequest {
                effort: Some("none".to_string()),
                max_tokens: None,
            });
        }
        if let Some(tokens) = self.reasoning_budget_tokens {
            return Some(ReasoningRequest {
                effort: None,
                max_tokens: Some(tokens),
            });
        }
        self.effort.as_ref().map(|effort| ReasoningRequest {
            effort: Some(effort.clone()),
            max_tokens: None,
        })
    }

    fn supports_reasoning_request(&self) -> bool {
        true
    }

    /// CR-10: whether this route's wire protocol recognizes
    /// `parallel_tool_calls`. True for every OpenAI-compatible route today
    /// (the official API and every currently-supported OpenAI-compatible
    /// gateway accept the field); kept as a method rather than a constant
    /// `true` inline at the call site so a future route that rejects the
    /// field has one place to override it, the same pattern used by
    /// `supports_prompt_cache_control`/`supports_reasoning_request`.
    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    #[cfg(test)]
    fn request_body_for_test(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        self.request_body_with_metadata_for_test(
            wire_messages,
            tools,
            &ProviderRequestMetadata::default(),
        )
    }

    #[cfg(test)]
    fn request_body_with_metadata_for_test(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> serde_json::Value {
        if self.is_codex_responses_route() {
            self.build_codex_request_body(wire_messages, tools, metadata)
        } else {
            serde_json::to_value(self.build_chat_request(wire_messages, tools, metadata)).unwrap()
        }
    }
}

#[cfg(test)]
mod retry_after_tests {
    use super::retry::{retry_after_secs, MAX_RETRY_AFTER_SECS};
    use super::OpenAiCompatClient;
    use std::collections::BTreeMap;

    fn response_with(
        status: u16,
        retry_after: Option<&str>,
    ) -> crate::agent_providers::http::ProviderHttpResponse {
        let headers: Vec<(&str, &str)> = retry_after
            .map(|v| ("retry-after", v))
            .into_iter()
            .collect();
        crate::agent_providers::http::ProviderHttpResponse::from_parts(status, &headers, b"")
    }

    #[test]
    fn honors_retry_after_seconds_on_429() {
        let resp = response_with(429, Some("2"));
        assert_eq!(retry_after_secs(&resp), Some(2));
    }

    #[test]
    fn caps_retry_after_at_60_seconds() {
        let resp = response_with(429, Some("9999"));
        assert_eq!(retry_after_secs(&resp), Some(MAX_RETRY_AFTER_SECS));
    }

    #[test]
    fn no_retry_after_header_returns_none() {
        let resp = response_with(429, None);
        assert_eq!(retry_after_secs(&resp), None);
    }

    #[test]
    fn non_429_status_returns_none_even_with_header() {
        let resp = response_with(503, Some("5"));
        assert_eq!(retry_after_secs(&resp), None);
    }

    #[test]
    fn unparseable_retry_after_returns_none() {
        let resp = response_with(429, Some("not-a-number"));
        assert_eq!(retry_after_secs(&resp), None);
    }

    #[test]
    fn direct_request_uses_only_validated_auth_and_public_headers() {
        let client = OpenAiCompatClient::new_direct(
            "model",
            "https://direct.example/v1",
            Some(("Authorization".into(), "Bearer sentinel".into())),
            BTreeMap::from([("x-tenant".into(), "workspace".into())]),
            &["127.0.0.1:443".parse().unwrap()],
        );
        let request = client.request_builder("https://direct.example/v1/chat/completions");
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(header("authorization"), Some("Bearer sentinel"));
        assert_eq!(header("x-tenant"), Some("workspace"));
        assert!(header("http-referer").is_none());
    }
}

#[cfg(test)]
mod reasoning_request_tests {
    use super::{
        is_incomplete_finish_reason, validate_complete_tool_calls, OpenAiCompatClient,
        ProviderRequestMetadata,
    };

    fn user_message() -> serde_json::Value {
        serde_json::json!({"role": "user", "content": "think carefully"})
    }

    #[test]
    fn effort_serializes_as_reasoning_object_not_legacy_top_level_effort() {
        let client =
            OpenAiCompatClient::new("key", "model", "http://127.0.0.1").with_effort("HIGH");

        let body = client.request_body_for_test(vec![user_message()], Vec::new());

        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("effort").is_none(), "{body}");
    }

    #[test]
    fn explicit_reasoning_budget_overrides_effort() {
        let client = OpenAiCompatClient::new("key", "model", "http://127.0.0.1")
            .with_effort("high")
            .with_reasoning_budget_tokens(2048);

        let body = client.request_body_for_test(vec![user_message()], Vec::new());

        assert_eq!(body["reasoning"]["max_tokens"], 2048);
        assert!(body["reasoning"].get("effort").is_none(), "{body}");
    }

    #[test]
    fn disable_thinking_serializes_reasoning_none() {
        let client = OpenAiCompatClient::new("key", "model", "http://127.0.0.1")
            .with_effort("high")
            .with_reasoning_disabled();

        let body = client.request_body_for_test(vec![user_message()], Vec::new());

        assert_eq!(body["reasoning"]["effort"], "none");
        assert!(body["reasoning"].get("max_tokens").is_none(), "{body}");
    }

    #[test]
    fn completion_tokens_are_uncapped_by_default() {
        let client = OpenAiCompatClient::new("key", "model", "https://openrouter.ai/api/v1");

        let body = client.request_body_for_test(vec![user_message()], Vec::new());

        assert!(body.get("max_completion_tokens").is_none(), "{body}");
    }

    #[test]
    fn output_limit_finish_reasons_are_never_treated_as_complete() {
        for reason in ["length", "max_tokens", "max_output_tokens", "token_limit"] {
            assert!(is_incomplete_finish_reason(reason), "{reason}");
        }
        for reason in ["stop", "tool_calls", "end_turn"] {
            assert!(!is_incomplete_finish_reason(reason), "{reason}");
        }
    }

    #[test]
    fn malformed_or_duplicate_tool_calls_are_rejected_before_dispatch() {
        // Adapted-to-Rust from Gemini CLI local-executor.test.ts
        // `processFunctionCalls`: each dispatchable invocation needs one,
        // unique call ID; the host app checks this at provider boundary.
        let duplicate = vec![
            rightkit_agent_engine_protocol::ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments_json: "{}".into(),
            },
            rightkit_agent_engine_protocol::ToolCall {
                id: "call-1".into(),
                name: "write_file".into(),
                arguments_json: "{}".into(),
            },
        ];
        assert!(validate_complete_tool_calls(&duplicate).is_err());
        let malformed = vec![rightkit_agent_engine_protocol::ToolCall {
            id: "call-2".into(),
            name: "read_file".into(),
            arguments_json: "{\"path\":".into(),
        }];
        assert!(validate_complete_tool_calls(&malformed).is_err());

        let complete = vec![
            rightkit_agent_engine_protocol::ToolCall {
                id: "call-3".into(),
                name: "read_file".into(),
                arguments_json: r#"{"path":"Cargo.toml"}"#.into(),
            },
            rightkit_agent_engine_protocol::ToolCall {
                id: "call-4".into(),
                name: "write_file".into(),
                arguments_json: r#"{"path":"notes.txt","content":"ok"}"#.into(),
            },
        ];
        assert!(validate_complete_tool_calls(&complete).is_ok());
    }

    #[test]
    fn request_byte_accounting_matches_exact_wire_serializer() {
        let client = OpenAiCompatClient::new("key", "model", "https://openrouter.ai/api/v1");
        let messages = vec![user_message()];
        let metadata = ProviderRequestMetadata::default();
        let expected =
            serde_json::to_vec(&client.build_chat_request(messages.clone(), Vec::new(), &metadata))
                .unwrap()
                .len();

        assert_eq!(
            client
                .request_body_bytes(messages, Vec::new(), &metadata)
                .unwrap(),
            expected
        );
    }

    #[test]
    fn codex_route_uses_responses_shape_not_chat_completions_shape() {
        let client = OpenAiCompatClient::new("token", "gpt-5.6", "https://chatgpt.com/backend-api");
        assert!(client.is_codex_responses_route());
        let body = client.request_body_for_test(
            vec![
                serde_json::json!({"role":"system","content":"Be concise."}),
                serde_json::json!({"role":"user","content":"hello"}),
            ],
            vec![serde_json::json!({"type":"function","function":{"name":"read_file","description":"Read","parameters":{"type":"object"}}})],
        );
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], "Be concise.");
        assert!(
            body.get("messages").is_none(),
            "Codex must not receive chat-completions messages"
        );
        let input = body["input"].as_array().expect("input must be an array");
        assert_eq!(
            input.len(),
            1,
            "input holds only the user turn — system moved to instructions"
        );
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert_eq!(body["parallel_tool_calls"], true);
    }

    /// CR-10: `parallel_tool_calls: true` must actually reach the wire when
    /// tools are present on a capability-compatible route — the only prior
    /// occurrence of `parallel_tool_calls` in this repo was a test name in
    /// `reassembler.rs`, never a serialized field.
    #[test]
    fn parallel_tool_calls_true_is_sent_when_tools_are_present() {
        let client = OpenAiCompatClient::new("key", "model", "https://openrouter.ai/api/v1");
        let body = client.request_body_for_test(
            vec![user_message()],
            vec![serde_json::json!({
                "type": "function",
                "function": {"name": "read_file", "description": "read", "parameters": {"type": "object"}}
            })],
        );
        assert_eq!(
            body["parallel_tool_calls"], true,
            "actual wire body: {body}"
        );
    }

    /// CR-10: the field must be omitted (not `false`) on a turn with no
    /// tools — there is nothing to run in parallel, and some OpenAI-
    /// compatible backends reject an unrecognized/irrelevant value.
    #[test]
    fn parallel_tool_calls_is_omitted_when_no_tools_are_offered() {
        let client = OpenAiCompatClient::new("key", "model", "https://openrouter.ai/api/v1");
        let body = client.request_body_for_test(vec![user_message()], Vec::new());
        assert!(
            body.get("parallel_tool_calls").is_none(),
            "actual wire body: {body}"
        );
    }
}

#[cfg(test)]
mod prompt_cache_control_tests {
    use super::{OpenAiCompatClient, ProviderRequestMetadata};

    fn system_message() -> serde_json::Value {
        serde_json::json!({"role": "system", "content": "stable instructions"})
    }

    fn user_message() -> serde_json::Value {
        serde_json::json!({"role": "user", "content": "hello"})
    }

    fn tool_def() -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "read",
                "parameters": {"type": "object"}
            }
        })
    }

    #[test]
    fn prompt_cache_control_is_added_by_default() {
        let client = OpenAiCompatClient::new("key", "model", "https://openrouter.ai/api/v1");
        let body =
            client.request_body_for_test(vec![system_message(), user_message()], vec![tool_def()]);

        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn official_openai_request_receives_stable_prompt_cache_key() {
        let client = OpenAiCompatClient::new("key", "gpt-5", "https://api.openai.com/v1");
        let metadata = ProviderRequestMetadata {
            stable_prefix_key: Some("session-prefix-v1".to_string()),
            max_output_tokens: None,
            ..ProviderRequestMetadata::default()
        };

        let body = client.request_body_with_metadata_for_test(
            vec![system_message(), user_message()],
            vec![tool_def()],
            &metadata,
        );

        assert_eq!(body["prompt_cache_key"], "session-prefix-v1");
        // Automatic prefix caching: no Anthropic-style markers on first-party OpenAI.
        assert!(body["messages"][0]["content"].is_string(), "{body}");
        assert!(body["tools"][0].get("cache_control").is_none(), "{body}");
    }

    #[test]
    fn codex_late_system_message_stays_in_place_and_keeps_instructions_stable() {
        let client =
            OpenAiCompatClient::new("key", "gpt-6.1-sol", "https://chatgpt.com/backend-api");
        let metadata = ProviderRequestMetadata::default();
        let turn1 = client.request_body_with_metadata_for_test(
            vec![system_message(), user_message()],
            vec![tool_def()],
            &metadata,
        );
        // Turn 2: a skill hint was appended mid-conversation.
        let turn2 = client.request_body_with_metadata_for_test(
            vec![
                system_message(),
                user_message(),
                serde_json::json!({"role": "assistant", "content": "hi"}),
                serde_json::json!({"role": "system", "content": "Triggered local skills"}),
                serde_json::json!({"role": "user", "content": "again"}),
            ],
            vec![tool_def()],
            &metadata,
        );
        assert_eq!(turn1["instructions"], turn2["instructions"]);
        assert_eq!(turn1["input"][0], turn2["input"][0]);
        assert_eq!(turn2["input"][2]["role"], "developer");
        assert_eq!(
            turn2["input"][2]["content"][0]["text"],
            "Triggered local skills"
        );
    }

    #[test]
    fn chat_request_prefix_is_identical_across_turns() {
        // Cacheability: the leading system+tools bytes must not change when
        // only the conversation grows.
        let client = OpenAiCompatClient::new("key", "gpt-5", "https://api.openai.com/v1");
        let first =
            client.request_body_for_test(vec![system_message(), user_message()], vec![tool_def()]);
        let second = client.request_body_for_test(
            vec![
                system_message(),
                user_message(),
                serde_json::json!({"role": "assistant", "content": "hi"}),
                serde_json::json!({"role": "user", "content": "again"}),
            ],
            vec![tool_def()],
        );
        assert_eq!(first["tools"], second["tools"]);
        assert_eq!(first["messages"][0], second["messages"][0]);
        assert_eq!(first["messages"][1], second["messages"][1]);
    }

    #[test]
    fn codex_request_carries_prompt_cache_key_and_stable_instructions_prefix() {
        let client =
            OpenAiCompatClient::new("key", "gpt-6.1-sol", "https://chatgpt.com/backend-api");
        let metadata = ProviderRequestMetadata {
            stable_prefix_key: Some("session-prefix-v1".to_string()),
            ..ProviderRequestMetadata::default()
        };
        let first = client.request_body_with_metadata_for_test(
            vec![system_message(), user_message()],
            vec![tool_def()],
            &metadata,
        );
        let second = client.request_body_with_metadata_for_test(
            vec![
                system_message(),
                user_message(),
                serde_json::json!({"role": "assistant", "content": "hi"}),
                serde_json::json!({"role": "user", "content": "again"}),
            ],
            vec![tool_def()],
            &metadata,
        );
        assert_eq!(first["prompt_cache_key"], "session-prefix-v1");
        assert_eq!(first["instructions"], second["instructions"]);
        assert_eq!(first["tools"], second["tools"]);
        assert_eq!(first["input"][0], second["input"][0]);
        let headers = super::transport::codex_cache_affinity_headers_for_test(&metadata);
        assert_eq!(
            headers
                .iter()
                .find(|(name, _)| name == "session_id")
                .map(|(_, value)| value.as_str()),
            Some("session-prefix-v1")
        );
    }

    #[test]
    fn compatible_provider_does_not_receive_openai_only_cache_key() {
        let client = OpenAiCompatClient::new("key", "model", "https://openrouter.ai/api/v1");
        let metadata = ProviderRequestMetadata {
            stable_prefix_key: Some("session-prefix-v1".to_string()),
            max_output_tokens: None,
            ..ProviderRequestMetadata::default()
        };

        let body = client.request_body_with_metadata_for_test(
            vec![system_message(), user_message()],
            vec![tool_def()],
            &metadata,
        );

        assert!(body.get("prompt_cache_key").is_none(), "{body}");
    }

    #[test]
    fn request_metadata_enforces_per_call_output_ceiling() {
        let client = OpenAiCompatClient::new("key", "model", "https://openrouter.ai/api/v1");
        let metadata = ProviderRequestMetadata {
            stable_prefix_key: None,
            max_output_tokens: Some(32_768),
            ..ProviderRequestMetadata::default()
        };
        let body = client.request_body_with_metadata_for_test(
            vec![system_message(), user_message()],
            vec![],
            &metadata,
        );
        assert_eq!(body["max_completion_tokens"], 32_768);
    }
}

#[cfg(test)]
mod cq06_measure_tests {
    use super::capacity::measure_o200k_base;

    /// CQ-06: the shared OnceLock BPE must return byte-identical counts to a
    /// freshly constructed encoder — reuse changes cost, not semantics.
    /// (Measured during the fix: ~1.22 s/call constructing the rank table per
    /// call vs ~12.8 ms/call cached on a 6.4 KB body.)
    #[test]
    fn admit_counts_each_inline_image_once_not_its_base64_bytes() {
        use super::capacity::*;
        let b64 = "QUJD".repeat(50_000);
        let body = serde_json::json!({"messages":[{"content":[
            {"type":"image_url","image_url":{"url":format!("data:image/png;base64,{b64}")}},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":b64}}
        ]}]})
        .to_string();
        let (projected, count) = project_inline_images(&body);
        assert_eq!(count, 2);
        assert!(!projected.contains("QUJDQUJD"));
        let measured = measure_image_projection(&projected, count);
        assert!(measured < 2 * IMAGE_TOKEN_COST + 200, "measured {measured}");
    }

    #[test]
    fn tool_description_is_bounded_at_serialization_on_a_char_boundary() {
        use super::{bounded_tool_description, MAX_TOOL_DESCRIPTION_BYTES};
        let long = "é".repeat(MAX_TOOL_DESCRIPTION_BYTES);
        let function = serde_json::json!({"description": long});
        let bounded = bounded_tool_description(&function);
        let text = bounded.as_str().unwrap();
        assert!(text.len() < MAX_TOOL_DESCRIPTION_BYTES + 80);
        assert!(text.contains("description truncated"));
        let short = bounded_tool_description(&serde_json::json!({"description": "read"}));
        assert_eq!(short, "read");
    }

    #[test]
    fn measure_o200k_base_cached_matches_fresh_construction() {
        let text = "fn main() { println!(\"hello\"); }".repeat(200);
        let fresh = tiktoken_rs::o200k_base()
            .unwrap()
            .encode_ordinary(&text)
            .len() as u64;
        assert_eq!(measure_o200k_base(&text), fresh);
        assert_eq!(
            measure_o200k_base(&text),
            fresh,
            "second call reuses the shared BPE"
        );
    }
}

#[cfg(test)]
mod tool_error_fidelity_tests {
    use super::*;

    #[test]
    fn failed_tool_message_is_marked_and_flag_key_is_stripped_for_chat() {
        let mut messages = vec![
            serde_json::json!({"role":"tool","tool_call_id":"a","content":"boom","is_error":true}),
            serde_json::json!({"role":"tool","tool_call_id":"b","content":"fine"}),
        ];
        mark_failed_tool_messages(&mut messages);
        assert_eq!(messages[0]["content"], "[tool error] boom");
        assert!(messages[0].get("is_error").is_none());
        assert_eq!(messages[1]["content"], "fine");
    }
}

#[cfg(test)]
mod jwt_account_id_tests {
    use super::{decode_jwt_segment, openai_account_id_from_token};

    fn token_with(payload: &serde_json::Value) -> String {
        let body = rightkit_crypto::base64url_encode(payload.to_string().as_bytes());
        format!("e30.{body}.sig")
    }

    #[test]
    fn reads_the_account_id_claim_through_the_owner_decoder() {
        let payload = serde_json::json!({
            (crate::agent_providers::subscription_auth::OPENAI_JWT_CLAIM_PATH): { "chatgpt_account_id": "acct_123" }
        });
        assert_eq!(
            openai_account_id_from_token(&token_with(&payload)).as_deref(),
            Some("acct_123")
        );
    }

    /// Inputs the retired hand-rolled decoder accepted: padded base64url, the standard alphabet and
    /// a final character with non-zero unused bits must still decode.
    #[test]
    fn keeps_accepting_padded_standard_alphabet_and_noncanonical_segments() {
        // Pick a payload whose base64url text contains `-` or `_` (so the standard alphabet differs)
        // and whose length is not a multiple of 4 (so padding and unused bits exist).
        let url = (1..64)
            .map(|n| {
                serde_json::json!({
                    (crate::agent_providers::subscription_auth::OPENAI_JWT_CLAIM_PATH): {
                        "chatgpt_account_id": "acct_123",
                        "pad": "?>".repeat(n)
                    }
                })
            })
            .map(|payload| rightkit_crypto::base64url_encode(payload.to_string().as_bytes()))
            .find(|text| (text.contains('-') || text.contains('_')) && text.len() % 4 != 0)
            .expect("a fixture with both properties exists");
        // Padded.
        let mut padded = url.clone();
        while padded.len() % 4 != 0 {
            padded.push('=');
        }
        assert_eq!(
            openai_account_id_from_token(&format!("h.{padded}.s")).as_deref(),
            Some("acct_123")
        );
        // Standard alphabet (the "?>?>" run produces `/` and `+` positions).
        let std_alpha = url.replace('-', "+").replace('_', "/");
        assert_ne!(std_alpha, url, "fixture must exercise the +/ alphabet");
        assert_eq!(
            openai_account_id_from_token(&format!("h.{std_alpha}.s")).as_deref(),
            Some("acct_123")
        );
        // Non-canonical last character: set the unused low bits.
        const URL: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut noncanonical = url.clone();
        let tail_bits = match noncanonical.len() % 4 {
            2 => Some(0b00_1111u8),
            3 => Some(0b00_0011u8),
            _ => None,
        };
        if let Some(bits) = tail_bits {
            let last = noncanonical.pop().unwrap();
            let value = URL.find(last).unwrap() as u8 | bits;
            noncanonical.push(URL.as_bytes()[value as usize] as char);
            assert_ne!(noncanonical, url);
            assert!(
                rightkit_crypto::base64url_decode(&noncanonical).is_err(),
                "the owner alone is strict"
            );
        }
        assert_eq!(
            openai_account_id_from_token(&format!("h.{noncanonical}.s")).as_deref(),
            Some("acct_123")
        );
    }

    /// The segment decoder itself, over every payload length 1..=40 bytes (so every final-group shape): padded,
    /// standard-alphabet and non-canonical-tail spellings decode to the original bytes, and the shapes no base64 text
    /// can have, or characters outside both alphabets, are refused.
    #[test]
    fn jwt_segment_decoder_accepts_the_retired_spellings_and_refuses_the_rest() {
        const URL: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        for n in 1..=40usize {
            let bytes: Vec<u8> = (0..n)
                .map(|i| (i as u8).wrapping_mul(37).wrapping_add(n as u8) ^ 0xA5)
                .collect();
            let canonical = rightkit_crypto::base64url_encode(&bytes);
            assert_eq!(
                decode_jwt_segment(&canonical).unwrap(),
                bytes,
                "canonical, {n} bytes"
            );
            let mut padded = canonical.clone();
            while padded.len() % 4 != 0 {
                padded.push('=');
            }
            assert_eq!(
                decode_jwt_segment(&padded).unwrap(),
                bytes,
                "padded, {n} bytes"
            );
            let standard = canonical.replace('-', "+").replace('_', "/");
            assert_eq!(
                decode_jwt_segment(&standard).unwrap(),
                bytes,
                "standard alphabet, {n} bytes"
            );
            // Set every unused low bit of the last character: the owner alone refuses it, the segment decoder keeps it.
            let unused = match canonical.len() % 4 {
                2 => 0b00_1111u8,
                3 => 0b00_0011u8,
                _ => 0,
            };
            if unused != 0 {
                let mut tail = canonical.clone();
                let last = tail.pop().unwrap();
                tail.push(
                    URL.as_bytes()[(URL.find(last).unwrap() as u8 | unused) as usize] as char,
                );
                assert!(
                    rightkit_crypto::base64url_decode(&tail).is_err(),
                    "the owner alone is strict, {n} bytes"
                );
                assert_eq!(
                    decode_jwt_segment(&tail).unwrap(),
                    bytes,
                    "non-canonical tail, {n} bytes"
                );
            }
        }
        // Fixed vector over the two characters the alphabets disagree on: 0xfb 0xff is `-_8` (URL-safe) and `+/8=`
        // (standard), 0xfb 0xff 0xbf is `-_-_` and `+/+/`. Every spelling, and a mix of the two alphabets, decodes.
        for text in ["-_8", "+/8", "+/8=", "-/8", "+_8==", "-_8="] {
            assert_eq!(decode_jwt_segment(text), Some(vec![0xfb, 0xff]), "{text}");
        }
        for text in ["-_-_", "+/+/", "+_-/", "-_-_="] {
            assert_eq!(
                decode_jwt_segment(text),
                Some(vec![0xfb, 0xff, 0xbf]),
                "{text}"
            );
        }
        assert_eq!(decode_jwt_segment(""), Some(Vec::new()));
        assert_eq!(
            decode_jwt_segment("A"),
            None,
            "one character is not a base64 length"
        );
        assert_eq!(decode_jwt_segment("AAAAA"), None);
        assert_eq!(
            decode_jwt_segment("aGk!"),
            None,
            "a character outside both alphabets"
        );
        assert_eq!(decode_jwt_segment("aG=k"), None, "padding inside the text");
    }

    #[test]
    fn malformed_or_claimless_tokens_yield_none() {
        assert_eq!(openai_account_id_from_token("not-a-jwt"), None);
        assert_eq!(openai_account_id_from_token("a.!!!.c"), None);
        assert_eq!(
            openai_account_id_from_token(&token_with(&serde_json::json!({"sub": "x"}))),
            None
        );
    }
}
