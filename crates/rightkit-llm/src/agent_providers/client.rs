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
}
