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
}

impl ImageRequest {
    pub fn new(model: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            prompt: prompt.into(),
            size: None,
            want_url: false,
        }
    }

    pub fn with_size(mut self, size: impl Into<String>) -> Self {
        self.size = Some(size.into());
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
    /// size error.
    pub fn with_max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = limit;
        self
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base64_and_url_results_distinctly() {
        let b = parse_response(&json!({"data": [{"b64_json": "QUJD", "revised_prompt": "x"}]}))
            .unwrap();
        assert_eq!(b.result, ImageResult::Base64("QUJD".into()));
        assert_eq!(b.revised_prompt.as_deref(), Some("x"));
        let u = parse_response(&json!({"data": [{"url": "https://example.test/i.png"}]})).unwrap();
        assert_eq!(
            u.result,
            ImageResult::Url("https://example.test/i.png".into())
        );
        assert!(matches!(
            parse_response(&json!({"data": []})),
            Err(LlmError::Malformed(_))
        ));
    }

    #[test]
    fn body_has_size_and_url_format_only_when_asked() {
        let plain = build_body(&ImageRequest::new("m", "p").with_size("1024x1024"));
        assert_eq!(plain["size"], "1024x1024");
        assert!(plain.get("response_format").is_none());
        let url = build_body(&ImageRequest::new("m", "p").url_result());
        assert_eq!(url["response_format"], "url");
        assert!(url.get("size").is_none());
    }

    #[test]
    fn gpt_image_models_reject_url_result_locally() {
        assert!(check_result_format(&ImageRequest::new("gpt-image-1", "p").url_result()).is_err());
        assert!(check_result_format(&ImageRequest::new("dall-e-3", "p").url_result()).is_ok());
        assert!(check_result_format(&ImageRequest::new("gpt-image-1", "p")).is_ok());
    }

    #[test]
    fn redact_url_hides_userinfo_and_query_values() {
        assert_eq!(
            redact_url("https://user:pw@api.example.test/v1?key=abc&api-version=2"),
            "https://REDACTED@api.example.test/v1?key=REDACTED&api-version=REDACTED"
        );
        assert_eq!(
            redact_url("http://localhost:8080/v1#frag"),
            "http://localhost:8080/v1#REDACTED"
        );
        assert_eq!(
            redact_url("https://api.example.test/v1"),
            "https://api.example.test/v1"
        );
    }

    #[test]
    fn catalogue_orders_by_priority_and_rejects_bad_rows() {
        let cat = ImageRouteCatalogue::from_json(
            r#"{"routes":[
                {"id":"b","model":"m2","sizes":["1024x1024"],"formats":["b64_json"],"priority":2,"usd_per_image":0.04},
                {"id":"a","model":"m1","sizes":["1024x1024"],"formats":["url"],"priority":1,"usd_per_image":0.02}
            ]}"#,
        )
        .unwrap();
        let ids: Vec<&str> = cat.ordered().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert!(ImageRouteCatalogue::from_json(
            r#"{"routes":[{"id":"a","model":"m","sizes":[],"formats":[],"priority":0,"usd_per_image":-1.0}]}"#
        )
        .is_err());
        assert!(ImageRouteCatalogue::from_json(
            r#"{"routes":[{"id":"a","model":"m","sizes":[],"formats":[],"priority":0,"usd_per_image":0.0},
                          {"id":"a","model":"m","sizes":[],"formats":[],"priority":0,"usd_per_image":0.0}]}"#
        )
        .is_err());
    }
}
