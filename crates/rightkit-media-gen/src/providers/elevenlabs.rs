//! ElevenLabs text-to-speech adapter (voice, synchronous).
//!
//! Sources (checked 2026-10-05):
//! - TTS endpoint: <https://elevenlabs.io/docs/api-reference/text-to-speech/convert>
//! - Model ids: <https://elevenlabs.io/docs/overview/models> (`eleven_v4`,
//!   `eleven_v4_turbo`, `eleven_flash_v2_5` confirmed there)
//! - API list prices: <https://elevenlabs.io/pricing/api>
//!   (v4 / multilingual v2 / v3: $0.08 per 1K chars; v4 turbo / flash: $0.04).
//!   The v4 page also showed a temporary 72% promo ($0.022) until Oct 12; the
//!   estimate deliberately uses the undiscounted list price (conservative).
//!
//! Actual cost: the TTS response carries a `character-cost` header (plus
//! `request-id`, `x-trace-id`) per ElevenLabs' SDK raw-response usage; it is NOT
//! in the REST reference. The Provider trait has no cost return, so the
//! estimate stands in for the actual cost for now; the lead can read
//! `character-cost` later (billed characters; multiply by the per-1K price).
//!
//! Plan limits: Free-plan API keys cannot use library voices (402 "Free users
//! cannot use library voices via the API"); that maps to `voice_unavailable`.
use super::{
    request_may_have_reached, status_may_have_run, Durations, GenRequest, Kind, ModelInfo, Output,
    PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use crate::capabilities::{self, Capabilities};
use crate::host::http::{retryable, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.elevenlabs.io";

/// Premade (non-library) voice "George" (`JBFqnCBsd6RMkjVDRZzb`), the voice in
/// every example of ElevenLabs' TTS API reference. Premade voices are part of
/// the default voice set, so Free-plan API keys can use them.
const DEFAULT_VOICE: &str = "JBFqnCBsd6RMkjVDRZzb";
const OUTPUT_FORMAT: &str = "mp3_44100_128";

const V4: &str = "eleven_v4";
const V4_TURBO: &str = "eleven_v4_turbo";
const FLASH: &str = "eleven_flash_v2_5";
const MUSIC: &str = "music_v1";
const SFX: &str = "eleven_text_to_sound_v2";

// `ModelInfo::est_usd` assumes a ~500-character reference script.
//
// Music and SFX are priced per minute of generated audio. ElevenLabs list prices:
// Music 0.15 USD per minute; Sound Effects 0.12 USD per minute.
// Source: https://elevenlabs.io/pricing/api, read 2026-10-11.
// The Music and SFX rows show one minute; `estimate_usd` uses the requested length, or the route
// maximum below when a request gives none.
const MUSIC_USD_PER_MINUTE: f64 = 0.15;
const SFX_USD_PER_MINUTE: f64 = 0.12;
// Longest audio each route accepts (`check` enforces these limits).
const MUSIC_MAX_MS: u64 = 600_000;
const SFX_MAX_SECONDS: f64 = 30.0;

static MODELS: [ModelInfo; 5] = [
    ModelInfo {
        provider: "elevenlabs",
        model: V4_TURBO,
        kind: Kind::Voice,
        label: "Eleven v4 Turbo (draft default) - $0.04/1K chars",
        est_usd: 0.02,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "elevenlabs",
        model: V4,
        kind: Kind::Voice,
        label: "Eleven v4 (final) - $0.08/1K chars",
        est_usd: 0.04,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "elevenlabs",
        model: FLASH,
        kind: Kind::Voice,
        label: "Eleven Flash v2.5 (lowest latency, $0.04/1K chars)",
        est_usd: 0.02,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "elevenlabs",
        model: MUSIC,
        kind: Kind::Music,
        label: "ElevenLabs Music text-to-music (params.music_length_ms or composition_plan)",
        est_usd: MUSIC_USD_PER_MINUTE,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "elevenlabs",
        model: SFX,
        kind: Kind::Sfx,
        label: "ElevenLabs text-to-sound (params.duration_seconds, loop, prompt_influence)",
        est_usd: SFX_USD_PER_MINUTE,
        durations: Durations::NONE,
    },
];

/// (model id, USD per 1K characters, max characters per request)
fn model_terms(model: &str) -> Option<(f64, usize)> {
    match model {
        V4 => Some((0.08, 10_000)),
        V4_TURBO => Some((0.04, 40_000)),
        FLASH => Some((0.04, 40_000)),
        _ => None,
    }
}

pub struct ElevenLabs {
    base: String,
    agent: Http,
}

impl ElevenLabs {
    /// `base` is the resolved endpoint (see `providers::base_url_override`).
    pub fn new(base: &str) -> Self {
        let agent = Http::new(Duration::from_secs(10), Duration::from_secs(180));
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent,
        }
    }

    /// Voice: (USD per 1K chars, char cap). Music/SFX: the flat catalog
    /// estimate is used instead.
    fn check(&self, req: &GenRequest) -> Result<(f64, usize), ProviderError> {
        match req.kind {
            Kind::Voice => {
                let (price, max) = model_terms(&req.model).ok_or_else(|| {
                    ProviderError::new(
                        "invalid_request",
                        format!("unknown elevenlabs model {}", req.model),
                        false,
                    )
                })?;
                if req.prompt.trim().is_empty() {
                    return Err(ProviderError::new(
                        "invalid_request",
                        "text is empty",
                        false,
                    ));
                }
                if req.prompt.chars().count() > max {
                    return Err(ProviderError::new(
                        "invalid_request",
                        format!("text exceeds {max} characters for {}", req.model),
                        false,
                    ));
                }
                Ok((price, max))
            }
            Kind::Music => {
                if req.model != MUSIC {
                    return Err(ProviderError::new(
                        "invalid_request",
                        format!("unknown elevenlabs music model {}", req.model),
                        false,
                    ));
                }
                if req.prompt.trim().is_empty()
                    && req
                        .params
                        .get("composition_plan")
                        .and_then(Value::as_object)
                        .is_none()
                {
                    return Err(ProviderError::new(
                        "invalid_request",
                        "music needs a prompt or params.composition_plan",
                        false,
                    ));
                }
                if let Some(ms) = req
                    .params
                    .get("music_length_ms")
                    .or_else(|| req.params.get("duration_ms"))
                {
                    let ok = ms
                        .as_u64()
                        .or_else(|| ms.as_str().and_then(|s| s.parse().ok()))
                        .is_some_and(|n| (3_000..=MUSIC_MAX_MS).contains(&n));
                    if !ok {
                        return Err(ProviderError::new(
                            "invalid_request",
                            "music_length_ms must be 3000-600000",
                            false,
                        ));
                    }
                }
                Ok((0.0, usize::MAX))
            }
            Kind::Sfx => {
                if req.model != SFX {
                    return Err(ProviderError::new(
                        "invalid_request",
                        format!("unknown elevenlabs sfx model {}", req.model),
                        false,
                    ));
                }
                if req.prompt.trim().is_empty() {
                    return Err(ProviderError::new(
                        "invalid_request",
                        "sound description is empty",
                        false,
                    ));
                }
                if let Some(d) = req.params.get("duration_seconds") {
                    let ok = d
                        .as_f64()
                        .or_else(|| d.as_str().and_then(|s| s.parse().ok()))
                        .is_some_and(|n| (0.5..=SFX_MAX_SECONDS).contains(&n));
                    if !ok {
                        return Err(ProviderError::new(
                            "invalid_request",
                            "duration_seconds must be 0.5-30",
                            false,
                        ));
                    }
                }
                Ok((0.0, usize::MAX))
            }
            _ => Err(ProviderError::new(
                "invalid_request",
                "elevenlabs produces voice, music and sfx only",
                false,
            )),
        }
    }

    /// Body + URL for the synchronous audio endpoints. All three return the
    /// audio bytes directly.
    fn audio_call(&self, req: &GenRequest) -> Result<(String, Value), ProviderError> {
        match req.kind {
            Kind::Voice => {
                let voice = req
                    .params
                    .get("voice_id")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .unwrap_or(DEFAULT_VOICE);
                if !voice
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(ProviderError::new(
                        "invalid_request",
                        "voice_id has invalid characters",
                        false,
                    ));
                }
                let mut body = json!({ "text": req.prompt, "model_id": req.model });
                if let Some(vs) = req.params.get("voice_settings").filter(|v| v.is_object()) {
                    body["voice_settings"] = vs.clone();
                }
                Ok((
                    format!(
                        "{}/v1/text-to-speech/{voice}?output_format={OUTPUT_FORMAT}",
                        self.base
                    ),
                    body,
                ))
            }
            Kind::Music => {
                // POST /v1/music: either a text prompt or a composition_plan
                // object; length is milliseconds.
                let mut body = json!({ "model_id": req.model });
                if let Some(plan) = req.params.get("composition_plan").filter(|v| v.is_object()) {
                    body["composition_plan"] = plan.clone();
                } else {
                    body["prompt"] = json!(req.prompt);
                }
                let ms = req
                    .params
                    .get("music_length_ms")
                    .or_else(|| req.params.get("duration_ms"))
                    .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
                if let Some(ms) = ms {
                    body["music_length_ms"] = json!(ms);
                }
                Ok((format!("{}/v1/music", self.base), body))
            }
            Kind::Sfx => {
                // POST /v1/sound-generation.
                let mut body = json!({ "text": req.prompt, "model_id": req.model });
                if let Some(d) = req.params.get("duration_seconds") {
                    body["duration_seconds"] = d.clone();
                }
                if let Some(i) = req.params.get("prompt_influence") {
                    body["prompt_influence"] = i.clone();
                }
                if let Some(l) = req.params.get("loop") {
                    body["loop"] = l.clone();
                }
                Ok((format!("{}/v1/sound-generation", self.base), body))
            }
            _ => Err(ProviderError::new(
                "invalid_request",
                "elevenlabs produces audio only",
                false,
            )),
        }
    }
}

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

/// Each request sets its own body cap with `max_response_bytes(256 << 20)`.
// Every send failure, including a failed body read, is a "transport" error; `retryable` sets the flag.
fn read(r: Result<Response, HttpError>, key: &str) -> Result<Resp, ProviderError> {
    let resp = r.map_err(|e| {
        ProviderError::new(
            "transport",
            redact(&format!("elevenlabs transport error: {e}"), key),
            retryable(&e),
        )
    })?;
    Ok(Resp {
        status: resp.status,
        body: resp.body,
    })
}

fn error_text(body: &[u8], key: &str) -> String {
    let v: Option<Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.get("detail"))
        .map(|d| match d {
            Value::String(s) => s.clone(),
            Value::Object(o) => o
                .get("message")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| d.to_string()),
            other => other.to_string(),
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    redact(&msg.chars().take(300).collect::<String>(), key)
}

fn map_status(r: &Resp, key: &str) -> ProviderError {
    let text = error_text(&r.body, key);
    let lower = text.to_ascii_lowercase();
    let plan_locked = [
        "library voice",
        "upgrade your subscription",
        "voice_not_found",
        "voice_access",
        "paid_plan_required",
    ]
    .iter()
    .any(|k| lower.contains(k));
    match r.status {
        402 | 403 if plan_locked => ProviderError::new(
            "voice_unavailable",
            "This ElevenLabs voice isn't available on your plan via the API; pick a premade voice or use WaveSpeed \u{203a} ElevenLabs Turbo v2.5.",
            false,
        ),
        401 | 403 => ProviderError::new("auth", format!("elevenlabs rejected the API key ({}): {text}", r.status), false),
        408 | 429 => ProviderError::new("rate_limited", format!("elevenlabs {}: {text}", r.status), true),
        500..=599 => ProviderError::new("unavailable", format!("elevenlabs {}: {text}", r.status), true),
        s => ProviderError::new("invalid_request", format!("elevenlabs {s}: {text}"), false),
    }
}

/// Music and SFX estimates: the per-minute price times the requested length. With no length the
/// estimate uses the route maximum (`MUSIC_MAX_MS`, `SFX_MAX_SECONDS`), the longest `check` accepts.
/// A request with no length is sent without one, so the API picks its own length; reserving for the
/// maximum keeps the estimate at or above what the request could be charged.
fn media_usd(req: &GenRequest) -> Result<f64, ProviderError> {
    match req.kind {
        Kind::Music => {
            let ms = req
                .params
                .get("music_length_ms")
                .or_else(|| req.params.get("duration_ms"))
                .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                .unwrap_or(MUSIC_MAX_MS);
            Ok(MUSIC_USD_PER_MINUTE * ms as f64 / 60_000.0)
        }
        _ => {
            let secs = req
                .params
                .get("duration_seconds")
                .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
                .unwrap_or(SFX_MAX_SECONDS);
            Ok(SFX_USD_PER_MINUTE * secs / 60.0)
        }
    }
}

impl Provider for ElevenLabs {
    fn id(&self) -> &'static str {
        "elevenlabs"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    /// Voice: `audio_call` takes `voice_id` of letters, digits, `_` and `-`. Music: `check` takes
    /// `music_length_ms` as whole milliseconds, 3000 to `MUSIC_MAX_MS`. Sfx: `duration_seconds`, 0.5 to
    /// `SFX_MAX_SECONDS`, and `loop`.
    fn capabilities(&self, model: &str) -> Capabilities {
        match model {
            MUSIC => Capabilities::default().mode("prompt").setting(
                "length",
                capabilities::length("music_length_ms", "ms", 3_000.0, MUSIC_MAX_MS as f64, true),
            ),
            SFX => Capabilities::default()
                .mode("prompt")
                .setting(
                    "length",
                    capabilities::length("duration_seconds", "s", 0.5, SFX_MAX_SECONDS, false),
                )
                .setting("loop", serde_json::json!({ "param": "loop" })),
            _ => Capabilities::default().mode("script").input(
                "voice_id",
                serde_json::json!({ "param": "voice_id", "pattern": "^[A-Za-z0-9_-]+$" }),
            ),
        }
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.check(req)?;
        if req.kind == Kind::Voice {
            // Assumption: undiscounted API list price per character (no plan credit applied).
            let (price, _) = model_terms(&req.model).unwrap();
            Ok(req.prompt.chars().count() as f64 / 1000.0 * price)
        } else {
            media_usd(req)
        }
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.check(req)?;
        let (url, body) = self.audio_call(req)?;
        let sent = Request::post(url.as_str())
            .secret_header("xi-api-key", api_key)
            .max_response_bytes(256 << 20)
            .json(&body)
            .and_then(|req| self.agent.send(req));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(&r, api_key).ambiguous_if(status_may_have_run(r.status)));
        }
        if r.body.is_empty() {
            return Err(
                ProviderError::new("bad_response", "elevenlabs returned no audio", true)
                    .ambiguous(),
            );
        }
        Ok(SubmitOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes: r.body,
                mime: "audio/mpeg".into(),
            }],
        })
    }

    fn poll(&self, _req: &GenRequest, _id: &str, _key: &str) -> Result<PollOutcome, ProviderError> {
        Err(ProviderError::new(
            "unsupported",
            "elevenlabs audio endpoints are synchronous; nothing to poll",
            false,
        ))
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /v1/user is free. Restricted keys without the user_read scope get
        // 401 here even though they can synthesize speech.
        let r = read(
            self.agent.send(
                Request::get(format!("{}/v1/user", self.base))
                    .secret_header("xi-api-key", api_key)
                    .max_response_bytes(256 << 20),
            ),
            api_key,
        )?;
        if (200..300).contains(&r.status) {
            return Ok(());
        }
        if r.status == 401 {
            // Restricted key without user_read: /v1/models is also free.
            let m = read(
                self.agent.send(
                    Request::get(format!("{}/v1/models", self.base))
                        .secret_header("xi-api-key", api_key)
                        .max_response_bytes(256 << 20),
                ),
                api_key,
            )?;
            if (200..300).contains(&m.status) {
                return Ok(());
            }
            return Err(map_status(&m, api_key));
        }
        Err(map_status(&r, api_key))
    }

    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        match output {
            Output::Bytes { bytes, mime } => Ok((bytes.clone(), mime.clone())),
            Output::Url { .. } => Err(ProviderError::new(
                "unsupported",
                "elevenlabs returns bytes, not URLs",
                false,
            )),
            Output::Path { .. } | Output::Finalized { .. } => Err(ProviderError::new(
                "unsupported",
                "path outputs are adopted by the library",
                false,
            )),
        }
    }
}
