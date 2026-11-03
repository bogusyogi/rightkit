//! NanoBananaAPI adapter (Nano Banana 2 image generation and editing).
//!
//! Sources (the workspace reference implementation, read 2026-10-05):
//! - `tools/backends/nanobananaapi.js`: `POST {base}/api/v1/nanobanana/generate-2`
//!   (Bearer auth; body `prompt`, `imageUrls`, `aspectRatio`, `resolution`,
//!   `googleSearch`, `outputFormat`), poll `GET .../nanobanana/record-info?taskId=`
//!   (`data.successFlag` 0 running, 1 success, 2/3 failed; image at
//!   `data.response.resultImageUrl`). Docs: <https://docs.nanobananaapi.ai/nanobanana-api/generate-image-2>.
//! - `tools/lib/nb2-credits.mjs`: free key check `GET /api/v1/common/credit`
//!   (`data` = remaining credits). Docs: <https://docs.nanobananaapi.ai/common-api/get-account-credits>.
//! - Pricing, invoice-verified 2026-05-07 (backend comment): credits per call
//!   1K=8, 2K=12, 4K=18 at $5 per 1000 credits ($0.005/credit) = $0.04 / $0.06 / $0.09.
//!
//! The API often answers HTTP 200 with the failure in a JSON `code` field, so
//! both the HTTP status and the body `code` are mapped. Reference images
//! (`params.image_urls`) turn the same endpoint into an editor/compositor. The API
//! reads only public http(s) URLs, so `api::generate` uploads a local reference to
//! the user's own fal storage first (`fal_storage`, only with the per-run
//! `confirm_fal_upload: true`); a local path that still reaches this adapter is refused.
use super::{
    request_may_have_reached, unknown_outcome, Durations, GenRequest, Kind, ModelInfo, Output,
    PollOutcome, Provider, ProviderError, ReferenceInput, SubmitOutcome,
};
use crate::capabilities::{self, Capabilities};
use crate::fal_storage::local_path;
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.nanobananaapi.ai";

/// Defensive refusal for a local reference that was not swapped for an uploaded URL.
/// NanoBananaAPI documents no file upload or inline image data; `api::generate`
/// uploads to the user's own fal storage (after confirmation) before the job exists,
/// so this is reached only by a caller that bypassed that step.
const LOCAL_REFERENCE_REFUSED: &str =
    "a local reference image reached NanoBanana without being uploaded; send it through generate with confirm_fal_upload: true so it is uploaded to your fal storage first";

const MODEL: &str = "nano-banana-2";
const GENERATE_PATH: &str = "/api/v1/nanobanana/generate-2";
const STATUS_PATH: &str = "/api/v1/nanobanana/record-info";
const CREDIT_PATH: &str = "/api/v1/common/credit";

/// USD per image: credits (8/12/18) x $0.005.
const PRICE_1K: f64 = 0.04;
const PRICE_2K: f64 = 0.06;
const PRICE_4K: f64 = 0.09;

static MODELS: [ModelInfo; 1] = [ModelInfo {
    provider: "nanobanana",
    model: MODEL,
    kind: Kind::Image,
    label: "Nano Banana 2 (image + edit, 2K default, $0.04/$0.06/$0.09 per 1K/2K/4K)",
    est_usd: PRICE_2K,
    durations: Durations::NONE,
}];

pub struct NanoBanana {
    base: String,
    agent: Http,
}

impl NanoBanana {
    /// `base` is the resolved endpoint (see `providers::base_url_override`).
    pub fn new(base: &str) -> Self {
        let agent = Http::new(Duration::from_secs(10), Duration::from_secs(300));
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent,
        }
    }

    /// Every reference must be an http(s) URL that NanoBananaAPI fetches itself. A local
    /// file is refused here, before any request, by both `estimate_usd` and `submit`
    /// (`api::generate` replaces local files with uploaded fal URLs before either runs).
    fn check(&self, req: &GenRequest) -> Result<(), ProviderError> {
        if req.model != MODEL {
            return Err(ProviderError::new(
                "invalid_request",
                format!("unknown nanobanana model {}", req.model),
                false,
            ));
        }
        if req.kind != Kind::Image {
            return Err(ProviderError::new(
                "invalid_request",
                "model does not match request kind",
                false,
            ));
        }
        resolution(req)?;
        let not_url = image_urls(req)?
            .into_iter()
            .find(|u| !u.starts_with("http://") && !u.starts_with("https://"));
        match not_url {
            None => Ok(()),
            Some(u) if local_path(&u).is_some() => Err(ProviderError::new(
                "invalid_request",
                LOCAL_REFERENCE_REFUSED,
                false,
            )),
            Some(_) => Err(ProviderError::new(
                "invalid_request",
                "image_urls must be public http(s) URLs",
                false,
            )),
        }
    }

    fn get(&self, path: &str, key: &str) -> Result<Resp, ProviderError> {
        read(
            self.agent.send(
                Request::get(format!("{}{}", self.base, path))
                    .secret_header("Authorization", format!("Bearer {key}"))
                    .max_response_bytes(BODY_LIMIT as usize),
            ),
            key,
        )
    }
}

fn resolution(req: &GenRequest) -> Result<&'static str, ProviderError> {
    match req.params.get("resolution").and_then(Value::as_str) {
        None | Some("2K") => Ok("2K"),
        Some("1K") => Ok("1K"),
        Some("4K") => Ok("4K"),
        Some(o) => Err(ProviderError::new(
            "invalid_request",
            format!("unsupported resolution {o} (1K, 2K or 4K)"),
            false,
        )),
    }
}

fn image_urls(req: &GenRequest) -> Result<Vec<String>, ProviderError> {
    match req.params.get("image_urls") {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str().map(String::from).ok_or_else(|| {
                    ProviderError::new("invalid_request", "image_urls must be strings", false)
                })
            })
            .collect(),
        Some(_) => Err(ProviderError::new(
            "invalid_request",
            "image_urls must be an array",
            false,
        )),
    }
}

/// Every ratio `aspect` accepts, in the order the app lists them.
const ASPECTS: [&str; 15] = [
    "1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3", "21:9", "4:5", "5:4", "1:4", "4:1", "1:8",
    "8:1", "auto",
];

fn aspect(req: &GenRequest) -> &'static str {
    const OK: [&str; 15] = ASPECTS;
    let a = req
        .params
        .get("aspect")
        .or_else(|| req.params.get("aspect_ratio"))
        .and_then(Value::as_str);
    match a {
        Some("17:22") => "3:4",
        Some(a) => OK.iter().find(|o| **o == a).copied().unwrap_or("1:1"),
        None => "1:1",
    }
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    content_type: Option<String>,
}

const BODY_LIMIT: u64 = 1 << 30;

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

/// Each request sets its own body cap with `max_response_bytes(BODY_LIMIT as usize)`.
// Every send failure, including a failed body read, is a "transport" error; `http::retryable` sets the flag.
fn read(r: Result<Response, HttpError>, key: &str) -> Result<Resp, ProviderError> {
    let resp = r.map_err(|e| {
        ProviderError::new(
            "transport",
            redact(&format!("nanobanana transport error: {e}"), key),
            http::retryable(&e),
        )
    })?;
    let content_type = http::content_type(&resp);
    Ok(Resp {
        status: resp.status,
        body: resp.body,
        content_type,
    })
}

fn error_text(body: &[u8], key: &str) -> String {
    let v: Option<Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| {
            v.get("msg")
                .or_else(|| v.get("message"))
                .or_else(|| v.get("error"))
        })
        .map(|d| match d {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    redact(&msg.chars().take(300).collect::<String>(), key)
}

fn map_code(code: u16, text: &str) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let billing = lower.contains("insufficient")
        || lower.contains("credits") && lower.contains("exhaust")
        || lower.contains("balance");
    match code {
        401 | 403 if !billing => ProviderError::new(
            "auth",
            format!("nanobanana rejected the API key ({code}): {text}"),
            false,
        ),
        401..=403 => ProviderError::new(
            "billing",
            format!("nanobanana billing problem: {text}"),
            false,
        ),
        408 | 429 => ProviderError::new("rate_limited", format!("nanobanana {code}: {text}"), true),
        500..=599 => ProviderError::new("unavailable", format!("nanobanana {code}: {text}"), true),
        c => ProviderError::new("invalid_request", format!("nanobanana {c}: {text}"), false),
    }
}

/// Maps a non-2xx HTTP status.
fn map_status(r: &Resp, key: &str) -> ProviderError {
    map_code(r.status, &error_text(&r.body, key))
}

/// Parses a 2xx reply as JSON and maps a body-level `code` other than 200.
fn parse_ok(r: &Resp, key: &str, what: &str) -> Result<Value, ProviderError> {
    if !(200..300).contains(&r.status) {
        return Err(map_status(r, key));
    }
    let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
        ProviderError::new(
            "bad_response",
            format!("nanobanana {what} reply was not JSON"),
            true,
        )
    })?;
    if let Some(c) = v.get("code").and_then(Value::as_u64).filter(|c| *c != 200) {
        return Err(map_code(
            u16::try_from(c).unwrap_or(400),
            &error_text(&r.body, key),
        ));
    }
    Ok(v)
}

fn mime_from_url(url: &str) -> Option<String> {
    super::fal::mime_from_url(url)
}

impl Provider for NanoBanana {
    fn id(&self) -> &'static str {
        "nanobanana"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    /// `params.image_urls` (public http(s) URLs; local files go through fal storage with confirmation).
    /// `image_urls` has no count check in this adapter, so no `max` is declared.
    fn references(&self, _model: &str) -> Option<ReferenceInput> {
        Some(ReferenceInput {
            param: "image_urls",
            max: None,
            local_upload: "fal_storage",
        })
    }

    /// `aspect` and `resolution` are the tables `aspect` and `resolution` check (2K when absent); no seed is sent.
    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities::default()
            .mode("prompt")
            .setting("aspect", capabilities::aspect("aspect", &ASPECTS, false))
            .setting(
                "resolution",
                capabilities::resolution("resolution", &["1K", "2K", "4K"], false, Some("2K")),
            )
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.check(req)?;
        Ok(match resolution(req)? {
            "1K" => PRICE_1K,
            "4K" => PRICE_4K,
            _ => PRICE_2K,
        })
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.check(req)?;
        let mut body = json!({
            "prompt": req.prompt,
            "imageUrls": image_urls(req)?,
            "aspectRatio": aspect(req),
            "resolution": resolution(req)?,
            "googleSearch": req.params.get("google_search").and_then(Value::as_bool).unwrap_or(false),
            "outputFormat": match req.params.get("output_format").and_then(Value::as_str) {
                Some("jpg") => "jpg",
                _ => "png",
            },
        });
        body["prompt"] = json!(req.prompt);
        let sent = Request::post(format!("{}{}", self.base, GENERATE_PATH))
            .secret_header("Authorization", format!("Bearer {api_key}"))
            .max_response_bytes(BODY_LIMIT as usize)
            .json(&body)
            .and_then(|req| self.agent.send(req));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        // After a reply, a retryable error is a 408/429/5xx (HTTP or body code) or an unreadable body.
        let v = parse_ok(&r, api_key, "submit").map_err(ProviderError::ambiguous_if_retryable)?;
        let id = v
            .pointer("/data/taskId")
            .or_else(|| v.get("taskId"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderError::new(
                    "bad_response",
                    "nanobanana submit reply had no taskId",
                    true,
                )
                .ambiguous()
            })?;
        Ok(SubmitOutcome::Accepted {
            provider_job_id: id.to_string(),
        })
    }

    fn poll(
        &self,
        _req: &GenRequest,
        provider_job_id: &str,
        api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        let enc: String = provider_job_id
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        let r = self.get(&format!("{STATUS_PATH}?taskId={enc}"), api_key)?;
        let v = parse_ok(&r, api_key, "status")?;
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        match data.get("successFlag").and_then(Value::as_i64) {
            Some(0) | None => Ok(PollOutcome::Running),
            Some(1) => {
                let url = data
                    .pointer("/response/resultImageUrl")
                    .or_else(|| data.pointer("/info/resultImageUrl"))
                    .or_else(|| data.get("resultImageUrl"))
                    .and_then(Value::as_str)
                    .filter(|u| !u.is_empty());
                match url {
                    Some(u) => Ok(PollOutcome::Completed {
                        outputs: vec![Output::Url {
                            url: u.to_string(),
                            mime: mime_from_url(u),
                        }],
                    }),
                    None => Ok(unknown_outcome(
                        "successFlag 1, no image URL",
                        &r.body,
                        api_key,
                        true,
                    )),
                }
            }
            // Confirmed-failure vocabulary (closed list): successFlag 2 (task creation failed) and 3
            // (generation failed). Any other flag value is `Unknown` and held.
            Some(f @ (2 | 3)) => {
                let m = data
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| v.get("msg").and_then(Value::as_str))
                    .unwrap_or("generation failed");
                Ok(PollOutcome::Failed {
                    message: redact(&format!("nanobanana failed (flag {f}): {m}"), api_key),
                })
            }
            Some(f) => Ok(unknown_outcome(
                &format!("successFlag {f}"),
                &r.body,
                api_key,
                false,
            )),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /api/v1/common/credit is free; `data` is the remaining credit balance.
        let r = self.get(CREDIT_PATH, api_key)?;
        let v = parse_ok(&r, api_key, "credit")?;
        if v.get("data").map(|d| d.is_number()).unwrap_or(false) {
            Ok(())
        } else {
            Err(ProviderError::new(
                "bad_response",
                "nanobanana credit reply had no numeric data",
                true,
            ))
        }
    }

    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        match output {
            Output::Bytes { bytes, mime } => Ok((bytes.clone(), mime.clone())),
            Output::Path { .. } | Output::Finalized { .. } => Err(ProviderError::new(
                "unsupported",
                "path outputs are adopted by the library",
                false,
            )),
            Output::Url { url, mime } => {
                // No credentials: result URLs are on the provider's CDN, not the API host.
                let r = read(
                    self.agent
                        .send(Request::get(url.as_str()).max_response_bytes(BODY_LIMIT as usize)),
                    "",
                )?;
                if !(200..300).contains(&r.status) {
                    return Err(map_status(&r, ""));
                }
                let m = r
                    .content_type
                    .clone()
                    .filter(|c| !c.is_empty() && c != "application/octet-stream")
                    .or_else(|| mime.clone())
                    .or_else(|| mime_from_url(url))
                    .unwrap_or_else(|| "application/octet-stream".into());
                Ok((r.body, m))
            }
        }
    }
}
