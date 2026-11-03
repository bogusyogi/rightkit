//! HeyGen avatar-video adapter (avatar, asynchronous; API only).
//!
//! Endpoints (HeyGen API v3, auth header `X-Api-Key`; request/response shapes
//! read from the live reference on 2026-10-06:
//! <https://developers.heygen.com/reference/create-video>, `get-video`,
//! `get-current-user`):
//! - `POST /v3/videos` with `{type:"avatar", avatar_id, engine, script[+voice_id] |
//!   audio_url, resolution, aspect_ratio, output_format, remove_background?}`
//!   -> `{data:{video_id,status}}`. `engine` is `avatar_iii` (model
//!   `avatar-standard`, default) or `avatar_iv` (model `avatar-iv`). Per the
//!   docs `voice_id` may be omitted to use the avatar's default voice.
//! - `GET /v3/videos/{video_id}` -> `{data:{status: pending|processing|completed|failed,
//!   video_url, failure_message}}`
//! - `GET /v3/users/me` (free) is the key check; a wallet with a remaining
//!   balance of zero or less is reported as `billing`.
//!
//! Price (pay-as-you-go API wallet). The official pricing page
//! (`developers.heygen.com/docs/api-pricing`, `heygen.com/api-pricing`) could
//! NOT be read on 2026-10-06 (404 / redirect to a login-gated app modal), so
//! these are secondary-source figures, unverified against HeyGen itself
//! (search of 2026-10-06 summarizing diyai.io / g2.com / anam.ai):
//! Avatar III ~ $1/min ($0.0167/s) -> `avatar-standard`; Avatar IV ~ $3/min
//! ($0.05/s), Avatar V ~ $4/min ($0.0667/s). `avatar-standard` uses $0.0167/s;
//! `avatar-iv` deliberately keeps the conservative workspace-router value
//! $0.0667/s (~$4/min) until the official page is confirmed.
//! Duration (the estimate is reserved against the spend limits, so it is an UPPER bound the
//! engine derives from what it sends, never from a number the caller states): the script at
//! the larger of ~150 words per minute (2.5 words/s, the adapter's original figure) and a
//! slow-speech bound of 8 characters/s for most scripts or 3 characters/s for CJK characters,
//! counted in Unicode scalar values. `params.duration_secs` is not sent to HeyGen; it can only
//! RAISE the estimate. An `audio_url` is a remote https URL the engine cannot measure and
//! HeyGen's maximum is not documented, so an audio-only request is refused until the engine
//! can measure the audio. HeyGen has no published per-call quote.
//!
//! No stock avatar or voice id is documented as available to all accounts, so
//! `params.avatar_id` is required; `params.voice_id` is optional (default voice).
use super::{
    request_may_have_reached, status_may_have_run, unknown_outcome, Durations, GenRequest, Kind,
    ModelInfo, Output, PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use crate::capabilities::{self, Capabilities};
use crate::host::http::{retryable, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.heygen.com";

const AVATAR_STANDARD: &str = "avatar-standard";
const AVATAR_IV: &str = "avatar-iv";
const STANDARD_USD_PER_SEC: f64 = 0.0167;
const IV_USD_PER_SEC: f64 = 0.0667;
const WORDS_PER_SEC: f64 = 2.5;
/// Slow-speech bound for scripts: characters per second of narration, spaces and punctuation
/// included. About 100 words per minute at 5 characters plus a space per word; faster speech
/// is billed less, so the bound errs high.
const SCRIPT_CHARS_PER_SEC: f64 = 8.0;
/// The same bound for Chinese, Japanese and Korean characters, which carry a word or a syllable each.
const CJK_CHARS_PER_SEC: f64 = 3.0;

// `ModelInfo::est_usd` assumes a ~30 second video.
static MODELS: [ModelInfo; 2] = [
    ModelInfo {
        provider: "heygen",
        model: AVATAR_STANDARD,
        kind: Kind::Avatar,
        label: "HeyGen standard avatar (Avatar III, ~$1/min)",
        est_usd: 0.5,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "heygen",
        model: AVATAR_IV,
        kind: Kind::Avatar,
        label: "HeyGen Avatar IV (~$3-4/min)",
        est_usd: 2.0,
        durations: Durations::NONE,
    },
];

pub struct HeyGen {
    base: String,
    agent: Http,
}

impl HeyGen {
    /// `base` is the resolved endpoint (see `providers::base_url_override`).
    pub fn new(base: &str) -> Self {
        let agent = Http::new(Duration::from_secs(10), Duration::from_secs(300));
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent,
        }
    }

    /// `POST /v3/videos`; `idempotency_key` goes out as the `Idempotency-Key` header.
    fn create(
        &self,
        req: &GenRequest,
        api_key: &str,
        idempotency_key: Option<&str>,
    ) -> Result<SubmitOutcome, ProviderError> {
        self.check(req)?;
        let avatar = param_str(req, "avatar_id").ok_or_else(|| {
            ProviderError::new("invalid_request", "params.avatar_id is required", false)
        })?;
        let audio_url = param_str(req, "audio_url");
        let script = req.prompt.trim();
        let mut body = json!({
            "type": "avatar",
            "avatar_id": avatar,
            "engine": if req.model == AVATAR_IV { "avatar_iv" } else { "avatar_iii" },
            "resolution": param_str(req, "resolution").unwrap_or("1080p"),
            "aspect_ratio": param_str(req, "aspect_ratio")
                .or_else(|| param_str(req, "aspect"))
                .or_else(|| param_str(req, "dimension").filter(|d| d.contains(':')))
                .unwrap_or("auto"),
            "output_format": "mp4",
        });
        match (audio_url, script.is_empty()) {
            (Some(_), false) => {
                return Err(ProviderError::new(
                    "invalid_request",
                    "use a script or params.audio_url, not both",
                    false,
                ))
            }
            (None, true) => {
                return Err(ProviderError::new(
                    "invalid_request",
                    "a script (prompt) or params.audio_url is required",
                    false,
                ))
            }
            (Some(u), true) => {
                if !u.starts_with("https://") {
                    return Err(ProviderError::new(
                        "invalid_request",
                        "audio_url must be https",
                        false,
                    ));
                }
                body["audio_url"] = json!(u);
            }
            (None, false) => {
                body["script"] = json!(script);
                if let Some(v) = param_str(req, "voice_id") {
                    body["voice_id"] = json!(v);
                }
            }
        }
        let transparent = match req.params.get("background") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => {
                s.eq_ignore_ascii_case("transparent") || s.eq_ignore_ascii_case("remove")
            }
            _ => false,
        };
        if transparent || req.params.get("remove_background").and_then(Value::as_bool) == Some(true)
        {
            body["remove_background"] = json!(true);
        }
        let mut post = Request::post(format!("{}/v3/videos", self.base))
            .secret_header("X-Api-Key", api_key)
            .max_response_bytes(1 << 30);
        if let Some(k) = idempotency_key {
            post = post.header("Idempotency-Key", k);
        }
        let sent = post.json(&body).and_then(|req| self.agent.send(req));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(&r, api_key).ambiguous_if(status_may_have_run(r.status)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "heygen submit reply was not JSON", true).ambiguous()
        })?;
        let id = v
            .pointer("/data/video_id")
            .or_else(|| v.get("video_id"))
            .and_then(Value::as_str)
            .filter(|s| valid_id(s))
            .ok_or_else(|| {
                ProviderError::new("bad_response", "heygen reply had no video_id", true).ambiguous()
            })?;
        Ok(SubmitOutcome::Accepted {
            provider_job_id: id.to_string(),
        })
    }

    fn check(&self, req: &GenRequest) -> Result<(), ProviderError> {
        if req.kind != Kind::Avatar {
            return Err(ProviderError::new(
                "invalid_request",
                "heygen only produces avatar video",
                false,
            ));
        }
        if req.model != AVATAR_STANDARD && req.model != AVATAR_IV {
            return Err(ProviderError::new(
                "invalid_request",
                format!("unknown heygen model {}", req.model),
                false,
            ));
        }
        Ok(())
    }
}

fn param_str<'a>(req: &'a GenRequest, name: &str) -> Option<&'a str> {
    req.params
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    content_type: Option<String>,
}

/// Each request sets its own body cap with `max_response_bytes(1 << 30)`.
// Every send failure, including a failed body read, is a "transport" error; `retryable` sets the flag.
fn read(r: Result<Response, HttpError>, key: &str) -> Result<Resp, ProviderError> {
    let resp = r.map_err(|e| {
        ProviderError::new(
            "transport",
            redact(&format!("heygen transport error: {e}"), key),
            retryable(&e),
        )
    })?;
    // Not lower-cased, as before: only the media type before `;`, trimmed.
    let content_type = resp
        .header("content-type")
        .map(|s| s.split(';').next().unwrap_or("").trim().to_string());
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
            v.pointer("/error/message")
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

fn map_status(r: &Resp, key: &str) -> ProviderError {
    let text = error_text(&r.body, key);
    let lower = text.to_ascii_lowercase();
    let money = lower.contains("insufficient")
        || lower.contains("quota")
        || lower.contains("balance")
        || lower.contains("credit");
    if money && matches!(r.status, 400 | 402 | 403) {
        return ProviderError::new("billing", format!("heygen billing/credits: {text}"), false);
    }
    match r.status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("heygen rejected the API key ({}): {text}", r.status),
            false,
        ),
        402 => ProviderError::new("billing", format!("heygen billing/credits: {text}"), false),
        408 | 429 => {
            ProviderError::new("rate_limited", format!("heygen {}: {text}", r.status), true)
        }
        500..=599 => {
            ProviderError::new("unavailable", format!("heygen {}: {text}", r.status), true)
        }
        s => ProviderError::new("invalid_request", format!("heygen {s}: {text}"), false),
    }
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn usd_per_sec(model: &str) -> f64 {
    if model == AVATAR_IV {
        IV_USD_PER_SEC
    } else {
        STANDARD_USD_PER_SEC
    }
}

/// Whether `c` is a Chinese, Japanese or Korean character (kana, Han, Hangul).
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF)
}

/// Upper bound on the narration length of a script, in seconds, at least 1: the larger of the
/// adapter's words-per-second figure and the slow-speech characters-per-second bound.
fn script_secs(script: &str) -> f64 {
    let by_words = script.split_whitespace().count() as f64 / WORDS_PER_SEC;
    let cjk = script.chars().filter(|c| is_cjk(*c)).count();
    let other = script.chars().count() - cjk;
    let by_chars = other as f64 / SCRIPT_CHARS_PER_SEC + cjk as f64 / CJK_CHARS_PER_SEC;
    by_words.max(by_chars).max(1.0)
}

/// Seconds the estimate reserves: the script bound, raised (never lowered) by a valid
/// `params.duration_secs`. Audio-only input is refused: its length cannot be measured.
fn video_secs(req: &GenRequest) -> Result<f64, ProviderError> {
    let bad = |m: &str| ProviderError::new("invalid_request", m.to_string(), false);
    let hint = match req.params.get("duration_secs").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|d| d.is_finite() && *d > 0.0)
                .ok_or_else(|| bad("params.duration_secs must be a positive number of seconds"))?,
        ),
    };
    let script = req.prompt.trim();
    if param_str(req, "audio_url").is_some() && script.is_empty() {
        return Err(bad(
            "params.audio_url is a remote URL the engine cannot measure before submit, and heygen documents no maximum length, so the request cannot be priced; send the script as the prompt instead (local audio files are not accepted by this adapter yet)",
        ));
    }
    let bound = script_secs(script);
    Ok(hint.map_or(bound, |h| h.max(bound)))
}

impl Provider for HeyGen {
    fn id(&self) -> &'static str {
        "heygen"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    /// `create`: `avatar_id` is required; the script (the prompt) and `audio_url` (https) are alternatives, one
    /// of them required; `voice_id` applies to a script. `resolution` and `aspect_ratio` are forwarded unchecked.
    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities::default()
            .mode("script_and_avatar_id")
            .input(
                "avatar_id",
                serde_json::json!({ "param": "avatar_id", "required": true }),
            )
            .input("audio", capabilities::io("audio_url", "https_url", false))
            .input("voice_id", serde_json::json!({ "param": "voice_id" }))
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.check(req)?;
        Ok(video_secs(req)? * usd_per_sec(&req.model))
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.create(req, api_key, None)
    }

    /// Sends the attempt key as `Idempotency-Key`. Whether `/v3/videos` honours
    /// it is not established, so the job layer still treats the submit as
    /// non-idempotent (`submit_is_idempotent` stays false).
    fn submit_job(
        &self,
        req: &GenRequest,
        api_key: &str,
        _job_id: &str,
        _attempt: i64,
        idempotency_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        self.create(req, api_key, Some(idempotency_key))
    }

    fn poll(
        &self,
        _req: &GenRequest,
        id: &str,
        api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        if !valid_id(id) {
            return Err(ProviderError::new(
                "invalid_request",
                "invalid heygen video id",
                false,
            ));
        }
        let r = read(
            self.agent.send(
                Request::get(format!("{}/v3/videos/{id}", self.base))
                    .secret_header("X-Api-Key", api_key)
                    .max_response_bytes(1 << 30),
            ),
            api_key,
        )?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(&r, api_key));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "heygen status reply was not JSON", true)
        })?;
        let d = v.get("data").unwrap_or(&v);
        let status = d
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        match status.as_str() {
            "completed" | "complete" | "succeeded" | "success" => {
                let url = d
                    .get("video_url")
                    .or_else(|| d.get("url"))
                    .and_then(Value::as_str)
                    .filter(|u| !u.is_empty());
                let Some(url) = url else {
                    // HeyGen says the video finished but gave no URL: it did the work, so the job
                    // is held with the reservation settled, never re-polled to the run limit.
                    return Ok(unknown_outcome(
                        "completed, no video_url",
                        &r.body,
                        api_key,
                        true,
                    ));
                };
                Ok(PollOutcome::Completed {
                    outputs: vec![Output::Url {
                        url: url.to_string(),
                        mime: Some("video/mp4".into()),
                    }],
                })
            }
            // Confirmed-failure vocabulary (closed list): "failed" only (HeyGen's documented
            // terminal failure; the documented set is pending / processing / completed / failed,
            // unverified against a fetched page). "error", "cancelled" and every other word are
            // `Unknown` and held.
            "failed" => {
                let msg = d
                    .get("failure_message")
                    .or_else(|| d.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("heygen video failed");
                Ok(PollOutcome::Failed {
                    message: redact(&msg.chars().take(300).collect::<String>(), api_key),
                })
            }
            // In-progress video statuses; an absent status is still running.
            "" | "pending" | "processing" | "waiting" => Ok(PollOutcome::Running),
            // Anything else is unknown: hold the job (reservation kept) rather than poll until the
            // run limit or call it a failure.
            other => Ok(unknown_outcome(other, &r.body, api_key, false)),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /v3/users/me is free (profile, billing type, wallet balance).
        let r = read(
            self.agent.send(
                Request::get(format!("{}/v3/users/me", self.base))
                    .secret_header("X-Api-Key", api_key)
                    .max_response_bytes(1 << 30),
            ),
            api_key,
        )?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(&r, api_key));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "heygen users/me reply was not JSON", true)
        })?;
        let d = v.get("data").unwrap_or(&v);
        if d.get("billing_type").and_then(Value::as_str) == Some("wallet") {
            if let Some(b) = d
                .pointer("/wallet/remaining_balance")
                .and_then(Value::as_f64)
            {
                if b <= 0.0 {
                    return Err(ProviderError::new(
                        "billing",
                        "heygen wallet balance is empty",
                        false,
                    ));
                }
            }
        }
        Ok(())
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
                // No credentials: result URLs are pre-signed CDN links.
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(ProviderError::new(
                        "invalid_request",
                        "result url must be http(s)",
                        false,
                    ));
                }
                let r = read(
                    self.agent
                        .send(Request::get(url.as_str()).max_response_bytes(1 << 30)),
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
                    .unwrap_or_else(|| "video/mp4".into());
                Ok((r.body, m))
            }
        }
    }
}
