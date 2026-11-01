//! Google Gemini API adapter: Veo 3.1 video, Gemini Omni Flash video, Gemini (Nano Banana) image models.
//!
//! Sources (fetched 2026-10-17):
//! - Veo guide: <https://ai.google.dev/gemini-api/docs/veo>. `POST {base}/v1beta/models/{model}:predictLongRunning`
//!   with header `x-goog-api-key`, body `{"instances":[{"prompt"}],"parameters":{aspectRatio, durationSeconds,
//!   resolution}}`. Reply `{"name": "<operation>"}`; poll `GET {base}/v1beta/{name}` until `done` is true; video at
//!   `response.generateVideoResponse.generatedSamples[0].video.uri`, downloaded with the same key header
//!   (the guide's curl uses `-L`, so redirects are followed). Videos are kept 2 days.
//!   Models: veo-3.1-generate-preview, veo-3.1-fast-generate-preview, veo-3.1-lite-generate-preview (all Preview).
//!   aspectRatio 16:9|9:16; durationSeconds 4|6|8; resolution 720p|1080p|4k, where 1080p and 4k need 8 s and
//!   4k is not offered on Lite.
//! - Prices per second with audio (<https://ai.google.dev/gemini-api/docs/pricing>, "Last updated 2026-10-16 UTC"):
//!   standard 720p $0.40 / 1080p $0.40 / 4k $0.60; fast $0.10 / $0.12 / $0.30; lite $0.05 / $0.08 / n/a.
//!   Not checked against an invoice.
//! - Imagen: NOT offered. <https://ai.google.dev/gemini-api/docs/models> (fetched 2026-10-17) lists "Imagen 4 (Shut down)".
//! - Interactions API, used for Omni Flash and the Gemini image models. Issue #57 said generateContent, but the
//!   image and Omni guides fetched 2026-10-17 document only `POST {base}/v1beta/interactions`; generateContent for
//!   image models is UNVERIFIED and not used:
//!   <https://ai.google.dev/gemini-api/docs/omni> and <https://ai.google.dev/gemini-api/docs/image-generation>.
//!   Body `{"model","input": "<prompt>","response_format":{"type":"video"|"image","aspect_ratio",
//!   "resolution"|"image_size","delivery":"uri"(video)}}`. The reply is the finished interaction
//!   (`status:"completed"`); media is in `steps[type=model_output].content[]` items `{type:"video"|"image",
//!   mime_type, data(base64) | uri}`. Treated as a synchronous submit. A video `uri` is a Files API file: poll
//!   `GET /v1beta/files/{id}` until `state` is ACTIVE, then download `{uri}` with the key header. The raw REST image
//!   reply shape (mime_type vs mimeType) is UNVERIFIED; both spellings are read.
//! - Omni Flash `gemini-omni-1.1-flash` (GA; `gemini-omni-flash-preview` also exists, not registered): aspect 16:9|9:16,
//!   resolution 360p|720p|1080p(upscaled)|4k(upscaled). No duration parameter is documented. Pricing page
//!   (<https://ai.google.dev/gemini-api/docs/pricing>, fetched 2026-10-17): $17.50 per 1M video output tokens at
//!   5,792 tokens per 720p second (about $0.10/s) plus $1.50 per 1M input tokens. The estimate assumes 10 s of
//!   720p video and ignores input tokens; whether 1080p/4k cost more is UNVERIFIED.
//! - Image models, standard paid output per image (pricing page, 2026-10-17): gemini-nano-banana-2.1 1K $0.0336, 2K
//!   $0.0504, 4K $0.113; gemini-3.1-flash-image 0.5K $0.045, 1K $0.067, 2K $0.101, 4K $0.151;
//!   gemini-3.1-flash-lite-image 1K $0.0336 (1K only); gemini-3-pro-image 1K/2K $0.134, 4K $0.24.
//!   gemini-2.5-flash-image is deprecated (shutdown 2026-10-02 per the pricing page) and not registered. Aspect ratios
//!   1:1 3:2 2:3 3:4 4:3 4:5 5:4 9:16 16:9 21:9 are verified only for the Lite model; the same list is assumed
//!   for the others (UNVERIFIED). Input-token charges are ignored in estimates.
//! - Image-to-video, last frame and reference images need inline base64 inputs and are not implemented.
//!
//! The finished video is downloaded inside `poll` (the key header is needed and `fetch` has no key), only from
//! the configured base host, so the key never goes to another host. The result is `Output::Bytes`.
use super::{
    request_may_have_reached, status_may_have_run, unknown_outcome, Durations, GenRequest, Kind,
    ModelInfo, Output, PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use crate::capabilities::{self, Capabilities};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://generativelanguage.googleapis.com";
const STD: &str = "veo-3.1-generate-preview";
const FAST: &str = "veo-3.1-fast-generate-preview";
const LITE: &str = "veo-3.1-lite-generate-preview";
const OMNI: &str = "gemini-omni-1.1-flash";
const NB21: &str = "gemini-nano-banana-2.1";
const NB2: &str = "gemini-3.1-flash-image";
const NB2L: &str = "gemini-3.1-flash-lite-image";
const NBPRO: &str = "gemini-3-pro-image";
const BODY_LIMIT: usize = 1 << 30;
const IMAGE_ASPECTS: [&str; 10] = [
    "1:1", "3:2", "2:3", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
];
/// Assumed Omni clip length for estimates; the docs list no duration parameter (unverified).
const OMNI_ASSUMED_SECS: f64 = 10.0;
const OMNI_USD_PER_SEC: f64 = 0.10;

static MODELS: [ModelInfo; 8] = [
    ModelInfo {
        provider: "google",
        model: STD,
        kind: Kind::Video,
        label: "Google Veo 3.1 direct (720p/1080p $0.40/s, 4k $0.60/s; docs price, unverified)",
        est_usd: 3.2,
        durations: Durations::set(&[4, 6, 8], 8),
    },
    ModelInfo {
        provider: "google",
        model: FAST,
        kind: Kind::Video,
        label: "Google Veo 3.1 Fast direct (720p $0.10/s, 1080p $0.12/s, 4k $0.30/s; docs price, unverified)",
        est_usd: 0.8,
        durations: Durations::set(&[4, 6, 8], 8),
    },
    ModelInfo {
        provider: "google",
        model: LITE,
        kind: Kind::Video,
        label: "Google Veo 3.1 Lite direct (720p $0.05/s, 1080p $0.08/s; docs price, unverified)",
        est_usd: 0.4,
        durations: Durations::set(&[4, 6, 8], 8),
    },
    ModelInfo {
        provider: "google",
        model: OMNI,
        kind: Kind::Video,
        label: "Google Gemini Omni Flash direct (about $0.10/s 720p, assumed 10 s clip; docs price, unverified)",
        est_usd: 1.0,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "google",
        model: NB21,
        kind: Kind::Image,
        label: "Google Nano Banana 2.1 direct (1K $0.0336, 2K $0.0504, 4K $0.113; docs price, unverified)",
        est_usd: 0.0336,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "google",
        model: NB2,
        kind: Kind::Image,
        label: "Google Nano Banana 2 direct (0.5K $0.045, 1K $0.067, 2K $0.101, 4K $0.151; docs price, unverified)",
        est_usd: 0.067,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "google",
        model: NB2L,
        kind: Kind::Image,
        label: "Google Nano Banana 2 Lite direct (1K only, $0.0336; docs price, unverified)",
        est_usd: 0.0336,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "google",
        model: NBPRO,
        kind: Kind::Image,
        label: "Google Nano Banana Pro direct (1K/2K $0.134, 4K $0.24; docs price, unverified)",
        est_usd: 0.134,
        durations: Durations::NONE,
    },
];

fn is_veo(model: &str) -> bool {
    matches!(model, STD | FAST | LITE)
}

/// Image size to USD per image; None when the model does not offer that size.
fn image_rate(model: &str, size: &str) -> Option<f64> {
    match (model, size) {
        (NB21, "1K") => Some(0.0336),
        (NB21, "2K") => Some(0.0504),
        (NB21, "4K") => Some(0.113),
        (NB2, "0.5K") => Some(0.045),
        (NB2, "1K") => Some(0.067),
        (NB2, "2K") => Some(0.101),
        (NB2, "4K") => Some(0.151),
        (NB2L, "1K") => Some(0.0336),
        (NBPRO, "1K") | (NBPRO, "2K") => Some(0.134),
        (NBPRO, "4K") => Some(0.24),
        _ => None,
    }
}

fn image_sizes(model: &str) -> &'static [&'static str] {
    match model {
        NB2 => &["0.5K", "1K", "2K", "4K"],
        NB2L => &["1K"],
        _ => &["1K", "2K", "4K"],
    }
}

/// Standard base64; whitespace and padding optional. None on any other byte.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut t: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '=')
        .collect();
    while t.len() % 4 != 0 {
        t.push('=');
    }
    rightkit_crypto::base64_decode(&t).ok()
}

fn no_inputs(req: &GenRequest) -> Result<(), ProviderError> {
    for k in [
        "image_url",
        "image_urls",
        "last_frame_url",
        "video_url",
        "reference_images",
    ] {
        if req.params.get(k).is_some_and(|v| !v.is_null()) {
            return Err(bad(format!(
                "google adapter is text-to-media only; params.{k} is not supported"
            )));
        }
    }
    Ok(())
}

/// Validates an Interactions API request (Omni video or Gemini image); returns (body, estimated USD).
fn resolve_interaction(req: &GenRequest) -> Result<(Value, f64), ProviderError> {
    let info = MODELS
        .iter()
        .find(|m| m.model == req.model)
        .ok_or_else(|| bad(format!("unknown google model {}", req.model)))?;
    if req.kind != info.kind {
        return Err(bad("model does not match request kind"));
    }
    if req.prompt.trim().is_empty() {
        return Err(bad("prompt is required"));
    }
    no_inputs(req)?;
    if info.model == OMNI {
        let ar = aspect(req)?;
        let res = match str_param(req, "resolution") {
            None => "720p",
            Some("360p") => "360p",
            Some("720p") => "720p",
            Some("1080p") => "1080p",
            Some("4k") | Some("4K") => "4k",
            Some(o) => {
                return Err(bad(format!(
                    "omni resolution must be 360p, 720p, 1080p or 4k, got {o}"
                )))
            }
        };
        let body = json!({
            "model": OMNI,
            "input": req.prompt,
            "response_format": {"type": "video", "aspect_ratio": ar, "resolution": res, "delivery": "uri"},
        });
        return Ok((body, OMNI_ASSUMED_SECS * OMNI_USD_PER_SEC));
    }
    let ar = match str_param(req, "aspect").or_else(|| str_param(req, "aspect_ratio")) {
        None => "1:1",
        Some(a) => IMAGE_ASPECTS
            .iter()
            .copied()
            .find(|x| *x == a)
            .ok_or_else(|| bad(format!("image aspect {a} is not supported")))?,
    };
    let size = match str_param(req, "resolution") {
        None => "1K",
        Some(r) => image_sizes(info.model)
            .iter()
            .copied()
            .find(|x| x.eq_ignore_ascii_case(r))
            .ok_or_else(|| bad(format!("{} does not offer image size {r}", info.model)))?,
    };
    let usd = image_rate(info.model, size)
        .ok_or_else(|| bad(format!("{} does not offer image size {size}", info.model)))?;
    let body = json!({
        "model": info.model,
        "input": req.prompt,
        "response_format": {"type": "image", "aspect_ratio": ar, "image_size": size},
    });
    Ok((body, usd))
}

pub struct Google {
    base: String,
    agent: Http,
}

impl Google {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent: Http::new(Duration::from_secs(10), Duration::from_secs(600)),
        }
    }

    /// Synchronous Interactions API call (Omni video, Gemini image). Any failure after the request may have reached
    /// Google is ambiguous because the work can be billed.
    fn submit_interaction(
        &self,
        req: &GenRequest,
        api_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        let (body, _) = resolve_interaction(req)?;
        let sent = Request::post(format!("{}/v1beta/interactions", self.base))
            .secret_header("x-goog-api-key", api_key)
            .max_response_bytes(BODY_LIMIT)
            .json(&body)
            .and_then(|r| self.agent.send(r));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key))
                .ambiguous_if(status_may_have_run(r.status)));
        }
        let amb = |code: &str, msg: String| {
            ProviderError::new(code, redact(&msg, api_key), false).ambiguous()
        };
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            amb(
                "bad_response",
                "google interaction reply was not JSON".into(),
            )
        })?;
        match v.get("status").and_then(Value::as_str) {
            Some("completed") | None => {}
            Some(o) => {
                return Err(amb(
                    "bad_response",
                    format!("google interaction status {o}"),
                ))
            }
        }
        let want = if req.kind == Kind::Video {
            "video"
        } else {
            "image"
        };
        let item = v
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|s| s.get("type").and_then(Value::as_str) == Some("model_output"))
            .filter_map(|s| s.get("content").and_then(Value::as_array))
            .flatten()
            .find(|c| c.get("type").and_then(Value::as_str) == Some(want))
            .ok_or_else(|| {
                amb(
                    "bad_response",
                    format!("google interaction had no {want} output (possibly filtered)"),
                )
            })?;
        let mime = item
            .get("mime_type")
            .or_else(|| item.get("mimeType"))
            .and_then(Value::as_str)
            .unwrap_or(if want == "video" {
                "video/mp4"
            } else {
                "image/png"
            })
            .to_string();
        if let Some(data) = item.get("data").and_then(Value::as_str) {
            let bytes = b64_decode(data)
                .filter(|b| !b.is_empty())
                .ok_or_else(|| amb("bad_response", "google output was not valid base64".into()))?;
            return Ok(SubmitOutcome::Completed {
                outputs: vec![Output::Bytes { bytes, mime }],
            });
        }
        let uri = item.get("uri").and_then(Value::as_str).ok_or_else(|| {
            amb(
                "bad_response",
                "google output had neither data nor uri".into(),
            )
        })?;
        // The key header goes only to the configured Gemini host.
        if !uri.starts_with(&format!("{}/", self.base)) {
            return Err(amb(
                "bad_response",
                "google output uri on unexpected host".into(),
            ));
        }
        let id = uri
            .rsplit('/')
            .next()
            .unwrap_or("")
            .split([':', '?'])
            .next()
            .unwrap_or("");
        if id.is_empty() || !safe_op(id) {
            return Err(amb(
                "bad_response",
                "google output uri had no usable file id".into(),
            ));
        }
        for attempt in 0..60 {
            let f = self
                .get(&format!("{}/v1beta/files/{id}", self.base), api_key)
                .map_err(|e| e.ambiguous())?;
            if !(200..300).contains(&f.status) {
                return Err(map_status(f.status, error_text(&f.body, api_key)).ambiguous());
            }
            let state = serde_json::from_slice::<Value>(&f.body)
                .ok()
                .and_then(|j| j.get("state").and_then(Value::as_str).map(String::from));
            match state.as_deref() {
                Some("FAILED") => {
                    return Err(amb(
                        "provider_failed",
                        "google file processing FAILED".into(),
                    ))
                }
                Some("ACTIVE") | None => break,
                _ if attempt == 59 => {
                    return Err(amb("timeout", "google file never became ACTIVE".into()))
                }
                _ => std::thread::sleep(Duration::from_secs(5)),
            }
        }
        let d = self.get(uri, api_key).map_err(|e| e.ambiguous())?;
        if !(200..300).contains(&d.status) {
            return Err(map_status(d.status, error_text(&d.body, api_key)).ambiguous());
        }
        if d.body.is_empty() {
            return Err(amb(
                "bad_response",
                "google video download was empty".into(),
            ));
        }
        let mime = http::content_type(&d)
            .filter(|c| c.starts_with("video/"))
            .unwrap_or(mime);
        Ok(SubmitOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes: d.body,
                mime,
            }],
        })
    }

    fn get(&self, url: &str, key: &str) -> Result<Response, ProviderError> {
        read(
            self.agent.send(
                Request::get(url)
                    .secret_header("x-goog-api-key", key)
                    .max_response_bytes(BODY_LIMIT),
            ),
            key,
        )
    }
}

fn bad(msg: impl Into<String>) -> ProviderError {
    ProviderError::new("invalid_request", msg, false)
}

fn str_param<'a>(req: &'a GenRequest, k: &str) -> Option<&'a str> {
    req.params
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn duration(req: &GenRequest) -> Result<u32, ProviderError> {
    let v = match req
        .params
        .get("duration")
        .or_else(|| req.params.get("duration_s"))
    {
        None | Some(Value::Null) => return Ok(8),
        Some(v) => v,
    };
    let n = v.as_u64().or_else(|| {
        v.as_str()
            .and_then(|s| s.trim_end_matches('s').parse::<u64>().ok())
    });
    match n {
        Some(n @ (4 | 6 | 8)) => Ok(n as u32),
        _ => Err(bad(format!(
            "veo duration must be 4, 6 or 8 seconds, got {v}"
        ))),
    }
}

fn resolution(req: &GenRequest) -> Result<&'static str, ProviderError> {
    match str_param(req, "resolution") {
        None | Some("720p") => Ok("720p"),
        Some("1080p") => Ok("1080p"),
        Some("4k") | Some("4K") => Ok("4k"),
        Some(o) => Err(bad(format!(
            "veo resolution must be 720p, 1080p or 4k, got {o}"
        ))),
    }
}

fn aspect(req: &GenRequest) -> Result<&'static str, ProviderError> {
    match str_param(req, "aspect").or_else(|| str_param(req, "aspect_ratio")) {
        None | Some("16:9") => Ok("16:9"),
        Some("9:16") => Ok("9:16"),
        Some(o) => Err(bad(format!("veo aspect must be 16:9 or 9:16, got {o}"))),
    }
}

/// USD per second with audio, from the pricing page.
fn rate(model: &str, res: &str) -> Option<f64> {
    match (model, res) {
        (STD, "720p") | (STD, "1080p") => Some(0.40),
        (STD, "4k") => Some(0.60),
        (FAST, "720p") => Some(0.10),
        (FAST, "1080p") => Some(0.12),
        (FAST, "4k") => Some(0.30),
        (LITE, "720p") => Some(0.05),
        (LITE, "1080p") => Some(0.08),
        _ => None,
    }
}

/// Validates and returns (model, seconds, resolution, aspect).
fn resolve_veo(
    req: &GenRequest,
) -> Result<(&'static str, u32, &'static str, &'static str), ProviderError> {
    let info = MODELS
        .iter()
        .find(|m| m.model == req.model)
        .ok_or_else(|| bad(format!("unknown google model {}", req.model)))?;
    if req.kind != Kind::Video || !is_veo(info.model) {
        return Err(bad("model does not match request kind"));
    }
    if req.prompt.trim().is_empty() {
        return Err(bad("prompt is required"));
    }
    let (secs, res, ar) = (duration(req)?, resolution(req)?, aspect(req)?);
    if rate(info.model, res).is_none() {
        return Err(bad(format!("{} does not offer {res}", info.model)));
    }
    if res != "720p" && secs != 8 {
        return Err(bad("veo 1080p and 4k require duration 8"));
    }
    for k in ["image_url", "image_urls", "last_frame_url", "video_url"] {
        if req.params.get(k).is_some_and(|v| !v.is_null()) {
            return Err(bad(format!(
                "google adapter is text-to-video only; params.{k} is not supported"
            )));
        }
    }
    Ok((info.model, secs, res, ar))
}

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

fn read(r: Result<Response, HttpError>, key: &str) -> Result<Response, ProviderError> {
    r.map_err(|e| {
        ProviderError::new(
            "transport",
            redact(&format!("google transport error: {e}"), key),
            http::retryable(&e),
        )
    })
}

fn error_text(body: &[u8], key: &str) -> String {
    let v: Option<Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.pointer("/error/message"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    redact(&msg.chars().take(300).collect::<String>(), key)
}

fn map_status(status: u16, text: String) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let billing = [
        "billing",
        "quota",
        "resource_exhausted",
        "prepayment",
        "credits",
    ]
    .iter()
    .any(|n| lower.contains(n));
    match status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("google rejected the API key ({status}): {text}"),
            false,
        ),
        402 => ProviderError::new("billing", format!("google billing problem: {text}"), false),
        // 429 RESOURCE_EXHAUSTED may be a hard quota stop or a transient rate limit; a plain 429 stays retryable.
        429 if billing
            && (lower.contains("billing")
                || lower.contains("prepayment")
                || lower.contains("credits")) =>
        {
            ProviderError::new("billing", format!("google billing problem: {text}"), false)
        }
        408 | 429 => ProviderError::new("rate_limited", format!("google {status}: {text}"), true),
        500..=599 => ProviderError::new("unavailable", format!("google {status}: {text}"), true),
        s => ProviderError::new("invalid_request", format!("google {s}: {text}"), false),
    }
}

/// An operation name such as `models/veo-3.1-generate-preview/operations/abc123`.
fn safe_op(n: &str) -> bool {
    !n.is_empty()
        && !n.contains("..")
        && !n.starts_with('/')
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
}

impl Provider for Google {
    fn id(&self) -> &'static str {
        "google"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        if model == OMNI {
            return Capabilities::default()
                .mode("prompt")
                .setting(
                    "aspect",
                    capabilities::aspect("aspect", &["16:9", "9:16"], false),
                )
                .setting(
                    "resolution",
                    capabilities::resolution(
                        "resolution",
                        &["360p", "720p", "1080p", "4k"],
                        false,
                        Some("720p"),
                    ),
                );
        }
        if !is_veo(model) {
            return Capabilities::default()
                .mode("prompt")
                .setting(
                    "aspect",
                    capabilities::aspect("aspect", &IMAGE_ASPECTS, false),
                )
                .setting(
                    "resolution",
                    capabilities::resolution("resolution", image_sizes(model), false, Some("1K")),
                );
        }
        let res: &[&str] = if model == LITE {
            &["720p", "1080p"]
        } else {
            &["720p", "1080p", "4k"]
        };
        Capabilities::default()
            .mode("prompt")
            .setting(
                "aspect",
                capabilities::aspect("aspect", &["16:9", "9:16"], false),
            )
            .setting(
                "resolution",
                capabilities::resolution("resolution", res, false, Some("720p")),
            )
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        if !is_veo(&req.model) {
            return resolve_interaction(req).map(|(_, usd)| usd);
        }
        let (m, secs, res, _) = resolve_veo(req)?;
        Ok(secs as f64 * rate(m, res).unwrap_or(0.0))
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        if !is_veo(&req.model) {
            return self.submit_interaction(req, api_key);
        }
        let (model, secs, res, ar) = resolve_veo(req)?;
        let body = json!({
            "instances": [{"prompt": req.prompt}],
            "parameters": {"aspectRatio": ar, "durationSeconds": secs.to_string(), "resolution": res},
        });
        let sent = Request::post(format!(
            "{}/v1beta/models/{model}:predictLongRunning",
            self.base
        ))
        .secret_header("x-goog-api-key", api_key)
        .max_response_bytes(BODY_LIMIT)
        .json(&body)
        .and_then(|r| self.agent.send(r));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key))
                .ambiguous_if(status_may_have_run(r.status)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "google submit reply was not JSON", true).ambiguous()
        })?;
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| safe_op(n))
            .ok_or_else(|| {
                ProviderError::new(
                    "bad_response",
                    "google submit reply had no usable operation name",
                    true,
                )
                .ambiguous()
            })?;
        Ok(SubmitOutcome::Accepted {
            provider_job_id: name.to_string(),
        })
    }

    fn poll(
        &self,
        _req: &GenRequest,
        provider_job_id: &str,
        api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        if !safe_op(provider_job_id) {
            return Err(bad("invalid google operation name"));
        }
        let r = self.get(
            &format!("{}/v1beta/{}", self.base, provider_job_id),
            api_key,
        )?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "google operation reply was not JSON", true)
        })?;
        if v.get("done").and_then(Value::as_bool) != Some(true) {
            return Ok(PollOutcome::Running);
        }
        if let Some(e) = v.get("error") {
            let m = e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Ok(PollOutcome::Failed {
                message: redact(&format!("google operation failed: {m}"), api_key),
            });
        }
        let Some(uri) = v
            .pointer("/response/generateVideoResponse/generatedSamples/0/video/uri")
            .and_then(Value::as_str)
        else {
            // Done with no sample: often a safety filter (raiMediaFilteredReasons); the work may be billed.
            return Ok(unknown_outcome("done, no video", &r.body, api_key, true));
        };
        // The key header goes only to the configured Gemini host.
        if !uri.starts_with(&format!("{}/", self.base)) {
            return Ok(unknown_outcome(
                "video uri on unexpected host",
                &r.body,
                api_key,
                true,
            ));
        }
        let d = self.get(uri, api_key)?;
        if !(200..300).contains(&d.status) {
            return Err(map_status(d.status, error_text(&d.body, api_key)));
        }
        if d.body.is_empty() {
            return Ok(unknown_outcome("done, empty video", &r.body, api_key, true));
        }
        let mime = http::content_type(&d)
            .filter(|c| c.starts_with("video/"))
            .unwrap_or_else(|| "video/mp4".into());
        Ok(PollOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes: d.body,
                mime,
            }],
        })
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // Listing models is free and requires a valid key.
        let r = self.get(&format!("{}/v1beta/models?pageSize=1", self.base), api_key)?;
        if (200..300).contains(&r.status) {
            Ok(())
        } else {
            Err(map_status(r.status, error_text(&r.body, api_key)))
        }
    }

    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        match output {
            Output::Bytes { bytes, mime } => Ok((bytes.clone(), mime.clone())),
            _ => Err(ProviderError::new(
                "unsupported",
                "google video is downloaded during poll",
                false,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const KEY: &str = "AIza-google-secret-1";

    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    fn mock<F>(handler: F) -> (String, Seen)
    where
        F: Fn(&str, &str, &str, &str) -> (u16, &'static str, Vec<u8>) + Send + 'static,
    {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (s2, b2) = (seen.clone(), base.clone());
        std::thread::spawn(move || {
            for st in l.incoming() {
                let Ok(mut st) = st else { continue };
                let (mut buf, mut tmp) = (Vec::new(), [0u8; 4096]);
                let hend = loop {
                    let n = st.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(p + 4);
                    }
                };
                let Some(hend) = hend else { continue };
                let head = String::from_utf8_lossy(&buf[..hend]).to_string();
                let cl = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while buf.len() < hend + cl {
                    let n = st.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body = String::from_utf8_lossy(&buf[hend..]).to_string();
                let mut first = head.lines().next().unwrap_or("").split(' ');
                let (method, path) = (
                    first.next().unwrap_or("").to_string(),
                    first.next().unwrap_or("").to_string(),
                );
                s2.lock().unwrap().push((head.clone(), body.clone()));
                let (code, ct, out) = handler(&b2, &method, &path, &body);
                let h = format!("HTTP/1.1 {code} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = st.write_all(h.as_bytes());
                let _ = st.write_all(&out);
            }
        });
        (base, seen)
    }

    fn req(model: &str, params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::Video,
            provider: "google".into(),
            model: model.into(),
            prompt: "a lighthouse at dusk".into(),
            params,
        }
    }

    fn js(v: Value) -> (u16, &'static str, Vec<u8>) {
        (200, "application/json", v.to_string().into_bytes())
    }

    #[test]
    fn registered_shape_and_prices_by_hand() {
        let g = Google::new("http://127.0.0.1:1");
        for (m, p, usd) in [
            (STD, json!({}), 8.0 * 0.40),
            (STD, json!({"resolution": "4k"}), 8.0 * 0.60),
            (FAST, json!({"duration": 6}), 6.0 * 0.10),
            (
                FAST,
                json!({"duration": "8s", "resolution": "1080p"}),
                8.0 * 0.12,
            ),
            (FAST, json!({"resolution": "4k"}), 8.0 * 0.30),
            (LITE, json!({"duration": 4}), 4.0 * 0.05),
            (LITE, json!({"resolution": "1080p"}), 8.0 * 0.08),
        ] {
            assert!(
                (g.estimate_usd(&req(m, p.clone())).unwrap() - usd).abs() < 1e-9,
                "{m} {p}"
            );
        }
    }

    #[test]
    fn interaction_prices_and_validation() {
        let g = Google::new("http://127.0.0.1:1");
        let ir = |m: &str, k: Kind, p: Value| GenRequest {
            kind: k,
            provider: "google".into(),
            model: m.into(),
            prompt: "x".into(),
            params: p,
        };
        for (m, k, p, usd) in [
            (NB21, Kind::Image, json!({}), 0.0336),
            (
                NB21,
                Kind::Image,
                json!({"resolution": "4K", "aspect": "21:9"}),
                0.113,
            ),
            (NB2, Kind::Image, json!({"resolution": "0.5K"}), 0.045),
            (NB2L, Kind::Image, json!({}), 0.0336),
            (NBPRO, Kind::Image, json!({"resolution": "2K"}), 0.134),
            (NBPRO, Kind::Image, json!({"resolution": "4K"}), 0.24),
            (OMNI, Kind::Video, json!({"resolution": "1080p"}), 1.0),
        ] {
            assert!(
                (g.estimate_usd(&ir(m, k, p.clone())).unwrap() - usd).abs() < 1e-9,
                "{m} {p}"
            );
        }
        for (m, k, p) in [
            (NB2L, Kind::Image, json!({"resolution": "2K"})),
            (NB21, Kind::Image, json!({"resolution": "0.5K"})),
            (NB21, Kind::Image, json!({"aspect": "7:3"})),
            (NB21, Kind::Video, json!({})),
            (OMNI, Kind::Image, json!({})),
            (OMNI, Kind::Video, json!({"aspect": "1:1"})),
            (OMNI, Kind::Video, json!({"image_url": "https://x/a.png"})),
            ("imagen-4.0-generate", Kind::Image, json!({})),
        ] {
            assert!(g.estimate_usd(&ir(m, k, p.clone())).is_err(), "{m} {p}");
        }
    }

    #[test]
    fn image_interaction_decodes_inline_data() {
        let (base, seen) = mock(|_, m, path, _| match (m, path) {
            ("POST", "/v1beta/interactions") => js(json!({"status": "completed", "steps": [
                {"type": "user_input", "content": []},
                {"type": "model_output", "content": [{"type": "image", "mime_type": "image/png", "data": "iVBORw0K"}]}]})),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let r = GenRequest {
            kind: Kind::Image,
            provider: "google".into(),
            model: NB21.into(),
            prompt: "a fox".into(),
            params: json!({"aspect": "16:9", "resolution": "2K"}),
        };
        let SubmitOutcome::Completed { outputs } = Google::new(&base).submit(&r, KEY).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            outputs,
            vec![Output::Bytes {
                bytes: vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a],
                mime: "image/png".into()
            }]
        );
        let s = seen.lock().unwrap();
        let b: Value = serde_json::from_str(&s[0].1).unwrap();
        assert_eq!(
            (
                b["model"].as_str(),
                b["input"].as_str(),
                b["response_format"]["image_size"].as_str(),
                b["response_format"]["aspect_ratio"].as_str()
            ),
            (Some(NB21), Some("a fox"), Some("2K"), Some("16:9"))
        );
        assert!(s[0].0.to_ascii_lowercase().contains("x-goog-api-key"));
    }

    #[test]
    fn omni_uri_download_and_failure_modes() {
        let (base, seen) = mock(|base, m, path, body| match (m, path) {
            ("POST", "/v1beta/interactions") if body.contains("nofile") => js(
                json!({"status": "completed", "steps": [{"type": "model_output", "content": [{"type": "text"}]}]}),
            ),
            ("POST", "/v1beta/interactions") if body.contains("foreign") => js(
                json!({"status": "completed", "steps": [{"type": "model_output", "content": [{"type": "video", "uri": "https://evil.example/v1beta/files/z:download"}]}]}),
            ),
            ("POST", "/v1beta/interactions") => js(
                json!({"status": "completed", "steps": [{"type": "model_output", "content": [{"type": "video", "mime_type": "video/mp4", "uri": format!("{base}/v1beta/files/vid1:download?alt=media")}]}]}),
            ),
            ("GET", "/v1beta/files/vid1") => js(json!({"name": "files/vid1", "state": "ACTIVE"})),
            ("GET", "/v1beta/files/vid1:download?alt=media") => {
                (200, "video/mp4", vec![0, 0, 0, 24, 102, 116, 121, 112])
            }
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let g = Google::new(&base);
        let mut r = req(OMNI, json!({"aspect": "9:16", "resolution": "1080p"}));
        let SubmitOutcome::Completed { outputs } = g.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(g.fetch(&outputs[0]).unwrap().1, "video/mp4");
        let b: Value = serde_json::from_str(&seen.lock().unwrap()[0].1).unwrap();
        assert_eq!(
            (
                b["response_format"]["type"].as_str(),
                b["response_format"]["delivery"].as_str(),
                b["response_format"]["resolution"].as_str()
            ),
            (Some("video"), Some("uri"), Some("1080p"))
        );
        for p in ["nofile", "foreign"] {
            r.prompt = p.into();
            let e = g.submit(&r, KEY).unwrap_err();
            assert!(e.ambiguous && e.code == "bad_response", "{p} {e:?}");
        }
    }

    #[test]
    fn invalid_requests_never_reach_network() {
        let (base, seen) = mock(|_, _, _, _| js(json!({"name": "models/x/operations/a"})));
        let g = Google::new(&base);
        for (m, p) in [
            (STD, json!({"duration": 5})),
            (STD, json!({"duration": 6, "resolution": "1080p"})),
            (STD, json!({"duration": 4, "resolution": "4k"})),
            (LITE, json!({"resolution": "4k"})),
            (STD, json!({"aspect": "1:1"})),
            (STD, json!({"image_url": "https://x/a.png"})),
            ("veo-2", json!({})),
        ] {
            assert!(g.submit(&req(m, p.clone()), KEY).is_err(), "{m} {p}");
        }
        let mut r = req(STD, json!({}));
        r.kind = Kind::Image;
        assert!(g.submit(&r, KEY).is_err());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn submit_poll_and_authenticated_download() {
        let polls = Arc::new(Mutex::new(0));
        let p2 = polls.clone();
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/v1beta/models/veo-3.1-fast-generate-preview:predictLongRunning") => {
                js(json!({"name": "models/veo-3.1-fast-generate-preview/operations/op1"}))
            }
            ("GET", "/v1beta/models/veo-3.1-fast-generate-preview/operations/op1") => {
                let mut n = p2.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    js(json!({"name": "x", "done": false}))
                } else {
                    js(
                        json!({"done": true, "response": {"generateVideoResponse": {"generatedSamples": [{"video": {"uri": format!("{base}/v1beta/files/f1:download?alt=media")}}]}}}),
                    )
                }
            }
            ("GET", "/v1beta/files/f1:download?alt=media") => {
                (200, "video/mp4", vec![0, 0, 0, 24, 102, 116, 121, 112])
            }
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let g = Google::new(&base);
        let r = req(FAST, json!({"aspect": "9:16", "duration": 6}));
        let SubmitOutcome::Accepted { provider_job_id } = g.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            g.poll(&r, &provider_job_id, KEY).unwrap(),
            PollOutcome::Running
        );
        let PollOutcome::Completed { outputs } = g.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(g.fetch(&outputs[0]).unwrap().1, "video/mp4");
        let s = seen.lock().unwrap();
        let b: Value = serde_json::from_str(&s[0].1).unwrap();
        assert_eq!(b["instances"][0]["prompt"], "a lighthouse at dusk");
        assert_eq!(
            (
                b["parameters"]["aspectRatio"].as_str(),
                b["parameters"]["durationSeconds"].as_str(),
                b["parameters"]["resolution"].as_str()
            ),
            (Some("9:16"), Some("6"), Some("720p"))
        );
        assert!(s[0]
            .0
            .to_ascii_lowercase()
            .contains(&format!("x-goog-api-key: {KEY}").to_ascii_lowercase()));
        assert!(s
            .last()
            .unwrap()
            .0
            .to_ascii_lowercase()
            .contains("x-goog-api-key"));
    }

    #[test]
    fn done_states_failure_unknown_and_foreign_host() {
        let (base, _) = mock(|_, m, path, _| match (m, path) {
            ("GET", "/v1beta/models/m/operations/err") => {
                js(json!({"done": true, "error": {"code": 3, "message": "blocked"}}))
            }
            ("GET", "/v1beta/models/m/operations/nosample") => js(
                json!({"done": true, "response": {"generateVideoResponse": {"raiMediaFilteredCount": 1}}}),
            ),
            ("GET", "/v1beta/models/m/operations/foreign") => js(
                json!({"done": true, "response": {"generateVideoResponse": {"generatedSamples": [{"video": {"uri": "https://evil.example/v.mp4"}}]}}}),
            ),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let g = Google::new(&base);
        let r = req(STD, json!({}));
        assert_eq!(
            g.poll(&r, "models/m/operations/err", KEY).unwrap(),
            PollOutcome::Failed {
                message: "google operation failed: blocked".into()
            }
        );
        assert!(matches!(
            g.poll(&r, "models/m/operations/nosample", KEY).unwrap(),
            PollOutcome::Unknown {
                completed: true,
                ..
            }
        ));
        assert!(matches!(
            g.poll(&r, "models/m/operations/foreign", KEY).unwrap(),
            PollOutcome::Unknown { .. }
        ));
        assert!(g.poll(&r, "../etc", KEY).is_err());
    }

    #[test]
    fn errors_map_redact_and_flag_ambiguity() {
        for (code, c, retry, amb) in [
            (401u16, "auth", false, false),
            (400, "invalid_request", false, false),
            (429, "rate_limited", true, true),
            (503, "unavailable", true, true),
        ] {
            let (base, _) = mock(move |_, _, _, _| {
                (
                    code,
                    "application/json",
                    format!("{{\"error\":{{\"message\":\"bad {KEY}\"}}}}").into_bytes(),
                )
            });
            let e = Google::new(&base)
                .submit(&req(STD, json!({})), KEY)
                .unwrap_err();
            assert_eq!(
                (e.code.as_str(), e.retryable, e.ambiguous),
                (c, retry, amb),
                "{code} {e:?}"
            );
            assert!(!e.message.contains(KEY));
        }
        let (base, _) = mock(|_, _, _, _| js(json!({"name": "../bad"})));
        let e = Google::new(&base)
            .submit(&req(STD, json!({})), KEY)
            .unwrap_err();
        assert!(e.ambiguous && e.code == "bad_response");
        let (base, _) = mock(|_, _, path, _| {
            if path.starts_with("/v1beta/models") {
                js(json!({"models": []}))
            } else {
                (404, "text/plain", vec![])
            }
        });
        assert!(Google::new(&base).test_key(KEY).is_ok());
    }
}
