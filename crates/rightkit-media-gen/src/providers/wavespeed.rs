//! WaveSpeed adapter (video + image + voice + avatar).
//!
//! Sources (checked 2026-10-05):
//! - API: `POST {base}/api/v3/{model-slug}` returns `{code, data:{id,status}}`;
//!   `GET {base}/api/v3/predictions/{id}/result` returns
//!   `{code, data:{status: created|processing|completed|failed, outputs:[url], error}}`;
//!   auth is `Authorization: Bearer <key>`. Reference JS (read-only, copied
//!   from): `tools/backends/wavespeed.js`, `tools/backends/wavespeed-image.js`,
//!   `tools/pipelines/video/router.mjs`.
//! - Balance (free key check): `GET {base}/api/v3/balance` ->
//!   `{code:200, data:{balance: <usd>}}` (<https://wavespeed.ai/docs/docs-common-api/balance>).
//! - Kling v3.0 Pro t2v: <https://wavespeed.ai/models/kwaivgi/kling-v3.0-pro/text-to-video>
//!   (3-15 s, `aspect_ratio`, `sound`; $0.112/s, x1.5 with sound).
//!
//! - ElevenLabs Turbo v2.5 TTS: <https://wavespeed.ai/models/elevenlabs/turbo-v2.5>
//!   (`text` 1-10000 chars, `voice_id` required, `similarity`/`stability` 0-1,
//!   `use_speaker_boost`; no speed field; $0.10 per 1000 characters, prorated).
//! - MiniMax Speech-02 HD TTS: <https://wavespeed.ai/models/minimax/speech-02-hd>
//!   (`text` <=10000 chars, `voice_id` required, `speed`, `volume`, `pitch`,
//!   `emotion`, `english_normalization`, `sample_rate`, `bitrate`, `channel`,
//!   `format`, `language_boost`; $0.10 per 1000 characters (the page header
//!   says $0.05; the detailed pricing table says $0.10, which is used)).
//! - InfiniteTalk: <https://wavespeed.ai/models/wavespeed-ai/infinitetalk>
//!   (`image` + `audio` required, `resolution` 480p|720p, `prompt`, `seed`,
//!   `mask_image`; $0.03/s at 480p, $0.06/s at 720p, 3 s minimum, 600 s cap).
//!   The workspace JS reference uses the `infinitetalk-fast` slug; this adapter
//!   follows the public `infinitetalk` page. Voice/avatar slugs were not
//!   exercised against the live API.
//!
//! Voice: `prompt` is the text; `params.voice_id` (alias `params.voice`) picks
//! the voice, with a documented example voice as default. Avatar:
//! `params.image_url` and `params.audio_url` (https only) are required;
//! cost is reserved for the provider's 600 s maximum audio length, because the engine cannot
//! measure a remote audio URL before submit (real billing follows the audio length, and the
//! estimate is settled as the cost); `params.duration_secs` is never sent and cannot lower it.
//!
//! Prices: `kling-3-pro` $0.112/s (invoice-verified 2026-05-07 in the
//! reference); `seedance-2-fast` $0.10/$0.18/$0.50 per s at 480p/720p/1080p
//! (720p invoice-verified, others doc-asserted); `wan-ultra` $0.01/s and
//! `wan-hd` $0.02/s (doc-asserted); images `qwen-image` $0.02, `seedream-v4`
//! $0.027, `recraft-v4` $0.04 per image (billing-verified 2026-05-07).
//!
//! Workspace rule `docs/rules/video-pipeline.md`: WaveSpeed carries everything
//! except Veo; no silent defaults or snapping. The caller picks the model;
//! durations outside a model's range are rejected, Seedance requires an
//! explicit resolution, and Wan accepts only {5, 8} s. Audio is always sent
//! off unless the caller opts in. The endpoint is chosen from
//! `params.image_url` (image-to-video) vs. none (text-to-video); first/last
//! frame use `params.image_url` + `params.end_image_url`.
//! The model slugs for text-to-video of Seedance and Wan follow WaveSpeed's
//! naming pattern and were not exercised against the live API.
use super::{
    request_may_have_reached, transcript_output, unknown_outcome, Durations, GenRequest, Kind,
    ModelInfo, Output, PollOutcome, Provider, ProviderError, ReferenceInput, SubmitOutcome,
};
use crate::capabilities::{self, Capabilities, Need};
use crate::catalogue::{Catalogue, Entry};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.wavespeed.ai";

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Kling,
    Seedance,
    Wan,
    Happy,
    Imagen,
    Image,
    Voice,
    Avatar,
    Lipsync,
    Transcribe,
}

struct Spec {
    id: &'static str,
    family: Family,
    t2v: &'static str,
    i2v: &'static str,
    /// Flat price per second (video) or per image.
    flat: f64,
    /// Tiered $/s by resolution (Seedance); empty for flat models.
    tiers: &'static [(&'static str, f64)],
    sound_mult: f64,
    min_s: u32,
    max_s: u32,
    allowed_s: &'static [u32],
    /// Name of the end-frame field, if the model supports first+last frames.
    end_field: Option<&'static str>,
    /// Resolution baked into the endpoint (Wan).
    fixed_res: Option<&'static str>,
}

const NONE: &[(&str, f64)] = &[];

static SPECS: [Spec; 15] = [
    Spec {
        id: "kling-3-pro",
        family: Family::Kling,
        t2v: "kwaivgi/kling-v3.0-pro/text-to-video",
        i2v: "kwaivgi/kling-v3.0-pro/image-to-video",
        flat: 0.112,
        tiers: NONE,
        sound_mult: 1.5,
        min_s: 3,
        max_s: 15,
        allowed_s: &[],
        end_field: Some("end_image"),
        fixed_res: None,
    },
    Spec {
        id: "seedance-2-fast",
        family: Family::Seedance,
        t2v: "bytedance/seedance-2.0-fast/text-to-video",
        i2v: "bytedance/seedance-2.0-fast/image-to-video",
        flat: 0.0,
        tiers: &[("480p", 0.10), ("720p", 0.18), ("1080p", 0.50)],
        sound_mult: 1.0,
        min_s: 4,
        max_s: 15,
        allowed_s: &[],
        end_field: Some("last_image"),
        fixed_res: None,
    },
    Spec {
        id: "wan-ultra",
        family: Family::Wan,
        t2v: "wavespeed-ai/wan-2.2/t2v-480p-ultra-fast",
        i2v: "wavespeed-ai/wan-2.2/i2v-480p-ultra-fast",
        flat: 0.01,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 5,
        max_s: 8,
        allowed_s: &[5, 8],
        end_field: None,
        fixed_res: Some("480p"),
    },
    Spec {
        id: "wan-hd",
        family: Family::Wan,
        t2v: "wavespeed-ai/wan-2.2/t2v-720p-ultra-fast",
        i2v: "wavespeed-ai/wan-2.2/i2v-720p-ultra-fast",
        flat: 0.02,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 5,
        max_s: 8,
        allowed_s: &[5, 8],
        end_field: None,
        fixed_res: Some("720p"),
    },
    Spec {
        id: "happy-horse",
        family: Family::Happy,
        t2v: "",
        i2v: "alibaba/happyhorse-1.0/image-to-video",
        flat: 0.0,
        tiers: &[("720p", 0.14), ("1080p", 0.28)],
        sound_mult: 1.0,
        min_s: 3,
        max_s: 15,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    Spec {
        id: "imagen-4-standard",
        family: Family::Imagen,
        t2v: "google/imagen4",
        i2v: "",
        flat: 0.04,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    Spec {
        id: "imagen-4-ultra",
        family: Family::Imagen,
        t2v: "google/imagen4-ultra",
        i2v: "",
        flat: 0.05,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    Spec {
        id: "qwen-image",
        family: Family::Image,
        t2v: "wavespeed-ai/qwen-image/text-to-image",
        i2v: "",
        flat: 0.02,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    Spec {
        id: "seedream-v4",
        family: Family::Image,
        t2v: "bytedance/seedream-v4/text-to-image",
        i2v: "",
        flat: 0.027,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    Spec {
        id: "recraft-v4",
        family: Family::Image,
        t2v: "recraft-ai/recraft-v4/text-to-image",
        i2v: "",
        flat: 0.04,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    // Voice: `flat` is USD per character ($0.10 / 1000).
    Spec {
        id: "elevenlabs-turbo-v2.5",
        family: Family::Voice,
        t2v: "elevenlabs/turbo-v2.5",
        i2v: "",
        flat: 0.0001,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    Spec {
        id: "minimax-speech-02-hd",
        family: Family::Voice,
        t2v: "minimax/speech-02-hd",
        i2v: "",
        flat: 0.0001,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    // Avatar: tiered $/s of audio by resolution.
    Spec {
        id: "infinitetalk",
        family: Family::Avatar,
        t2v: "wavespeed-ai/infinitetalk",
        i2v: "",
        flat: 0.0,
        tiers: &[("480p", 0.03), ("720p", 0.06)],
        sound_mult: 1.0,
        min_s: 3,
        max_s: 600,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    // Lipsync: flat per call (per-second billing follows the audio length).
    Spec {
        id: "lipsync-3",
        family: Family::Lipsync,
        t2v: "sync/lipsync-3",
        i2v: "",
        flat: 0.05,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
    Spec {
        id: "scribe-v2",
        family: Family::Transcribe,
        t2v: "elevenlabs/scribe-v2",
        i2v: "",
        flat: TRANSCRIBE_USD_PER_MIN,
        tiers: NONE,
        sound_mult: 1.0,
        min_s: 0,
        max_s: 0,
        allowed_s: &[],
        end_field: None,
        fixed_res: None,
    },
];

const DEFAULT_SECS: u32 = 5;
const AVATAR_MIN_SECS: f64 = 3.0;
const AVATAR_MAX_SECS: f64 = 600.0;
const VOICE_MAX_CHARS: usize = 10_000;
/// WaveSpeed list price for ElevenLabs Scribe v2 (`elevenlabs/scribe-v2`): $0.01 per input minute,
/// $0.012 when keyterms are sent; billed proportionally, first 60 minutes only. Source:
/// https://wavespeed.ai/models/elevenlabs/scribe-v2, read 2026-10-17 (the page notes its prices may be
/// outdated; the final task charge governs).
const TRANSCRIBE_USD_PER_MIN: f64 = 0.01;
const TRANSCRIBE_KEYTERM_USD_PER_MIN: f64 = 0.012;
const TRANSCRIBE_CAP_MIN: f64 = 60.0;

static MODELS: [ModelInfo; 15] = [
    ModelInfo { provider: "wavespeed", model: "kling-3-pro", kind: Kind::Video, label: "Kling 3.0 Pro (3-15 s, first+last frame, $0.112/s)", est_usd: 0.56, durations: Durations::range(3, 15, Some(DEFAULT_SECS)) },
    ModelInfo { provider: "wavespeed", model: "seedance-2-fast", kind: Kind::Video, label: "Seedance 2.0 Fast (4-15 s, first+last frame, $0.10-0.50/s by resolution)", est_usd: 0.90, durations: Durations::range(4, 15, Some(DEFAULT_SECS)) },
    ModelInfo { provider: "wavespeed", model: "wan-ultra", kind: Kind::Video, label: "Wan 2.2 480p ultra-fast (5 or 8 s, $0.01/s)", est_usd: 0.05, durations: Durations::set(&[5, 8], DEFAULT_SECS) },
    ModelInfo { provider: "wavespeed", model: "wan-hd", kind: Kind::Video, label: "Wan 2.2 720p ultra-fast (5 or 8 s, $0.02/s)", est_usd: 0.10, durations: Durations::set(&[5, 8], DEFAULT_SECS) },
    ModelInfo { provider: "wavespeed", model: "happy-horse", kind: Kind::Video, label: "HappyHorse 1.0 image-to-video (3-15 s, $0.14/s 720p, $0.28/s 1080p)", est_usd: 0.70, durations: Durations::range(3, 15, Some(DEFAULT_SECS)) },
    ModelInfo { provider: "wavespeed", model: "imagen-4-standard", kind: Kind::Image, label: "Imagen 4 standard ($0.04/image)", est_usd: 0.04, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "imagen-4-ultra", kind: Kind::Image, label: "Imagen 4 Ultra ($0.05/image)", est_usd: 0.05, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "qwen-image", kind: Kind::Image, label: "Qwen Image (painterly, $0.02/image)", est_usd: 0.02, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "seedream-v4", kind: Kind::Image, label: "Seedream v4 ($0.027/image)", est_usd: 0.027, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "recraft-v4", kind: Kind::Image, label: "Recraft v4 ($0.04/image)", est_usd: 0.04, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "elevenlabs-turbo-v2.5", kind: Kind::Voice, label: "ElevenLabs Turbo v2.5 TTS ($0.10/1000 chars)", est_usd: 0.01, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "minimax-speech-02-hd", kind: Kind::Voice, label: "MiniMax Speech-02 HD TTS ($0.10/1000 chars)", est_usd: 0.01, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "infinitetalk", kind: Kind::Avatar, label: "InfiniteTalk talking avatar (image + audio, $0.03/s 480p, $0.06/s 720p, 3 s min)", est_usd: 0.30, durations: Durations::range(3, 600, None) },
    ModelInfo { provider: "wavespeed", model: "lipsync-3", kind: Kind::Lipsync, label: "Sync Lipsync-3 (video + audio, params.sync_mode)", est_usd: 0.05, durations: Durations::NONE },
    ModelInfo { provider: "wavespeed", model: "scribe-v2", kind: Kind::Transcribe, label: "ElevenLabs Scribe v2 transcription (needs params.audio_url, $0.01/audio min, $0.012 with keyterms)", est_usd: 0.60, durations: Durations::NONE },
];

/// True for a model defined in the Rust table above (catalogue entries cannot redefine it).
pub(crate) fn is_builtin(model: &str) -> bool {
    SPECS.iter().any(|s| s.id == model)
}

pub struct WaveSpeed {
    base: String,
    agent: Http,
    catalogue: &'static Catalogue,
}

impl WaveSpeed {
    /// `base` is the resolved endpoint (see `providers::base_url_override`).
    pub fn new(base: &str) -> Self {
        let agent = Http::new(Duration::from_secs(10), Duration::from_secs(300));
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent,
            catalogue: Catalogue::get(),
        }
    }

    /// Use `catalogue` for declared models instead of the process catalogue.
    #[cfg(test)]
    fn with_catalogue(mut self, catalogue: &'static Catalogue) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// The catalogue entry for a model that is not in the Rust table.
    fn declared(&self, model: &str) -> Option<&'static Entry> {
        if is_builtin(model) {
            return None;
        }
        self.catalogue.find("wavespeed", model)
    }

    /// Prediction submit shared by Rust-table and declared models.
    fn post(
        &self,
        endpoint: &str,
        body: &Value,
        api_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        let url = format!("{}/api/v3/{}", self.base, endpoint);
        let req = Request::post(&url)
            .secret_header("Authorization", format!("Bearer {api_key}"))
            .max_response_bytes(BODY_LIMIT)
            .json(body);
        let sent = req.and_then(|req| self.agent.send(req));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        // After a reply, a retryable error is a 408/429/5xx (HTTP or envelope code) or an unreadable body.
        let v = parse_envelope(&r, api_key).map_err(ProviderError::ambiguous_if_retryable)?;
        let id = v
            .get("data")
            .and_then(|d| d.get("id"))
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .filter(|i| {
                !i.is_empty()
                    && i.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
            .ok_or_else(|| {
                ProviderError::new(
                    "bad_response",
                    "wavespeed submit reply had no usable id",
                    true,
                )
                .ambiguous()
            })?;
        Ok(SubmitOutcome::Accepted {
            provider_job_id: id.to_string(),
        })
    }

    fn get(&self, url: &str, key: Option<&str>) -> Result<Resp, ProviderError> {
        let mut req = Request::get(url).max_response_bytes(BODY_LIMIT);
        if let Some(k) = key {
            req = req.secret_header("Authorization", format!("Bearer {k}"));
        }
        read(self.agent.send(req), key.unwrap_or(""))
    }
}

fn spec_for(req: &GenRequest) -> Result<&'static Spec, ProviderError> {
    let s = SPECS.iter().find(|s| s.id == req.model).ok_or_else(|| {
        ProviderError::new(
            "invalid_request",
            format!("unknown wavespeed model {}", req.model),
            false,
        )
    })?;
    let want = match s.family {
        Family::Image | Family::Imagen => Kind::Image,
        Family::Voice => Kind::Voice,
        Family::Avatar => Kind::Avatar,
        Family::Lipsync => Kind::Lipsync,
        Family::Transcribe => Kind::Transcribe,
        _ => Kind::Video,
    };
    if req.kind != want {
        return Err(ProviderError::new(
            "invalid_request",
            "model does not match request kind",
            false,
        ));
    }
    Ok(s)
}

fn str_param<'a>(req: &'a GenRequest, k: &str) -> Option<&'a str> {
    req.params
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn bad(msg: impl Into<String>) -> ProviderError {
    ProviderError::new("invalid_request", msg, false)
}

fn duration_secs(s: &Spec, req: &GenRequest) -> Result<u32, ProviderError> {
    let Some(v) = req.params.get("duration").filter(|v| !v.is_null()) else {
        return Ok(DEFAULT_SECS);
    };
    let n = v
        .as_u64()
        .or_else(|| {
            v.as_str()
                .and_then(|x| x.trim().trim_end_matches('s').parse().ok())
        })
        .and_then(|n| u32::try_from(n).ok());
    let range = format!("{} duration must be {}-{} seconds", s.id, s.min_s, s.max_s);
    match n {
        Some(d) if !s.allowed_s.is_empty() && !s.allowed_s.contains(&d) => Err(bad(format!(
            "{} only accepts durations {:?} seconds",
            s.id, s.allowed_s
        ))),
        Some(d) if d >= s.min_s && d <= s.max_s => Ok(d),
        _ => Err(bad(range)),
    }
}

fn resolution(s: &Spec, req: &GenRequest) -> Result<Option<&'static str>, ProviderError> {
    let given = str_param(req, "resolution");
    if let Some(fixed) = s.fixed_res {
        return match given {
            None => Ok(Some(fixed)),
            Some(g) if g == fixed => Ok(Some(fixed)),
            Some(g) => Err(bad(format!("{} is fixed at {fixed}, got {g}", s.id))),
        };
    }
    if s.tiers.is_empty() {
        return match given {
            None => Ok(None),
            Some(g) => Err(bad(format!("{} does not accept resolution {g}", s.id))),
        };
    }
    if s.family == Family::Avatar && given.is_none() {
        // Schema default is 480p; sent explicitly so the estimate matches billing.
        return Ok(Some("480p"));
    }
    let g = given.ok_or_else(|| {
        bad(format!(
            "{} requires params.resolution (no silent default)",
            s.id
        ))
    })?;
    s.tiers
        .iter()
        .find(|(r, _)| *r == g)
        .map(|(r, _)| Some(*r))
        .ok_or_else(|| bad(format!("unsupported {} resolution {g}", s.id)))
}

fn sound(req: &GenRequest) -> bool {
    req.params
        .get("sound")
        .or_else(|| req.params.get("generate_audio"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn https_param<'a>(req: &'a GenRequest, k: &str) -> Result<&'a str, ProviderError> {
    let v = str_param(req, k).ok_or_else(|| bad(format!("params.{k} is required")))?;
    if !v.starts_with("https://") {
        return Err(bad(format!("params.{k} must be an https URL")));
    }
    Ok(v)
}

/// Audio length the avatar estimate reserves. `audio_url` is an https URL the engine cannot measure
/// before submit, so the estimate is the provider's documented 600 s cap ([`AVATAR_MAX_SECS`]),
/// whatever the caller states. A stated `params.duration_secs` is never sent; it is validated
/// (0 to the cap) and can only raise the figure, which the cap already bounds.
fn avatar_secs(req: &GenRequest) -> Result<f64, ProviderError> {
    let stated = match req.params.get("duration_secs").filter(|v| !v.is_null()) {
        None => 0.0,
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0 && *n <= AVATAR_MAX_SECS)
            .ok_or_else(|| {
                bad(format!(
                    "params.duration_secs must be 0-{AVATAR_MAX_SECS} seconds"
                ))
            })?,
    };
    Ok(AVATAR_MAX_SECS.max(stated))
}

fn voice_id(s: &Spec, req: &GenRequest) -> &'static str {
    // Fixed fallbacks use example voices listed on the model pages.
    let _ = req;
    if s.id == "minimax-speech-02-hd" {
        "Wise_Woman"
    } else {
        "Alicia"
    }
}

fn voice_text(req: &GenRequest) -> Result<usize, ProviderError> {
    let n = req.prompt.chars().count();
    if req.prompt.trim().is_empty() {
        return Err(bad("voice models need non-empty text in the prompt"));
    }
    if n > VOICE_MAX_CHARS {
        return Err(bad(format!(
            "voice text is {n} characters; the limit is {VOICE_MAX_CHARS}"
        )));
    }
    Ok(n)
}

/// Validates the request and returns the endpoint slug.
fn resolve(req: &GenRequest) -> Result<(&'static Spec, &'static str), ProviderError> {
    let s = spec_for(req)?;
    if s.family == Family::Voice {
        voice_text(req)?;
        if s.id == "elevenlabs-turbo-v2.5" && req.params.get("speed").is_some_and(|v| !v.is_null())
        {
            return Err(bad("elevenlabs-turbo-v2.5 has no speed field"));
        }
        return Ok((s, s.t2v));
    }
    if s.family == Family::Avatar {
        https_param(req, "image_url")?;
        https_param(req, "audio_url")?;
        avatar_secs(req)?;
        resolution(s, req)?;
        return Ok((s, s.t2v));
    }
    if s.family == Family::Transcribe {
        // elevenlabs/scribe-v2 takes audio only; the prompt is not used.
        https_param(req, "audio_url")?;
        return Ok((s, s.t2v));
    }
    if s.family == Family::Lipsync {
        // sync/lipsync-3: an existing video re-timed to an audio track.
        https_param(req, "video_url")?;
        https_param(req, "audio_url")?;
        if let Some(m) = str_param(req, "sync_mode") {
            if !SYNC_MODES.contains(&m) {
                return Err(bad(format!(
                    "params.sync_mode must be bounce|loop|cut_off|silence|remap, got {m}"
                )));
            }
        }
        return Ok((s, s.t2v));
    }
    if s.family == Family::Image || s.family == Family::Imagen {
        if req
            .params
            .get("num_images")
            .and_then(Value::as_u64)
            .is_some_and(|n| n != 1)
        {
            return Err(bad("wavespeed image models produce one image per request"));
        }
        return Ok((s, s.t2v));
    }
    let image = str_param(req, "image_url");
    let end = str_param(req, "end_image_url");
    if s.family == Family::Happy && image.is_none() {
        return Err(bad(
            "happy-horse is image-to-video only: params.image_url is required",
        ));
    }
    if end.is_some() && image.is_none() {
        return Err(bad("params.end_image_url requires params.image_url"));
    }
    if end.is_some() && s.end_field.is_none() {
        return Err(bad(format!("{} does not support an end frame", s.id)));
    }
    duration_secs(s, req)?;
    resolution(s, req)?;
    Ok((s, if image.is_some() { s.i2v } else { s.t2v }))
}

/// Imagen on WaveSpeed accepts only these ratios; others snap to the nearest
/// (reference `wavespeed-image.js`: 2:3 becomes 3:4).
const IMAGEN_ASPECTS: [&str; 5] = ["1:1", "16:9", "9:16", "4:3", "3:4"];

fn imagen_aspect(p: &Value) -> &'static str {
    let want = p
        .get("aspect")
        .or_else(|| p.get("aspect_ratio"))
        .and_then(Value::as_str)
        .unwrap_or("1:1");
    let ratio = |s: &str| {
        let mut it = s.split(':').filter_map(|n| n.parse::<f64>().ok());
        match (it.next(), it.next()) {
            (Some(w), Some(h)) if h > 0.0 => w / h,
            _ => 1.0,
        }
    };
    let target = match want {
        "square" => 1.0,
        "portrait" => 2.0 / 3.0,
        "landscape" => 1.5,
        other => ratio(other),
    };
    IMAGEN_ASPECTS
        .iter()
        .min_by(|a, b| {
            (ratio(a) - target)
                .abs()
                .total_cmp(&(ratio(b) - target).abs())
        })
        .copied()
        .unwrap_or("1:1")
}

fn size_string(p: &Value) -> String {
    if let Some(s) = p.get("size").and_then(Value::as_str) {
        return s.to_string();
    }
    match p
        .get("aspect")
        .or_else(|| p.get("aspect_ratio"))
        .and_then(Value::as_str)
    {
        Some("16:9") => "1280*720",
        Some("9:16") => "720*1280",
        Some("2:3") => "832*1216",
        Some("3:2") => "1216*832",
        _ => "1024*1024",
    }
    .to_string()
}

fn build_body(s: &Spec, req: &GenRequest, endpoint: &str) -> Value {
    let p = &req.params;
    if s.family == Family::Transcribe {
        let mut b = json!({ "audio": str_param(req, "audio_url").unwrap_or("") });
        for k in ["language_code", "diarize", "tag_audio_events", "keyterms"] {
            if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
                b[k] = v.clone();
            }
        }
        return b;
    }
    if s.family == Family::Voice {
        let vid = str_param(req, "voice_id")
            .or_else(|| str_param(req, "voice"))
            .unwrap_or_else(|| voice_id(s, req));
        let mut b = json!({ "text": req.prompt, "voice_id": vid });
        let keys: &[&str] = if s.id == "minimax-speech-02-hd" {
            &[
                "speed",
                "volume",
                "pitch",
                "emotion",
                "english_normalization",
                "sample_rate",
                "bitrate",
                "channel",
                "format",
                "language_boost",
            ]
        } else {
            &["stability", "similarity", "use_speaker_boost"]
        };
        for k in keys {
            if let Some(v) = p.get(*k).filter(|v| !v.is_null()) {
                b[*k] = v.clone();
            }
        }
        return b;
    }
    if s.family == Family::Avatar {
        let mut b = json!({
            "image": str_param(req, "image_url").unwrap_or(""),
            "audio": str_param(req, "audio_url").unwrap_or(""),
        });
        if let Some(r) = resolution(s, req).ok().flatten() {
            b["resolution"] = json!(r);
        }
        if !req.prompt.trim().is_empty() {
            b["prompt"] = json!(req.prompt);
        }
        if let Some(seed) = p.get("seed").and_then(Value::as_i64) {
            b["seed"] = json!(seed);
        }
        if let Some(m) = str_param(req, "mask_image") {
            b["mask_image"] = json!(m);
        }
        return b;
    }
    if s.family == Family::Lipsync {
        let mut b = json!({
            "video": str_param(req, "video_url").unwrap_or(""),
            "audio": str_param(req, "audio_url").unwrap_or(""),
        });
        if let Some(m) = str_param(req, "sync_mode") {
            b["sync_mode"] = json!(m);
        }
        return b;
    }
    let mut body = json!({ "prompt": req.prompt });
    if s.family == Family::Imagen {
        body["aspect_ratio"] = json!(imagen_aspect(p));
        if let Some(neg) = str_param(req, "negative_prompt") {
            body["negative_prompt"] = json!(neg);
        }
        if let Some(seed) = p.get("seed").and_then(Value::as_i64) {
            body["seed"] = json!(seed);
        }
        return body;
    }
    if s.family == Family::Image {
        body["size"] = json!(size_string(p));
        if s.id != "recraft-v4" {
            body["output_format"] = json!(str_param(req, "output_format").unwrap_or("png"));
        }
        if s.id != "recraft-v4" {
            if let Some(seed) = p.get("seed").and_then(Value::as_i64) {
                body["seed"] = json!(seed);
            }
        }
        return body;
    }
    let image = str_param(req, "image_url");
    let i2v = endpoint == s.i2v;
    body["duration"] = json!(duration_secs(s, req).unwrap_or(DEFAULT_SECS));
    let aspect = p
        .get("aspect")
        .or_else(|| p.get("aspect_ratio"))
        .and_then(Value::as_str);
    match s.family {
        Family::Kling => {
            body["sound"] = json!(sound(req));
            // i2v takes its aspect from the image.
            if !i2v {
                body["aspect_ratio"] = json!(aspect.unwrap_or("16:9"));
            }
        }
        Family::Seedance => {
            body["generate_audio"] = json!(sound(req));
            if let Some(r) = resolution(s, req).ok().flatten() {
                body["resolution"] = json!(r);
            }
            if let Some(a) = aspect {
                body["aspect_ratio"] = json!(a);
            }
        }
        Family::Wan => {
            if !i2v {
                if let Some(a) = aspect {
                    body["aspect_ratio"] = json!(a);
                }
            }
        }
        Family::Happy => {
            if let Some(r) = resolution(s, req).ok().flatten() {
                body["resolution"] = json!(r);
            }
        }
        Family::Image
        | Family::Imagen
        | Family::Voice
        | Family::Avatar
        | Family::Lipsync
        | Family::Transcribe => {}
    }
    if let Some(neg) = str_param(req, "negative_prompt") {
        if s.family != Family::Seedance && s.family != Family::Happy {
            body["negative_prompt"] = json!(neg);
        }
    }
    if let Some(seed) = p.get("seed").and_then(Value::as_i64) {
        if s.family != Family::Kling {
            body["seed"] = json!(seed);
        }
    }
    if let Some(img) = image {
        body["image"] = json!(img);
        if let (Some(f), Some(end)) = (s.end_field, str_param(req, "end_image_url")) {
            body[f] = json!(end);
        }
    }
    body
}

/// `sync_mode` values `resolve` accepts for the lipsync model.
const SYNC_MODES: [&str; 5] = ["bounce", "loop", "cut_off", "silence", "remap"];
/// Aspects the video bodies forward (`build_body` passes `aspect` through for Kling text-to-video,
/// Seedance and Wan text-to-video without checking it). The adapter enforces no list; these two are the
/// ratios the app has always offered (an `open` list).
const VIDEO_ASPECTS: [&str; 2] = ["16:9", "9:16"];
/// Aspects `size_string` maps to a size for the Image family; any other value falls back to 1024*1024.
const IMAGE_ASPECTS: [&str; 5] = ["1:1", "16:9", "9:16", "3:2", "2:3"];

/// Facts for a model in the Rust table, from `resolve`, `resolution`, `duration_secs` and `build_body`.
fn builtin_capabilities(model: &str) -> Capabilities {
    let Some(s) = SPECS.iter().find(|s| s.id == model) else {
        return Capabilities::default();
    };
    let tiers: Vec<&str> = s.tiers.iter().map(|(r, _)| *r).collect();
    match s.family {
        Family::Kling | Family::Seedance | Family::Wan | Family::Happy => {
            let mut c = Capabilities::default()
                .setting("duration", serde_json::json!({ "param": "duration" }));
            // `resolve`: Happy Horse is image-to-video only; the others take an optional start frame, and a
            // last frame only where the spec has an end field (which also requires the start frame).
            let (start, mode) = if s.family == Family::Happy {
                (Need::Required, "image")
            } else {
                (Need::Optional, "prompt")
            };
            let end = if s.end_field.is_some() {
                Need::Optional
            } else {
                Need::None
            };
            c = c
                .mode(mode)
                .frames(start, "image_url", end, "end_image_url");
            // `build_body` sends no aspect for Happy Horse.
            if s.family != Family::Happy {
                c = c.setting(
                    "aspect",
                    capabilities::aspect("aspect", &VIDEO_ASPECTS, true),
                );
            }
            // `resolution`: a tiered model requires an explicit value (no default); a fixed one (Wan) takes none.
            if !tiers.is_empty() {
                c = c.setting(
                    "resolution",
                    capabilities::resolution("resolution", &tiers, true, None),
                );
            }
            // `build_body` sends no seed for Kling.
            if s.family != Family::Kling {
                c = c.setting("seed", capabilities::seed());
            }
            c
        }
        Family::Imagen => Capabilities::default()
            .mode("prompt")
            .setting(
                "aspect",
                capabilities::aspect("aspect", &IMAGEN_ASPECTS, false),
            )
            .setting("seed", capabilities::seed()),
        Family::Image => {
            let c = Capabilities::default().mode("prompt").setting(
                "aspect",
                capabilities::aspect("aspect", &IMAGE_ASPECTS, false),
            );
            // `build_body` sends no seed for recraft-v4.
            if s.id == "recraft-v4" {
                c
            } else {
                c.setting("seed", capabilities::seed())
            }
        }
        Family::Voice => Capabilities::default()
            .mode("script")
            .input("voice_id", serde_json::json!({ "param": "voice_id" })),
        Family::Avatar => Capabilities::default()
            .mode("image_and_audio")
            .input("image", capabilities::io("image_url", "https_url", true))
            .input("audio", capabilities::io("audio_url", "https_url", true))
            // `resolution`: 480p or 720p, 480p when absent.
            .setting(
                "resolution",
                capabilities::resolution("resolution", &tiers, false, Some("480p")),
            )
            .setting("seed", capabilities::seed()),
        Family::Lipsync => Capabilities::default()
            .mode("video_and_audio")
            .input("video", capabilities::io("video_url", "https_url", true))
            .input("audio", capabilities::io("audio_url", "https_url", true))
            .setting(
                "sync_mode",
                serde_json::json!({ "param": "sync_mode", "values": SYNC_MODES }),
            ),
        Family::Transcribe => Capabilities::default()
            .mode("audio")
            .input("audio", capabilities::io("audio_url", "https_url", true)),
    }
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    content_type: Option<String>,
}

const BODY_LIMIT: usize = 1 << 30;

fn redact(s: &str, key: &str) -> String {
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

fn read(r: Result<Response, HttpError>, key: &str) -> Result<Resp, ProviderError> {
    // Only an over-limit body reports as "read error"; every other send failure is "transport error".
    let resp = r.map_err(|e| match e {
        HttpError::BodyTooLarge { .. } => ProviderError::new(
            "transport",
            redact(&format!("wavespeed read error: {e}"), key),
            http::retryable(&e),
        ),
        _ => ProviderError::new(
            "transport",
            redact(&format!("wavespeed transport error: {e}"), key),
            http::retryable(&e),
        ),
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
            v.get("message")
                .or_else(|| v.get("error"))
                .or_else(|| v.get("detail"))
        })
        .map(|d| match d {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    let msg: String = msg.chars().take(300).collect();
    redact(&msg, key)
}

fn map_code(status: u16, text: String) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let balance = [
        "insufficient",
        "low balance",
        "out of balance",
        "out of credits",
        "payment required",
    ]
    .iter()
    .any(|n| lower.contains(n));
    match status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("wavespeed rejected the API key ({status}): {text}"),
            false,
        ),
        402 => ProviderError::new(
            "billing",
            format!("wavespeed billing problem: {text}"),
            false,
        ),
        s if (400..500).contains(&s) && s != 408 && s != 429 && balance => ProviderError::new(
            "billing",
            format!("wavespeed billing problem: {text}"),
            false,
        ),
        408 | 429 => {
            ProviderError::new("rate_limited", format!("wavespeed {status}: {text}"), true)
        }
        500..=599 => ProviderError::new("unavailable", format!("wavespeed {status}: {text}"), true),
        s => ProviderError::new("invalid_request", format!("wavespeed {s}: {text}"), false),
    }
}

/// Parse a reply: HTTP status and the `{code}` envelope must both be success.
fn parse_envelope(r: &Resp, key: &str) -> Result<Value, ProviderError> {
    if !(200..300).contains(&r.status) {
        return Err(map_code(r.status, error_text(&r.body, key)));
    }
    let v: Value = serde_json::from_slice(&r.body)
        .map_err(|_| ProviderError::new("bad_response", "wavespeed reply was not JSON", true))?;
    if let Some(code) = v.get("code").and_then(Value::as_u64) {
        if code != 200 {
            return Err(map_code(
                u16::try_from(code).unwrap_or(500),
                error_text(&r.body, key),
            ));
        }
    }
    Ok(v)
}

fn mime_for(u: &str) -> Option<String> {
    let path = u.split(['?', '#']).next().unwrap_or("");
    let m = match path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("ogg") => "audio/ogg",
        _ => return super::fal::mime_from_url(u),
    };
    Some(m.to_string())
}

fn outputs(d: &Value) -> Vec<Output> {
    // `outputs` is an array of URLs; tolerate an object like {"video": url}.
    let urls: Vec<&str> = match d.get("outputs") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        Some(Value::Object(o)) => o.values().filter_map(Value::as_str).collect(),
        Some(Value::String(s)) => vec![s.as_str()],
        _ => vec![],
    };
    urls.into_iter()
        .map(|u| Output::Url {
            url: u.to_string(),
            mime: mime_for(u),
        })
        .collect()
}

/// Scribe v2 list price for one request (see TRANSCRIBE_USD_PER_MIN). Without
/// `params.duration_seconds` the estimate assumes the 60-minute billing cap.
fn transcribe_usd(req: &GenRequest) -> f64 {
    let mins = req
        .params
        .get("duration_seconds")
        .and_then(Value::as_f64)
        .map_or(TRANSCRIBE_CAP_MIN, |s| (s / 60.0).min(TRANSCRIBE_CAP_MIN));
    let keyterms = req
        .params
        .get("keyterms")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    mins * if keyterms {
        TRANSCRIBE_KEYTERM_USD_PER_MIN
    } else {
        TRANSCRIBE_USD_PER_MIN
    }
}

/// The transcription outputs field holds the transcript object (text, words), not URLs;
/// a JSON string holding that object is accepted too.
fn transcript_outputs(d: &Value) -> Vec<Output> {
    let reply = match d.get("outputs") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    };
    reply
        .as_ref()
        .and_then(transcript_output)
        .into_iter()
        .collect()
}

impl Provider for WaveSpeed {
    fn id(&self) -> &'static str {
        "wavespeed"
    }

    fn models(&self) -> &'static [ModelInfo] {
        self.catalogue.rows("wavespeed", &MODELS)
    }

    /// Declared (catalogue) models only: the built-in table takes no reference list here.
    fn references(&self, model: &str) -> Option<ReferenceInput> {
        self.declared(model)?.references()
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        match self.declared(model) {
            Some(entry) => capabilities::from_entry(entry),
            None => builtin_capabilities(model),
        }
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        if let Some(entry) = self.declared(&req.model) {
            return entry.price(&entry.prepare(req)?.values);
        }
        let (s, _) = resolve(req)?;
        if s.family == Family::Transcribe {
            return Ok(transcribe_usd(req));
        }
        if s.family == Family::Image || s.family == Family::Imagen {
            return Ok(s.flat);
        }
        if s.family == Family::Voice {
            return Ok(voice_text(req)? as f64 * s.flat);
        }
        if s.family == Family::Avatar {
            let secs = avatar_secs(req)?.max(AVATAR_MIN_SECS);
            let rate = resolution(s, req)?
                .and_then(|r| s.tiers.iter().find(|(n, _)| *n == r).map(|(_, p)| *p))
                .unwrap_or(0.0);
            return Ok(secs * rate);
        }
        if s.family == Family::Lipsync {
            return Ok(s.flat);
        }
        let secs = duration_secs(s, req)? as f64;
        let rate = match resolution(s, req)? {
            Some(r) if !s.tiers.is_empty() => s
                .tiers
                .iter()
                .find(|(n, _)| *n == r)
                .map(|(_, p)| *p)
                .unwrap_or(0.0),
            _ => s.flat,
        };
        let mult = if sound(req) { s.sound_mult } else { 1.0 };
        Ok(secs * rate * mult)
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        if let Some(entry) = self.declared(&req.model) {
            return self.post(&entry.endpoint, &entry.prepare(req)?.body, api_key);
        }
        let (s, endpoint) = resolve(req)?;
        let body = build_body(s, req, endpoint);
        self.post(endpoint, &body, api_key)
    }

    fn poll(
        &self,
        req: &GenRequest,
        provider_job_id: &str,
        api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        if provider_job_id.is_empty()
            || !provider_job_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(bad("invalid wavespeed job id"));
        }
        let r = self.get(
            &format!(
                "{}/api/v3/predictions/{}/result",
                self.base, provider_job_id
            ),
            Some(api_key),
        )?;
        let v = parse_envelope(&r, api_key)?;
        let d = v.get("data").unwrap_or(&v);
        // Confirmed-failure vocabulary (closed list): "failed" and "error". Anything outside the
        // running / completed / failed words below, and a completed job with no readable media,
        // is `Unknown` and held with its reservation.
        match d.get("status").and_then(Value::as_str).unwrap_or("") {
            "created" | "processing" | "queued" | "pending" => Ok(PollOutcome::Running),
            "completed" | "succeeded" => {
                let o = if req.kind == Kind::Transcribe {
                    transcript_outputs(d)
                } else {
                    outputs(d)
                };
                if o.is_empty() {
                    Ok(unknown_outcome(
                        "completed, no usable media",
                        &r.body,
                        api_key,
                        true,
                    ))
                } else {
                    Ok(PollOutcome::Completed { outputs: o })
                }
            }
            "failed" | "error" => {
                let m = d
                    .get("error")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    .unwrap_or("unknown error");
                Ok(PollOutcome::Failed {
                    message: redact(&format!("wavespeed job failed: {m}"), api_key),
                })
            }
            "" => Err(ProviderError::new(
                "bad_response",
                "wavespeed result had no status",
                true,
            )),
            other => Ok(unknown_outcome(other, &r.body, api_key, false)),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /api/v3/balance is free and requires a valid key.
        let r = self.get(&format!("{}/api/v3/balance", self.base), Some(api_key))?;
        parse_envelope(&r, api_key).map(|_| ())
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
                // No credentials: result URLs are public CDN links.
                let r = self.get(url, None)?;
                if !(200..300).contains(&r.status) {
                    return Err(map_code(r.status, error_text(&r.body, "")));
                }
                let m = r
                    .content_type
                    .clone()
                    .filter(|c| !c.is_empty() && c != "application/octet-stream")
                    .or_else(|| mime.clone())
                    .or_else(|| super::fal::mime_from_url(url))
                    .unwrap_or_else(|| "application/octet-stream".into());
                Ok((r.body, m))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    #[allow(dead_code)]
    struct Seen {
        method: String,
        path: String,
        headers: String,
        body: String,
    }

    /// Minimal HTTP/1.1 mock. `handler(method, path, body) -> (status, content_type, body)`.
    fn mock<F>(handler: F) -> (String, Arc<Mutex<Vec<Seen>>>)
    where
        F: Fn(&str, &str, &str, &str) -> (u16, &'static str, Vec<u8>) + Send + 'static,
    {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let b2 = base.clone();
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut st) = stream else { continue };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
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
                let method = first.next().unwrap_or("").to_string();
                let path = first.next().unwrap_or("").to_string();
                s2.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: path.clone(),
                    headers: head.clone(),
                    body: body.clone(),
                });
                let (code, ct, out) = handler(&b2, &method, &path, &body);
                let h = format!("HTTP/1.1 {code} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = st.write_all(h.as_bytes());
                let _ = st.write_all(&out);
            }
        });
        (base, seen)
    }

    fn req(kind: Kind, model: &str, params: Value) -> GenRequest {
        GenRequest {
            kind,
            provider: "wavespeed".into(),
            model: model.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    const KEY: &str = "ws-secret-KEY-123";

    fn json_resp(v: Value) -> (u16, &'static str, Vec<u8>) {
        (200, "application/json", v.to_string().into_bytes())
    }

    fn accept(id: &str) -> (u16, &'static str, Vec<u8>) {
        json_resp(
            json!({"code": 200, "message": "success", "data": {"id": id, "status": "created"}}),
        )
    }

    fn body_of(seen: &Arc<Mutex<Vec<Seen>>>, i: usize) -> Value {
        serde_json::from_str(&seen.lock().unwrap()[i].body).unwrap()
    }

    #[test]
    fn video_happy_path_submit_poll_fetch() {
        let polls = Arc::new(Mutex::new(0));
        let p2 = polls.clone();
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/api/v3/kwaivgi/kling-v3.0-pro/text-to-video") => accept("job-1"),
            ("GET", "/api/v3/predictions/job-1/result") => {
                let mut n = p2.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    json_resp(json!({"code": 200, "data": {"status": "processing"}}))
                } else {
                    json_resp(
                        json!({"code": 200, "data": {"status": "completed", "outputs": [format!("{base}/cdn/c.mp4")]}}),
                    )
                }
            }
            ("GET", "/cdn/c.mp4") => (200, "application/octet-stream", vec![7; 3]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ws = WaveSpeed::new(&base);
        let r = req(
            Kind::Video,
            "kling-3-pro",
            json!({"duration": 5, "aspect": "9:16"}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = ws.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(provider_job_id, "job-1");
        assert_eq!(
            ws.poll(&r, &provider_job_id, KEY).unwrap(),
            PollOutcome::Running
        );
        let PollOutcome::Completed { outputs } = ws.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        let (bytes, mime) = ws.fetch(&outputs[0]).unwrap();
        assert_eq!((bytes, mime.as_str()), (vec![7; 3], "video/mp4"));
        let b = body_of(&seen, 0);
        assert_eq!(
            (
                b["duration"].as_u64(),
                b["aspect_ratio"].as_str(),
                b["sound"].as_bool()
            ),
            (Some(5), Some("9:16"), Some(false))
        );
        assert!(b.get("image").is_none());
        let s = seen.lock().unwrap();
        assert!(s[0]
            .headers
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
        assert!(s[1]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer"));
        let cdn = s.iter().find(|x| x.path == "/cdn/c.mp4").unwrap();
        assert!(!cdn.headers.contains(KEY));
    }

    #[test]
    fn first_last_frame_params() {
        let (base, seen) = mock(|_, _, _, _| accept("j"));
        let ws = WaveSpeed::new(&base);
        let p = json!({"image_url": "https://x/a.png", "end_image_url": "https://x/b.png", "duration": 4});
        ws.submit(&req(Kind::Video, "kling-3-pro", p.clone()), KEY)
            .unwrap();
        let mut sp = p.clone();
        sp["resolution"] = json!("720p");
        ws.submit(&req(Kind::Video, "seedance-2-fast", sp), KEY)
            .unwrap();
        ws.submit(
            &req(
                Kind::Video,
                "wan-hd",
                json!({"image_url": "https://x/a.png"}),
            ),
            KEY,
        )
        .unwrap();
        let s = seen.lock().unwrap();
        assert_eq!(s[0].path, "/api/v3/kwaivgi/kling-v3.0-pro/image-to-video");
        assert_eq!(
            s[1].path,
            "/api/v3/bytedance/seedance-2.0-fast/image-to-video"
        );
        assert_eq!(
            s[2].path,
            "/api/v3/wavespeed-ai/wan-2.2/i2v-720p-ultra-fast"
        );
        drop(s);
        let k = body_of(&seen, 0);
        assert_eq!(
            (k["image"].as_str(), k["end_image"].as_str()),
            (Some("https://x/a.png"), Some("https://x/b.png"))
        );
        assert!(k.get("aspect_ratio").is_none());
        let sd = body_of(&seen, 1);
        assert_eq!(
            (
                sd["image"].as_str(),
                sd["last_image"].as_str(),
                sd["resolution"].as_str(),
                sd["generate_audio"].as_bool()
            ),
            (
                Some("https://x/a.png"),
                Some("https://x/b.png"),
                Some("720p"),
                Some(false)
            )
        );
        let w = body_of(&seen, 2);
        assert_eq!(
            (w["image"].as_str(), w["duration"].as_u64()),
            (Some("https://x/a.png"), Some(5))
        );
        assert!(w.get("last_image").is_none());
    }

    #[test]
    fn strict_validation_never_reaches_network() {
        let (base, seen) = mock(|_, _, _, _| accept("j"));
        let ws = WaveSpeed::new(&base);
        let cases = [
            ("kling-3-pro", json!({"duration": 2})),
            ("kling-3-pro", json!({"duration": 16})),
            ("kling-3-pro", json!({"duration": "abc"})),
            ("seedance-2-fast", json!({"duration": 5})), // no resolution
            (
                "seedance-2-fast",
                json!({"duration": 3, "resolution": "720p"}),
            ),
            ("seedance-2-fast", json!({"resolution": "4k"})),
            ("wan-ultra", json!({"duration": 6})),
            ("wan-ultra", json!({"resolution": "720p"})),
            ("wan-ultra", json!({"image_url": "u", "end_image_url": "v"})),
            ("kling-3-pro", json!({"end_image_url": "v"})),
            ("nope", json!({})),
        ];
        for (m, p) in cases {
            let e = ws.submit(&req(Kind::Video, m, p.clone()), KEY).unwrap_err();
            assert_eq!(e.code, "invalid_request", "{m} {p}");
            assert_eq!(
                ws.estimate_usd(&req(Kind::Video, m, p)).unwrap_err().code,
                "invalid_request"
            );
        }
        assert_eq!(
            ws.submit(&req(Kind::Image, "kling-3-pro", json!({})), KEY)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            ws.submit(
                &req(Kind::Image, "qwen-image", json!({"num_images": 2})),
                KEY
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
        assert_eq!(seen.lock().unwrap().len(), 0);
    }

    #[test]
    fn image_models() {
        let (base, seen) = mock(|base, m, path, _| match (m, path) {
            ("POST", _) => accept("img1"),
            ("GET", "/api/v3/predictions/img1/result") => json_resp(
                json!({"code": 200, "data": {"status": "completed", "outputs": [format!("{base}/o/a.png")]}}),
            ),
            _ => (404, "text/plain", vec![]),
        });
        let ws = WaveSpeed::new(&base);
        ws.submit(
            &req(
                Kind::Image,
                "qwen-image",
                json!({"aspect": "16:9", "seed": 4}),
            ),
            KEY,
        )
        .unwrap();
        ws.submit(&req(Kind::Image, "recraft-v4", json!({})), KEY)
            .unwrap();
        let s = seen.lock().unwrap();
        assert_eq!(s[0].path, "/api/v3/wavespeed-ai/qwen-image/text-to-image");
        assert_eq!(s[1].path, "/api/v3/recraft-ai/recraft-v4/text-to-image");
        drop(s);
        let q = body_of(&seen, 0);
        assert_eq!(
            (
                q["size"].as_str(),
                q["output_format"].as_str(),
                q["seed"].as_i64()
            ),
            (Some("1280*720"), Some("png"), Some(4))
        );
        let rc = body_of(&seen, 1);
        assert!(rc.get("output_format").is_none() && rc["size"] == "1024*1024");
        let r = req(Kind::Image, "qwen-image", json!({}));
        let PollOutcome::Completed { outputs } = ws.poll(&r, "img1", KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            outputs,
            vec![Output::Url {
                url: format!("{base}/o/a.png"),
                mime: Some("image/png".into())
            }]
        );
    }

    #[test]
    fn tts_happy_path_body_checked() {
        let (base, seen) = mock(|base, m, path, _| match (m, path) {
            ("POST", "/api/v3/minimax/speech-02-hd") => accept("tts-1"),
            ("POST", "/api/v3/elevenlabs/turbo-v2.5") => accept("tts-2"),
            ("GET", "/api/v3/predictions/tts-1/result") => json_resp(
                json!({"code": 200, "data": {"status": "completed", "outputs": [format!("{base}/cdn/v.mp3")]}}),
            ),
            ("GET", "/cdn/v.mp3") => (200, "application/octet-stream", vec![9; 4]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ws = WaveSpeed::new(&base);
        let mut r = req(
            Kind::Voice,
            "minimax-speech-02-hd",
            json!({"voice": "Deep_Voice_Man", "speed": 1.1, "emotion": "happy"}),
        );
        r.prompt = "Hello there".into();
        let SubmitOutcome::Accepted { provider_job_id } = ws.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let PollOutcome::Completed { outputs } = ws.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            outputs,
            vec![Output::Url {
                url: format!("{base}/cdn/v.mp3"),
                mime: Some("audio/mpeg".into())
            }]
        );
        let (bytes, mime) = ws.fetch(&outputs[0]).unwrap();
        assert_eq!((bytes, mime.as_str()), (vec![9; 4], "audio/mpeg"));
        let b = body_of(&seen, 0);
        assert_eq!(b["text"], "Hello there");
        assert_eq!(b["voice_id"], "Deep_Voice_Man");
        assert_eq!(
            (b["speed"].as_f64(), b["emotion"].as_str()),
            (Some(1.1), Some("happy"))
        );
        assert!(b.get("prompt").is_none());
        // ElevenLabs: default voice, its own fields, no speed.
        let mut e = req(
            Kind::Voice,
            "elevenlabs-turbo-v2.5",
            json!({"stability": 0.5, "speed": 2}),
        );
        assert_eq!(ws.submit(&e, KEY).unwrap_err().code, "invalid_request");
        e.params = json!({"stability": 0.5, "similarity": 0.8});
        ws.submit(&e, KEY).unwrap();
        assert_eq!(
            seen.lock().unwrap()[3].path,
            "/api/v3/elevenlabs/turbo-v2.5"
        );
        let b = body_of(&seen, 3);
        assert_eq!(
            (
                b["voice_id"].as_str(),
                b["stability"].as_f64(),
                b["similarity"].as_f64()
            ),
            (Some("Alicia"), Some(0.5), Some(0.8))
        );
        // Empty text never reaches the network.
        e.prompt = "  ".into();
        assert_eq!(ws.submit(&e, KEY).unwrap_err().code, "invalid_request");
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    #[test]
    fn infinitetalk_happy_path() {
        let (base, seen) = mock(|base, m, path, _| match (m, path) {
            ("POST", "/api/v3/wavespeed-ai/infinitetalk") => accept("av-1"),
            ("GET", "/api/v3/predictions/av-1/result") => json_resp(
                json!({"code": 200, "data": {"status": "completed", "outputs": [format!("{base}/cdn/a.mp4")]}}),
            ),
            ("GET", "/cdn/a.mp4") => (200, "video/mp4", vec![1; 2]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ws = WaveSpeed::new(&base);
        let mut r = req(
            Kind::Avatar,
            "infinitetalk",
            json!({"image_url": "https://x/f.png", "audio_url": "https://x/a.mp3", "resolution": "720p"}),
        );
        r.prompt = String::new();
        let SubmitOutcome::Accepted { provider_job_id } = ws.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let PollOutcome::Completed { outputs } = ws.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        let (bytes, mime) = ws.fetch(&outputs[0]).unwrap();
        assert_eq!((bytes, mime.as_str()), (vec![1; 2], "video/mp4"));
        let b = body_of(&seen, 0);
        assert_eq!(
            (
                b["image"].as_str(),
                b["audio"].as_str(),
                b["resolution"].as_str()
            ),
            (
                Some("https://x/f.png"),
                Some("https://x/a.mp3"),
                Some("720p")
            )
        );
        assert!(b.get("prompt").is_none());
        // object-shaped outputs are tolerated
        assert_eq!(
            outputs_of(json!({"video": "https://c/v.mp4"})),
            vec![Output::Url {
                url: "https://c/v.mp4".into(),
                mime: Some("video/mp4".into())
            }]
        );
    }

    fn outputs_of(o: Value) -> Vec<Output> {
        outputs(&json!({ "outputs": o }))
    }

    #[test]
    fn infinitetalk_missing_inputs_never_reach_network() {
        let (base, seen) = mock(|_, _, _, _| accept("j"));
        let ws = WaveSpeed::new(&base);
        let cases = [
            json!({}),
            json!({"image_url": "https://x/f.png"}),
            json!({"audio_url": "https://x/a.mp3"}),
            json!({"image_url": "http://x/f.png", "audio_url": "https://x/a.mp3"}),
            json!({"image_url": "https://x/f.png", "audio_url": "file:///a.mp3"}),
            json!({"image_url": "https://x/f.png", "audio_url": "https://x/a.mp3", "resolution": "1080p"}),
            json!({"image_url": "https://x/f.png", "audio_url": "https://x/a.mp3", "duration_secs": 0}),
        ];
        for p in cases {
            let r = req(Kind::Avatar, "infinitetalk", p.clone());
            assert_eq!(
                ws.submit(&r, KEY).unwrap_err().code,
                "invalid_request",
                "{p}"
            );
            assert_eq!(
                ws.estimate_usd(&r).unwrap_err().code,
                "invalid_request",
                "{p}"
            );
        }
        assert_eq!(
            ws.submit(&req(Kind::Video, "infinitetalk", json!({})), KEY)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(seen.lock().unwrap().len(), 0);
    }

    #[test]
    fn scribe_v2_submits_audio_and_returns_transcript_json() {
        let (base, seen) = mock(|_, m, path, _| match (m, path) {
            ("POST", "/api/v3/elevenlabs/scribe-v2") => accept("tr-1"),
            ("GET", "/api/v3/predictions/tr-1/result") => json_resp(
                json!({"code": 200, "data": {"status": "completed", "outputs": {
                    "language_code": "eng",
                    "language_probability": 1,
                    "text": "Hi there.",
                    "words": [
                        {"text": "Hi", "start": 0.1, "end": 0.3, "type": "word", "speaker_id": null},
                        {"text": " ", "start": 0.3, "end": 0.3, "type": "spacing", "speaker_id": null},
                        {"text": "there.", "start": 0.3, "end": 0.8, "type": "word", "speaker_id": null}
                    ]
                }}}),
            ),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ws = WaveSpeed::new(&base);
        let r = req(
            Kind::Transcribe,
            "scribe-v2",
            json!({"audio_url": "https://x/a.mp3", "diarize": false, "duration_seconds": 30}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = ws.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(provider_job_id, "tr-1");
        let PollOutcome::Completed { outputs } = ws.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        let (bytes, mime) = ws.fetch(&outputs[0]).unwrap();
        assert_eq!(mime, "application/json");
        let t: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(t["text"], "Hi there.");
        assert_eq!(t["language_code"], "eng");
        assert_eq!(
            t["segments"],
            json!([{"start": 0.1, "end": 0.8, "text": "Hi there.", "speaker": null}])
        );
        let b = body_of(&seen, 0);
        assert_eq!(b["audio"], "https://x/a.mp3");
        assert_eq!(b["diarize"], false);
        assert!(b.get("prompt").is_none());
        // 30 s = 0.5 min at $0.01/min; keyterms make it $0.012/min.
        assert!((ws.estimate_usd(&r).unwrap() - 0.005).abs() < 1e-9);
        let kt = req(
            Kind::Transcribe,
            "scribe-v2",
            json!({"audio_url": "https://x/a.mp3", "keyterms": ["wavespeed"], "duration_seconds": 60}),
        );
        assert!((ws.estimate_usd(&kt).unwrap() - 0.012).abs() < 1e-9);
        let capped = req(
            Kind::Transcribe,
            "scribe-v2",
            json!({"audio_url": "https://x/a.mp3", "duration_seconds": 7200}),
        );
        assert!((ws.estimate_usd(&capped).unwrap() - 0.6).abs() < 1e-9);
        let bad = req(Kind::Transcribe, "scribe-v2", json!({}));
        assert_eq!(ws.submit(&bad, KEY).unwrap_err().code, "invalid_request");
        let http_audio = req(
            Kind::Transcribe,
            "scribe-v2",
            json!({"audio_url": "http://x/a.mp3"}),
        );
        assert_eq!(
            ws.submit(&http_audio, KEY).unwrap_err().code,
            "invalid_request"
        );
    }

    #[test]
    fn lipsync_happy_path_and_validation() {
        let (base, seen) = mock(|base, m, path, _| match (m, path) {
            ("POST", "/api/v3/sync/lipsync-3") => accept("ls-1"),
            ("GET", "/api/v3/predictions/ls-1/result") => json_resp(
                json!({"code": 200, "data": {"status": "completed", "outputs": [format!("{base}/cdn/synced.mp4")]}}),
            ),
            ("GET", "/cdn/synced.mp4") => (200, "video/mp4", vec![3; 2]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ws = WaveSpeed::new(&base);
        let r = req(
            Kind::Lipsync,
            "lipsync-3",
            json!({"video_url": "https://x/v.mp4", "audio_url": "https://x/a.mp3", "sync_mode": "cut_off"}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = ws.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(provider_job_id, "ls-1");
        let PollOutcome::Completed { outputs } = ws.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        let (bytes, mime) = ws.fetch(&outputs[0]).unwrap();
        assert_eq!((bytes, mime.as_str()), (vec![3; 2], "video/mp4"));
        let b = body_of(&seen, 0);
        assert_eq!(
            (
                b["video"].as_str(),
                b["audio"].as_str(),
                b["sync_mode"].as_str()
            ),
            (
                Some("https://x/v.mp4"),
                Some("https://x/a.mp3"),
                Some("cut_off")
            )
        );
        assert!((ws.estimate_usd(&r).unwrap() - 0.05).abs() < 1e-9);

        let (base, seen) = mock(|_, _, _, _| accept("j"));
        let ws = WaveSpeed::new(&base);
        for p in [
            json!({}),
            json!({"video_url": "https://x/v.mp4"}),
            json!({"audio_url": "https://x/a.mp3"}),
            json!({"video_url": "http://x/v.mp4", "audio_url": "https://x/a.mp3"}),
            json!({"video_url": "https://x/v.mp4", "audio_url": "https://x/a.mp3", "sync_mode": "nope"}),
        ] {
            assert_eq!(
                ws.submit(&req(Kind::Lipsync, "lipsync-3", p.clone()), KEY)
                    .unwrap_err()
                    .code,
                "invalid_request",
                "{p}"
            );
        }
        // a lipsync model under a different kind is rejected
        assert_eq!(
            ws.submit(
                &req(
                    Kind::Video,
                    "lipsync-3",
                    json!({"video_url": "https://x/v.mp4", "audio_url": "https://x/a.mp3"})
                ),
                KEY
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
        assert_eq!(seen.lock().unwrap().len(), 0);
        let _ = base;
    }

    #[test]
    fn voice_and_avatar_estimates() {
        let ws = WaveSpeed::new("http://x");
        let close = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        let mut v = req(Kind::Voice, "elevenlabs-turbo-v2.5", json!({}));
        v.prompt = "x".repeat(1000);
        close(ws.estimate_usd(&v).unwrap(), 0.10);
        v.model = "minimax-speech-02-hd".into();
        v.prompt = "x".repeat(250);
        close(ws.estimate_usd(&v).unwrap(), 0.025);
        v.prompt = "x".repeat(10_001);
        assert_eq!(ws.estimate_usd(&v).unwrap_err().code, "invalid_request");
        let av = |p: Value| {
            ws.estimate_usd(&req(Kind::Avatar, "infinitetalk", p))
                .unwrap()
        };
        let base = json!({"image_url": "https://x/f.png", "audio_url": "https://x/a.mp3"});
        let with = |k: &str, v: Value| {
            let mut b = base.clone();
            b[k] = v;
            b
        };
        // The remote audio cannot be measured, so the provider's 600 s cap is reserved at the tier's rate
        // (it was 10 s: 0.30 at 480p, 0.60 at 720p), and a stated duration_secs cannot lower it.
        close(av(base.clone()), 600.0 * 0.03);
        close(av(with("resolution", json!("720p"))), 600.0 * 0.06);
        close(av(with("duration_secs", json!(20))), 600.0 * 0.03); // was 0.60
        close(av(with("duration_secs", json!(1))), 600.0 * 0.03); // was 0.09 (3 s minimum)
        close(av(with("duration_secs", json!(600))), 600.0 * 0.03);
        assert!(ws
            .models()
            .iter()
            .any(|m| m.model == "infinitetalk" && m.kind == Kind::Avatar));
        // Two built in plus the declared Inworld Realtime TTS 2 entry.
        assert_eq!(
            ws.models().iter().filter(|m| m.kind == Kind::Voice).count(),
            3
        );
    }

    #[test]
    fn failure_states() {
        let (base, _) = mock(|_, _, path, _| match path {
            "/api/v3/predictions/f1/result" => json_resp(
                json!({"code": 200, "data": {"status": "failed", "error": "content policy"}}),
            ),
            "/api/v3/predictions/f2/result" => {
                json_resp(json!({"code": 200, "data": {"status": "completed", "outputs": []}}))
            }
            _ => (404, "text/plain", vec![]),
        });
        let ws = WaveSpeed::new(&base);
        let r = req(Kind::Video, "kling-3-pro", json!({}));
        // old: a completed job with no media was `Failed` (released, resubmitted); new: `Unknown`
        // with completed=true, held with the reservation settled.
        match ws.poll(&r, "f1", KEY).unwrap() {
            PollOutcome::Failed { message } => {
                assert!(message.contains("content policy"), "{message}")
            }
            o => panic!("{o:?}"),
        }
        match ws.poll(&r, "f2", KEY).unwrap() {
            PollOutcome::Unknown {
                status, completed, ..
            } => assert_eq!(
                (status.as_str(), completed),
                ("completed, no usable media", true)
            ),
            o => panic!("{o:?}"),
        }
        assert_eq!(
            ws.poll(&r, "../x", KEY).unwrap_err().code,
            "invalid_request"
        );
    }

    #[test]
    fn error_mapping_and_no_key_leak() {
        let leak = format!(r#"{{"code":0,"message":"bad {KEY}"}}"#);
        for (status, code, retry) in [
            (401, "auth", false),
            (403, "auth", false),
            (402, "billing", false),
            (429, "rate_limited", true),
            (503, "unavailable", true),
            (400, "invalid_request", false),
        ] {
            let l = leak.clone();
            let (base, _) =
                mock(move |_, _, _, _| (status, "application/json", l.clone().into_bytes()));
            let e = WaveSpeed::new(&base)
                .submit(&req(Kind::Image, "qwen-image", json!({})), KEY)
                .unwrap_err();
            assert_eq!(
                (e.code.as_str(), e.retryable),
                (code, retry),
                "status {status}"
            );
            assert!(!e.message.contains(KEY), "{}", e.message);
        }
        // balance text on a 400 is billing
        let (base, _) = mock(|_, _, _, _| {
            (
                400,
                "application/json",
                br#"{"message":"Insufficient balance"}"#.to_vec(),
            )
        });
        assert_eq!(
            WaveSpeed::new(&base)
                .submit(&req(Kind::Image, "qwen-image", json!({})), KEY)
                .unwrap_err()
                .code,
            "billing"
        );
        // HTTP 200 with a failing envelope code
        let (base, _) =
            mock(|_, _, _, _| json_resp(json!({"code": 401, "message": "invalid api key"})));
        assert_eq!(
            WaveSpeed::new(&base)
                .submit(&req(Kind::Image, "qwen-image", json!({})), KEY)
                .unwrap_err()
                .code,
            "auth"
        );
        let e = WaveSpeed::new("http://127.0.0.1:1")
            .submit(&req(Kind::Image, "qwen-image", json!({})), KEY)
            .unwrap_err();
        assert!(e.retryable && e.code == "transport" && !e.message.contains(KEY));
    }

    #[test]
    fn key_check_and_estimates() {
        let (base, seen) = mock(|_, _, _, _| {
            json_resp(json!({"code": 200, "message": "success", "data": {"balance": 12.5}}))
        });
        WaveSpeed::new(&base).test_key(KEY).unwrap();
        assert_eq!(seen.lock().unwrap()[0].path, "/api/v3/balance");
        let (base, _) = mock(|_, _, _, _| (401, "application/json", b"{}".to_vec()));
        assert_eq!(
            WaveSpeed::new(&base).test_key(KEY).unwrap_err().code,
            "auth"
        );

        let ws = WaveSpeed::new("http://x");
        let est = |m: &str, k: Kind, p: Value| ws.estimate_usd(&req(k, m, p)).unwrap();
        let close = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        close(
            est("kling-3-pro", Kind::Video, json!({"duration": 5})),
            0.56,
        );
        close(
            est(
                "kling-3-pro",
                Kind::Video,
                json!({"duration": 10, "sound": true}),
            ),
            1.68,
        );
        close(
            est(
                "seedance-2-fast",
                Kind::Video,
                json!({"duration": 4, "resolution": "480p"}),
            ),
            0.40,
        );
        close(
            est(
                "seedance-2-fast",
                Kind::Video,
                json!({"duration": 8, "resolution": "720p"}),
            ),
            1.44,
        );
        close(
            est(
                "seedance-2-fast",
                Kind::Video,
                json!({"duration": 5, "resolution": "1080p"}),
            ),
            2.50,
        );
        close(est("wan-ultra", Kind::Video, json!({"duration": 8})), 0.08);
        close(est("wan-hd", Kind::Video, json!({})), 0.10);
        close(est("qwen-image", Kind::Image, json!({})), 0.02);
        close(est("seedream-v4", Kind::Image, json!({})), 0.027);
        // 15 built-in specs plus the 20 WaveSpeed entries of the embedded data catalogue.
        assert_eq!(ws.models().len(), 35);
        assert!(SPECS
            .iter()
            .all(|s| ws.models().iter().any(|m| m.model == s.id)));
    }

    #[test]
    fn happy_horse_i2v_params_price_and_validation() {
        let (base, seen) = mock(|_, _, _, _| accept("hh"));
        let ws = WaveSpeed::new(&base);
        let p = json!({"image_url": "https://x/a.png", "duration": 6, "resolution": "1080p", "seed": 7, "aspect": "9:16", "negative_prompt": "blur"});
        let r = req(Kind::Video, "happy-horse", p);
        assert!((ws.estimate_usd(&r).unwrap() - 6.0 * 0.28).abs() < 1e-9);
        ws.submit(&r, KEY).unwrap();
        let s = seen.lock().unwrap();
        assert_eq!(s[0].path, "/api/v3/alibaba/happyhorse-1.0/image-to-video");
        drop(s);
        let b = body_of(&seen, 0);
        assert_eq!(
            (
                b["image"].as_str(),
                b["duration"].as_u64(),
                b["resolution"].as_str(),
                b["seed"].as_i64()
            ),
            (Some("https://x/a.png"), Some(6), Some("1080p"), Some(7))
        );
        assert!(b.get("aspect_ratio").is_none() && b.get("negative_prompt").is_none());
        let r720 = req(
            Kind::Video,
            "happy-horse",
            json!({"image_url": "https://x/a.png", "resolution": "720p"}),
        );
        assert!((ws.estimate_usd(&r720).unwrap() - 5.0 * 0.14).abs() < 1e-9);
        for p in [
            json!({"resolution": "720p"}),
            json!({"image_url": "https://x/a.png"}),
            json!({"image_url": "https://x/a.png", "resolution": "480p"}),
            json!({"image_url": "https://x/a.png", "resolution": "720p", "duration": 2}),
            json!({"image_url": "https://x/a.png", "resolution": "720p", "duration": 16}),
            json!({"image_url": "https://x/a.png", "end_image_url": "https://x/b.png", "resolution": "720p"}),
        ] {
            assert!(
                ws.estimate_usd(&req(Kind::Video, "happy-horse", p.clone()))
                    .is_err(),
                "{p}"
            );
        }
        assert_eq!(seen.lock().unwrap().len(), 1);
        let m = ws
            .models()
            .iter()
            .find(|m| m.model == "happy-horse")
            .unwrap();
        assert_eq!(
            (m.durations.min_duration_s, m.durations.max_duration_s),
            (Some(3), Some(15))
        );
    }

    #[test]
    fn imagen_standard_and_ultra() {
        let (base, seen) = mock(|_, _, _, _| accept("im"));
        let ws = WaveSpeed::new(&base);
        let p = json!({"aspect": "2:3", "negative_prompt": "text", "seed": 3});
        let std = req(Kind::Image, "imagen-4-standard", p.clone());
        let ultra = req(Kind::Image, "imagen-4-ultra", json!({"aspect": "16:9"}));
        assert_eq!(
            (
                ws.estimate_usd(&std).unwrap(),
                ws.estimate_usd(&ultra).unwrap()
            ),
            (0.04, 0.05)
        );
        ws.submit(&std, KEY).unwrap();
        ws.submit(&ultra, KEY).unwrap();
        let s = seen.lock().unwrap();
        assert_eq!(s[0].path, "/api/v3/google/imagen4");
        assert_eq!(s[1].path, "/api/v3/google/imagen4-ultra");
        drop(s);
        let a = body_of(&seen, 0);
        assert_eq!(
            (
                a["aspect_ratio"].as_str(),
                a["negative_prompt"].as_str(),
                a["seed"].as_i64()
            ),
            (Some("3:4"), Some("text"), Some(3))
        );
        assert!(a.get("size").is_none() && a.get("output_format").is_none());
        assert_eq!(body_of(&seen, 1)["aspect_ratio"], "16:9");
        assert!(ws
            .estimate_usd(&req(Kind::Video, "imagen-4-ultra", json!({})))
            .is_err());
        assert!(ws
            .estimate_usd(&req(
                Kind::Image,
                "imagen-4-ultra",
                json!({"num_images": 2})
            ))
            .is_err());
    }

    // ---- declared models (catalogue entries) ----

    use crate::catalogue::testing;

    const DECLARED: &str = "vendor/test-declared";

    fn declared_req(params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::MotionControl,
            provider: "wavespeed".into(),
            model: DECLARED.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    fn declared_params() -> Value {
        json!({"video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"]})
    }

    fn declared_ws(base: &str, models: Vec<Value>) -> WaveSpeed {
        WaveSpeed::new(base).with_catalogue(testing::catalogue(models))
    }

    #[test]
    fn declared_model_lists_its_reference_images_and_built_ins_declare_none() {
        let a = declared_ws(
            "http://unused.invalid",
            vec![testing::sample("wavespeed", DECLARED)],
        );
        let r = a
            .references(DECLARED)
            .expect("sample declares a reference list");
        assert_eq!(
            serde_json::json!(r),
            serde_json::json!({"param": "image_urls", "max": 2, "local_upload": "none"})
        );
        assert!(a.references("not/declared").is_none());
        let builtin = MODELS.first().expect("built-in model").model;
        assert!(a.references(builtin).is_none());
    }

    #[test]
    fn declared_model_posts_exact_body_to_its_slug_and_extracts_outputs() {
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/api/v3/vendor/test-declared/v1") => accept("dj-1"),
            ("GET", "/api/v3/predictions/dj-1/result") => json_resp(
                json!({"code": 200, "data": {"status": "completed", "outputs": [format!("{base}/cdn/o.mp4")]}}),
            ),
            ("GET", "/cdn/o.mp4") => (200, "video/mp4", vec![4, 5]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ws = declared_ws(&base, vec![testing::sample("wavespeed", DECLARED)]);
        let r = declared_req(json!({
            "video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"], "duration": 8, "resolution": "1080p"
        }));
        assert!((ws.estimate_usd(&r).unwrap() - 1.6).abs() < 1e-12);
        let SubmitOutcome::Accepted { provider_job_id } = ws.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(provider_job_id, "dj-1");
        let PollOutcome::Completed { outputs } = ws.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            ws.fetch(&outputs[0]).unwrap(),
            (vec![4, 5], "video/mp4".to_string())
        );
        let s = seen.lock().unwrap();
        assert!(s[0]
            .headers
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
        drop(s);
        assert_eq!(
            body_of(&seen, 0),
            json!({
                "mode": "std", "prompt": "a red fox",
                "video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"],
                "duration": 8, "resolution": "1080p", "keep_original_sound": true
            })
        );
    }

    #[test]
    fn declared_model_rejects_bad_requests_before_any_http_call() {
        let (base, seen) = mock(|_, _, _, _| (500, "text/plain", b"must not be called".to_vec()));
        let ws = declared_ws(&base, vec![testing::sample("wavespeed", DECLARED)]);
        let with = |extra: Value| {
            let mut p = declared_params();
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            declared_req(p)
        };
        let cases = [
            (with(json!({"durtion": 6})), "durtion"),
            (
                declared_req(json!({"image_urls": ["https://x/a.png"]})),
                "params.video_url is required",
            ),
            (with(json!({"resolution": "4k"})), "must be one of"),
            (with(json!({"duration": 2})), "must be at least 3"),
            (with(json!({"duration": 16})), "must be at most 15"),
            (
                with(json!({"video_url": "http://x/v.mp4"})),
                "must be an https URL",
            ),
            (
                with(
                    json!({"image_urls": ["https://x/a.png", "https://x/b.png", "https://x/c.png"]}),
                ),
                "the limit is 2",
            ),
            (
                GenRequest {
                    kind: Kind::Image,
                    ..with(json!({}))
                },
                "does not match request kind",
            ),
        ];
        for (r, needle) in cases {
            for e in [
                ws.submit(&r, KEY).unwrap_err(),
                ws.estimate_usd(&r).unwrap_err(),
            ] {
                assert_eq!(e.code, "invalid_request", "{needle}");
                assert!(
                    e.message.contains(needle),
                    "{:?} lacks {needle:?}",
                    e.message
                );
            }
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "rejected requests never reached the network"
        );
    }

    #[test]
    fn declared_model_estimates_each_pricing_kind() {
        let priced = |name: &str, pricing: Value| {
            let mut v = testing::sample("wavespeed", name);
            v["inputs"].as_array_mut().unwrap().push(json!({"param": "num_images", "json_key": "num_images", "type": "integer", "default": 1, "min": 1, "max": 4}));
            v["pricing"] = pricing;
            v
        };
        let ws = declared_ws(
            "http://127.0.0.1:1",
            vec![
                priced("m/flat", json!({"kind": "flat", "usd": 0.63})),
                priced(
                    "m/second",
                    json!({"kind": "per_second", "usd": 0.125, "seconds_param": "duration"}),
                ),
                priced(
                    "m/tier",
                    json!({"kind": "per_second", "usd": 9.0, "seconds_param": "duration",
                "by": {"param": "resolution", "values": {"720p": 0.1, "1080p": 0.2}}}),
                ),
                priced(
                    "m/item",
                    json!({"kind": "per_item", "usd": 0.04, "count_param": "num_images"}),
                ),
                priced(
                    "m/mp",
                    json!({"kind": "per_megapixel", "usd": 0.006, "megapixels": 1.05}),
                ),
            ],
        );
        let est = |model: &str, extra: Value| {
            let mut p = declared_params();
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            ws.estimate_usd(&GenRequest {
                model: model.into(),
                ..declared_req(p)
            })
            .unwrap()
        };
        assert_eq!(est("m/flat", json!({})), 0.63);
        assert_eq!(est("m/second", json!({"duration": 8})), 1.0);
        assert_eq!(
            est("m/tier", json!({"duration": 8, "resolution": "1080p"})),
            1.6
        );
        assert_eq!(est("m/item", json!({"num_images": 3})), 0.04 * 3.0);
        assert_eq!(est("m/mp", json!({})), 0.006 * 1.05);
    }

    #[test]
    fn declared_models_follow_the_rust_rows_and_leave_them_untouched() {
        let (base, seen) = mock(|_, _, _, _| accept("j"));
        let mut shadow = testing::sample("wavespeed", "wan-ultra");
        shadow["kind"] = json!("video");
        let ws = declared_ws(
            &base,
            vec![
                testing::sample("wavespeed", DECLARED),
                testing::sample("fal", "fal-ai/not-ws"),
                shadow,
            ],
        );
        let rows = ws.models();
        assert_eq!(rows.len(), MODELS.len() + 2);
        for (a, b) in rows.iter().zip(MODELS.iter()) {
            assert_eq!((a.model, a.kind, a.est_usd), (b.model, b.kind, b.est_usd));
        }
        assert_eq!(rows[MODELS.len()].model, DECLARED);
        // Rust-table behaviour is unchanged, including for an id the catalogue also declares.
        ws.submit(&req(Kind::Video, "wan-ultra", json!({})), KEY)
            .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].path,
            "/api/v3/wavespeed-ai/wan-2.2/t2v-480p-ultra-fast"
        );
        assert_eq!(
            body_of(&seen, 0),
            json!({"prompt": "a red fox", "duration": 5})
        );
        assert!(
            (ws.estimate_usd(&req(Kind::Video, "wan-ultra", json!({})))
                .unwrap()
                - 0.05)
                .abs()
                < 1e-12
        );
        assert!(
            (ws.estimate_usd(&req(Kind::Image, "qwen-image", json!({})))
                .unwrap()
                - 0.02)
                .abs()
                < 1e-12
        );
        assert_eq!(
            ws.estimate_usd(&req(Kind::Video, "nope", json!({})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn declared_model_with_a_stale_price_is_refused_with_price_stale() {
        let (base, seen) = mock(|_, _, _, _| (500, "text/plain", b"must not be called".to_vec()));
        let mut old = testing::sample("wavespeed", DECLARED);
        old["priced_at"] = json!(testing::days_ago(61));
        let ws = declared_ws(&base, vec![old]);
        let r = declared_req(declared_params());
        for e in [
            ws.estimate_usd(&r).unwrap_err(),
            ws.submit(&r, KEY).unwrap_err(),
        ] {
            assert_eq!(e.code, "price_stale");
            assert!(e.message.contains(DECLARED), "{}", e.message);
        }
        assert!(seen.lock().unwrap().is_empty());
    }
}
