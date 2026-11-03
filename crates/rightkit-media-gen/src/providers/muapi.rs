//! Muapi aggregator adapter (generic models): any catalogue model, image or video.
//!
//! Sources (fetched 2026-10-17):
//! - <https://www.muapi.ai/docs/pricing>: the public catalogue `GET https://api.muapi.ai/api/v1/models` (no key)
//!   lists each model's `cost` in USD; for fixed-price models it is "exactly what you'll be charged per call";
//!   `dynamic_pricing: true` models need `POST /api/v1/models/{name}/estimate-cost` with the generate body.
//! - <https://muapi.ai/docs/quick-start>: `POST /api/v1/{model}` with the model JSON body returns `request_id`;
//!   `GET /api/v1/predictions/{request_id}/result` polls.
//! - <https://muapi.ai/docs/api-reference>: auth header `x-api-key`; poll reply `{id, status, outputs[], cost{amount_usd,..}}`;
//!   statuses queued | pending | processing | completed | failed | cancelled; `GET /api/v1/account/balance`.
//!
//! Atlas Cloud was rejected: its docs say billing varies by model and resolution but publish no per-request
//! numbers (<https://atlascloud.ai/docs/models/price>, fetched 2026-10-17).
//!
//! Request contract: `model` is `muapi-image` or `muapi-video` (sets the output kind); `params.muapi_model` = catalogue
//! `name`; `params.input` = the model's body; `params.estimate_usd` = REQUIRED positive price copied from the
//! catalogue `cost` (or the estimate-cost reply). Without it the adapter refuses with `price_unknown`.
//! UNVERIFIED: the submit reply field name `request_id` (prose only, no JSON example fetched); whether a
//! failed or cancelled job is billed (poll `cost.refunded` hints at refunds); that the balance endpoint is free
//! for a key check; error-body shape and HTTP codes (not documented in the fetched pages).
use super::{
    request_may_have_reached, status_may_have_run, unknown_outcome, Durations, GenRequest, Kind,
    ModelInfo, Output, PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.muapi.ai";
const IMAGE: &str = "muapi-image";
const VIDEO: &str = "muapi-video";
const BODY_LIMIT: usize = 1 << 30;
const MAX_ESTIMATE_USD: f64 = 50.0;

static MODELS: [ModelInfo; 2] = [
    ModelInfo {
        provider: "muapi",
        model: IMAGE,
        kind: Kind::Image,
        label: "Muapi: any image model (set params.muapi_model and params.estimate_usd from the catalogue)",
        est_usd: 0.05,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "muapi",
        model: VIDEO,
        kind: Kind::Video,
        label: "Muapi: any video model (set params.muapi_model and params.estimate_usd from the catalogue)",
        est_usd: 0.5,
        durations: Durations::NONE,
    },
];

pub struct Muapi {
    base: String,
    agent: Http,
}

impl Muapi {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent: Http::new(Duration::from_secs(10), Duration::from_secs(300)),
        }
    }

    fn get(&self, url: &str, key: Option<&str>) -> Result<Response, ProviderError> {
        let mut req = Request::get(url).max_response_bytes(BODY_LIMIT);
        if let Some(k) = key {
            req = req.secret_header("x-api-key", k.to_string());
        }
        read(self.agent.send(req), key.unwrap_or(""))
    }
}

fn bad(msg: impl Into<String>) -> ProviderError {
    ProviderError::new("invalid_request", msg, false)
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

struct Plan {
    name: String,
    input: Value,
    usd: f64,
}

fn resolve(req: &GenRequest) -> Result<Plan, ProviderError> {
    let info = MODELS
        .iter()
        .find(|m| m.model == req.model)
        .ok_or_else(|| bad(format!("unknown muapi model {}", req.model)))?;
    if req.kind != info.kind {
        return Err(bad("model does not match request kind"));
    }
    let name = req
        .params
        .get("muapi_model")
        .and_then(Value::as_str)
        .filter(|s| name_ok(s))
        .ok_or_else(|| bad("params.muapi_model must be a catalogue model name"))?;
    let usd = req
        .params
        .get("estimate_usd")
        .and_then(Value::as_f64)
        .filter(|u| u.is_finite() && *u > 0.0)
        .ok_or_else(|| {
            ProviderError::new(
                "price_unknown",
                "muapi prices vary per model: pass params.estimate_usd from the catalogue cost",
                false,
            )
        })?;
    if usd > MAX_ESTIMATE_USD {
        return Err(bad(format!(
            "params.estimate_usd above the ${MAX_ESTIMATE_USD} per-request ceiling"
        )));
    }
    let mut input = req
        .params
        .get("input")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let obj = input
        .as_object_mut()
        .ok_or_else(|| bad("params.input must be an object"))?;
    if !req.prompt.trim().is_empty() && !obj.contains_key("prompt") {
        obj.insert("prompt".into(), json!(req.prompt));
    }
    if obj.is_empty() {
        return Err(bad("prompt or params.input is required"));
    }
    Ok(Plan {
        name: name.to_string(),
        input,
        usd,
    })
}

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

fn read(r: Result<Response, HttpError>, key: &str) -> Result<Response, ProviderError> {
    r.map_err(|e| {
        ProviderError::new(
            "transport",
            redact(&format!("muapi transport error: {e}"), key),
            http::retryable(&e),
        )
    })
}

fn error_text(body: &[u8], key: &str) -> String {
    let v: Option<Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| {
            v.get("detail")
                .or_else(|| v.get("message"))
                .or_else(|| v.get("error"))
        })
        .map(|d| {
            d.as_str()
                .map(String::from)
                .unwrap_or_else(|| d.to_string())
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    redact(&msg.chars().take(300).collect::<String>(), key)
}

fn map_status(status: u16, text: String) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let billing = ["insufficient", "credit", "balance", "billing", "payment"]
        .iter()
        .any(|n| lower.contains(n));
    match status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("muapi rejected the API key ({status}): {text}"),
            false,
        ),
        402 => ProviderError::new("billing", format!("muapi billing problem: {text}"), false),
        s if billing && (400..500).contains(&s) && s != 408 && s != 429 => {
            ProviderError::new("billing", format!("muapi billing problem: {text}"), false)
        }
        408 | 429 => ProviderError::new("rate_limited", format!("muapi {status}: {text}"), true),
        500..=599 => ProviderError::new("unavailable", format!("muapi {status}: {text}"), true),
        s => ProviderError::new("invalid_request", format!("muapi {s}: {text}"), false),
    }
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Provider for Muapi {
    fn id(&self) -> &'static str {
        "muapi"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        Ok(resolve(req)?.usd)
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        let plan = resolve(req)?;
        let url = format!("{}/api/v1/{}", self.base, plan.name);
        let sent = Request::post(url)
            .secret_header("x-api-key", api_key.to_string())
            .max_response_bytes(BODY_LIMIT)
            .json(&plan.input)
            .and_then(|r| self.agent.send(r));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key))
                .ambiguous_if(status_may_have_run(r.status)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "muapi submit reply was not JSON", true).ambiguous()
        })?;
        let id = v
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|i| safe_id(i))
            .ok_or_else(|| {
                ProviderError::new(
                    "bad_response",
                    "muapi submit reply had no usable request_id",
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
        if !safe_id(provider_job_id) {
            return Err(bad("invalid muapi request id"));
        }
        let r = self.get(
            &format!(
                "{}/api/v1/predictions/{}/result",
                self.base, provider_job_id
            ),
            Some(api_key),
        )?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "muapi result reply was not JSON", true)
        })?;
        match v.get("status").and_then(Value::as_str).unwrap_or("") {
            "queued" | "pending" | "processing" => Ok(PollOutcome::Running),
            "completed" => {
                let outputs: Vec<Output> = v
                    .get("outputs")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                    .map(|u| Output::Url {
                        url: u.to_string(),
                        mime: super::fal::mime_from_url(u),
                    })
                    .collect();
                if outputs.is_empty() {
                    Ok(unknown_outcome(
                        "completed, no file output",
                        &r.body,
                        api_key,
                        true,
                    ))
                } else {
                    Ok(PollOutcome::Completed { outputs })
                }
            }
            // Confirmed-failure vocabulary (closed list): failed, cancelled. Anything else is held.
            "failed" | "cancelled" => {
                let m = v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                Ok(PollOutcome::Failed {
                    message: redact(&format!("muapi job failed: {m}"), api_key),
                })
            }
            "" => Err(ProviderError::new(
                "bad_response",
                "muapi result reply had no status",
                true,
            )),
            other => Ok(unknown_outcome(other, &r.body, api_key, false)),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /api/v1/account/balance (free-ness UNVERIFIED) requires a valid key.
        let r = self.get(
            &format!("{}/api/v1/account/balance", self.base),
            Some(api_key),
        )?;
        if (200..300).contains(&r.status) {
            Ok(())
        } else {
            Err(map_status(r.status, error_text(&r.body, api_key)))
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
                // No credentials: result files are public CDN links.
                let r = self.get(url, None)?;
                if !(200..300).contains(&r.status) {
                    return Err(map_status(r.status, error_text(&r.body, "")));
                }
                let m = http::content_type(&r)
                    .filter(|c| !c.is_empty() && c != "application/octet-stream")
                    .or_else(|| mime.clone())
                    .or_else(|| super::fal::mime_from_url(url))
                    .unwrap_or_else(|| "application/octet-stream".into());
                Ok((r.body, m))
            }
        }
    }
}
