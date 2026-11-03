//! Image generation client and image-route catalogue (Orthic-Labs/rightkit#663).
//!
//! [`ImageGenerationClient::generate`] posts to `{base}/images/generations` (the OpenAI images
//! route) with a bearer credential from a [`CredentialSource`]. The response body is bounded by
//! [`ImageGenerationClient::with_max_response_bytes`]. A base64 result and a URL result are
//! kept distinct as [`ImageResult::Base64`] and [`ImageResult::Url`]; the crate does not turn
//! one into the other.
//!
//! UNVERIFIED: the request and response field names (`model`, `prompt`, `n`, `size`,
//! `response_format`, `data[].b64_json`, `data[].url`, `data[].revised_prompt`) are the OpenAI
//! images API as commonly documented. The issue names the route only, and CodeRight's client
//! source (the reference shape) is not in this repository, so these were not checked against
//! it or the live API. Confirm them against CodeRight's `image_registry.rs` before relying on
//! this client.
//!
//! [`ImageRouteCatalogue`] lists image routes with sizes, formats, priority and a per-image USD
//! price, loaded from JSON. A TOML loader is not included: it needs a TOML parser dependency
//! that this crate does not declare.

use std::sync::Arc;

use rightkit_http::{Client, Request};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::attempt::{observe_attempts, AttemptNotice};
use crate::credentials::CredentialSource;
use crate::error::LlmError;

/// Default cap on the image response body: 16 MiB.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// One image request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    /// Size such as `1024x1024`. Omitted from the body when `None`.
    pub size: Option<String>,
    /// Ask for a URL result instead of base64. Default `false`.
    pub want_url: bool,
    /// Provider quality tier such as `low`, `medium`, `high` or `auto`. Omitted when `None`.
    pub quality: Option<String>,
    /// Encoded output format such as `png`, `jpeg` or `webp`. Omitted when `None`.
    pub output_format: Option<String>,
}

impl ImageRequest {
    pub fn new(model: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            prompt: prompt.into(),
            size: None,
            want_url: false,
            quality: None,
            output_format: None,
        }
    }

    pub fn with_size(mut self, size: impl Into<String>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Set the provider quality tier (#1004). Sent as `quality` only when set.
    pub fn with_quality(mut self, quality: impl Into<String>) -> Self {
        self.quality = Some(quality.into());
        self
    }

    /// Set the encoded output format (#1004). Sent as `output_format` only when set.
    pub fn with_output_format(mut self, format: impl Into<String>) -> Self {
        self.output_format = Some(format.into());
        self
    }

    pub fn url_result(mut self) -> Self {
        self.want_url = true;
        self
    }
}

/// The image as the provider returned it. Base64 is the bare encoded bytes, without a
/// `data:` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageResult {
    Base64(String),
    Url(String),
}

/// A generated image and the provider's revised prompt, when it gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    pub result: ImageResult,
    pub revised_prompt: Option<String>,
}

/// Client for the images route of one OpenAI-compatible endpoint.
pub struct ImageGenerationClient {
    id: String,
    base_url: String,
    client: Client,
    credentials: Arc<dyn CredentialSource>,
    max_response_bytes: usize,
}

impl std::fmt::Debug for ImageGenerationClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential source and HTTP client are omitted: neither prints a key.
        f.debug_struct("ImageGenerationClient")
            .field("id", &self.id)
            .field("base_url", &redact_url(&self.base_url))
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl ImageGenerationClient {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        client: Client,
        credentials: Arc<dyn CredentialSource>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            client,
            credentials,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    /// Cap the response body at `limit` bytes. A larger answer fails with the HTTP layer's
    /// size error. GenRight's `gpt-image-2` `b64_json` answers need up to 256 MiB.
    pub fn with_max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = limit;
        self
    }

    /// Report every physical attempt of this client to `observer` (#658 surface on the images
    /// route): a [`crate::attempt::SubmissionReceipt`] per attempt and a retry notice when
    /// another attempt follows. Call it after building the client; it replaces an observer the
    /// client already had.
    pub fn with_attempt_observer(
        mut self,
        observer: impl Fn(&AttemptNotice<'_>) + Send + Sync + 'static,
    ) -> Self {
        self.client = observe_attempts(self.client, observer);
        self
    }

    /// Check the credential against the endpoint without generating an image. The images route
    /// has no free call, so this lists models (`GET {base}/models`), as
    /// [`crate::openai::OpenAiCompatProvider::validate_credential`] does. A 401 or 403 is
    /// [`LlmError::Auth`]; a missing credential is [`LlmError::MissingCredential`]. Nothing
    /// generated is billed.
    pub fn probe_key(&self) -> Result<(), LlmError> {
        let credential = self
            .credentials
            .credential(&self.id)?
            .ok_or_else(|| LlmError::MissingCredential(self.id.clone()))?;
        let secret = credential.expose().to_string();
        let http = Request::get(format!("{}/models", self.base_url.trim_end_matches('/')))
            .header("Accept", "application/json")
            .bearer(secret.as_str());
        let resp = self.client.send(&http).map_err(|e| {
            match LlmError::from_http(e, Some(secret.as_str())) {
                LlmError::Status { status, message } if matches!(status, 401 | 403) => {
                    LlmError::Auth { status, message }
                }
                other => other,
            }
        })?;
        resp.json::<Value>()
            .map(|_| ())
            .map_err(|e| LlmError::Malformed(e.to_string()))
    }

    /// As [`ImageGenerationClient::generate`], with the failure classed for the caller's retry
    /// and billing decisions (see [`ImageFailureClass`]).
    pub fn generate_classified(&self, req: &ImageRequest) -> Result<GeneratedImage, ImageFailure> {
        self.generate(req).map_err(|error| ImageFailure {
            class: classify_image_error(&error),
            error,
        })
    }

    /// Generate one image. A missing credential is [`LlmError::MissingCredential`] and is
    /// reported before anything is sent.
    pub fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage, LlmError> {
        if req.model.trim().is_empty() || req.prompt.trim().is_empty() {
            return Err(LlmError::InvalidRequest(
                "image request needs a model and a prompt".into(),
            ));
        }
        check_result_format(req)?;
        let credential = self
            .credentials
            .credential(&self.id)?
            .ok_or_else(|| LlmError::MissingCredential(self.id.clone()))?;
        let secret = credential.expose().to_string();
        let http = Request::post(format!(
            "{}/images/generations",
            self.base_url.trim_end_matches('/')
        ))
        .header("Accept", "application/json")
        .bearer(secret.as_str())
        .max_response_bytes(self.max_response_bytes)
        .json(&build_body(req))?;
        let resp = self
            .client
            .send(&http)
            .map_err(|e| LlmError::from_http(e, Some(secret.as_str())))?;
        let v: Value = resp
            .json()
            .map_err(|e| LlmError::Malformed(e.to_string()))?;
        parse_response(&v)
    }
}

/// What a failed image call means for retrying and billing (#1004).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFailureClass {
    /// Nothing was sent or nothing was done: a connect failure, a `408`, `425` or `429` without
    /// a quota or billing message. Safe to retry.
    Retryable,
    /// The request may have been received and possibly billed: a failure after sending, a `5xx`,
    /// or a 2xx answer that cannot be read. Do not replay it blindly.
    AmbiguousMayBeBilled,
    /// The provider refused for billing: a `402`, or a `429` that names quota or billing.
    Billing,
    /// The credential is missing or refused: `401`, `403` or `407`, or no credential.
    Auth,
    /// The request is wrong or the route refuses it (other `4xx`, invalid request).
    Other,
}

/// A failed image call with its class.
#[derive(Debug)]
pub struct ImageFailure {
    pub class: ImageFailureClass,
    pub error: LlmError,
}

/// Class of an image call's error. The classes are decided from the error alone: a connect
/// failure is safe to retry, and anything after the request was sent may already be billed.
pub fn classify_image_error(error: &LlmError) -> ImageFailureClass {
    use rightkit_http::{HttpError, Phase};
    match error.root() {
        LlmError::MissingCredential(_) | LlmError::Auth { .. } => ImageFailureClass::Auth,
        LlmError::InvalidRequest(_) => ImageFailureClass::Other,
        LlmError::Status { status, message } => {
            let quota = {
                let lower = message.to_ascii_lowercase();
                lower.contains("quota") || lower.contains("billing")
            };
            match *status {
                401 | 403 | 407 => ImageFailureClass::Auth,
                402 => ImageFailureClass::Billing,
                429 if quota => ImageFailureClass::Billing,
                408 | 425 | 429 => ImageFailureClass::Retryable,
                500..=599 => ImageFailureClass::AmbiguousMayBeBilled,
                _ => ImageFailureClass::Other,
            }
        }
        LlmError::Http(HttpError::Transport { connect_phase, .. }) => {
            if *connect_phase {
                ImageFailureClass::Retryable
            } else {
                ImageFailureClass::AmbiguousMayBeBilled
            }
        }
        LlmError::Http(HttpError::Timeout { phase, .. }) => {
            if *phase == Phase::Connect {
                ImageFailureClass::Retryable
            } else {
                ImageFailureClass::AmbiguousMayBeBilled
            }
        }
        LlmError::Malformed(_) => ImageFailureClass::AmbiguousMayBeBilled,
        _ => ImageFailureClass::Other,
    }
}

/// Reject a result format the model is known to refuse, before anything is sent. The GPT image
/// models return base64 only and reject `response_format`, so a URL result is refused for them.
fn check_result_format(req: &ImageRequest) -> Result<(), LlmError> {
    if req.want_url
        && req
            .model
            .trim()
            .to_ascii_lowercase()
            .starts_with("gpt-image")
    {
        return Err(LlmError::InvalidRequest(
            "gpt-image models return base64 only; drop url_result() for this model".into(),
        ));
    }
    Ok(())
}

/// Redact a base URL for display: the userinfo becomes `REDACTED@`, every query value becomes
/// `REDACTED` (keys are kept), and a fragment becomes `#REDACTED`.
pub(crate) fn redact_url(raw: &str) -> String {
    const HIDDEN: &str = "REDACTED";
    let (rest, has_fragment) = match raw.split_once('#') {
        Some((head, _)) => (head, true),
        None => (raw, false),
    };
    let (head, query) = match rest.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (rest, None),
    };
    let (scheme, after_scheme) = match head.find("://") {
        Some(i) => (&head[..i + 3], &head[i + 3..]),
        None => ("", head),
    };
    let (authority, path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, ""),
    };
    let authority = match authority.rfind('@') {
        Some(i) => format!("{HIDDEN}@{}", &authority[i + 1..]),
        None => authority.to_owned(),
    };
    let mut out = format!("{scheme}{authority}{path}");
    if let Some(query) = query {
        let pairs: Vec<String> = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((key, _)) => format!("{key}={HIDDEN}"),
                None => HIDDEN.to_owned(),
            })
            .collect();
        out.push('?');
        out.push_str(&pairs.join("&"));
    }
    if has_fragment {
        out.push('#');
        out.push_str(HIDDEN);
    }
    out
}

fn build_body(req: &ImageRequest) -> Value {
    let mut body = json!({"model": req.model, "prompt": req.prompt, "n": 1});
    if let Some(size) = &req.size {
        body["size"] = json!(size);
    }
    if req.want_url {
        body["response_format"] = json!("url");
    }
    if let Some(quality) = &req.quality {
        body["quality"] = json!(quality);
    }
    if let Some(format) = &req.output_format {
        body["output_format"] = json!(format);
    }
    body
}

/// Reads the first image of an images response. Base64 is preferred when both are present.
pub fn parse_response(v: &Value) -> Result<GeneratedImage, LlmError> {
    let first = v
        .get("data")
        .and_then(|d| d.get(0))
        .ok_or_else(|| LlmError::Malformed("image response has no data".into()))?;
    let revised_prompt = first
        .get("revised_prompt")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(b64) = first.get("b64_json").and_then(Value::as_str) {
        if !b64.is_empty() {
            return Ok(GeneratedImage {
                result: ImageResult::Base64(b64.to_owned()),
                revised_prompt,
            });
        }
    }
    if let Some(url) = first.get("url").and_then(Value::as_str) {
        if !url.is_empty() {
            return Ok(GeneratedImage {
                result: ImageResult::Url(url.to_owned()),
                revised_prompt,
            });
        }
    }
    Err(LlmError::Malformed(
        "image response has neither b64_json nor url".into(),
    ))
}

/// One image route in a catalogue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRoute {
    /// Unique route id.
    pub id: String,
    /// Model id sent to the provider.
    pub model: String,
    /// Sizes the route accepts, such as `1024x1024`.
    pub sizes: Vec<String>,
    /// Result formats the route accepts, such as `b64_json` or `url`.
    pub formats: Vec<String>,
    /// Lower is tried first.
    pub priority: u32,
    /// USD price per generated image. Finite and not negative.
    pub usd_per_image: f64,
}

/// A set of image routes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRouteCatalogue {
    pub routes: Vec<ImageRoute>,
}

impl ImageRouteCatalogue {
    /// Parse and validate a catalogue. Ids must be non-empty and unique, and prices finite and
    /// not negative. Failures are [`LlmError::Malformed`].
    pub fn from_json(json: &str) -> Result<Self, LlmError> {
        let cat: Self =
            serde_json::from_str(json).map_err(|e| LlmError::Malformed(e.to_string()))?;
        cat.validate()?;
        Ok(cat)
    }

    fn validate(&self) -> Result<(), LlmError> {
        let mut seen = std::collections::BTreeSet::new();
        for r in &self.routes {
            if r.id.trim().is_empty() {
                return Err(LlmError::Malformed("image route id is empty".into()));
            }
            if !seen.insert(r.id.as_str()) {
                return Err(LlmError::Malformed(format!(
                    "duplicate image route id {}",
                    r.id
                )));
            }
            if !r.usd_per_image.is_finite() || r.usd_per_image < 0.0 {
                return Err(LlmError::Malformed(format!(
                    "image route {} has an invalid price",
                    r.id
                )));
            }
        }
        Ok(())
    }

    /// Routes ordered by priority, then id.
    pub fn ordered(&self) -> Vec<&ImageRoute> {
        let mut v: Vec<&ImageRoute> = self.routes.iter().collect();
        v.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
        v
    }

    pub fn route(&self, id: &str) -> Option<&ImageRoute> {
        self.routes.iter().find(|r| r.id == id)
    }
}
