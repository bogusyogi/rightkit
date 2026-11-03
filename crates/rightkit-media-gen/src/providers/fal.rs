//! fal.ai queue adapter (image + video).
//!
//! Sources (checked 2026-10-05):
//! - Queue API: <https://fal.ai/docs/model-apis/model-endpoints/queue>
//!   (`POST {queue}/{endpoint}`, status/response URLs come back in the submit
//!   reply; auth is `Authorization: Key <key>`).
//! - Image model `fal-ai/flux-2/klein/9b`: <https://fal.ai/models/fal-ai/flux-2/klein/9b>
//!   ($0.006 per output megapixel).
//! - Video models `fal-ai/veo3.1/fast` (text-to-video) and
//!   `fal-ai/veo3.1/fast/image-to-video`: <https://fal.ai/models/fal-ai/veo3.1/fast>
//!   ($0.10/s audio off, $0.15/s audio on at 720p/1080p; 4K, a `resolution`
//!   value `"4k"` on the same endpoint, is $0.30/s off and $0.35/s on, from the
//!   model page and OpenAPI schema read 2026-10-12). Input: `prompt`, `duration` string enum "4s"|"6s"|"8s",
//!   `aspect_ratio`, `resolution`, `generate_audio` (fal defaults it to true,
//!   so it is always sent), `image_url` (i2v); output `video.url`.
//!
//! Workspace rule `docs/rules/video-pipeline.md` "Backend split (HARD)": fal
//! carries Veo 3.1 only (Kling etc. live on WaveSpeed). Audio is off by
//! default, and durations outside {4, 6, 8} s are rejected, never snapped.
//!
//! `provider_job_id` is `"{request_id}|{status_url}|{response_url}"`. fal's
//! status/response URLs use the app root (e.g. `fal-ai/veo3.1`), not the
//! full endpoint path, so the URLs returned by submit must be reused.
use super::{
    request_may_have_reached, status_may_have_run, transcript_output, unknown_outcome, Durations,
    GenRequest, Kind, ModelInfo, Output, PollOutcome, Provider, ProviderError, ReferenceInput,
    SubmitOutcome,
};
use crate::capabilities::{self, Capabilities, Need};
use crate::catalogue::{Catalogue, Entry};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const QUEUE_BASE: &str = "https://queue.fal.run";
/// Platform REST API (model discovery); used only for the key check.
pub const REST_BASE: &str = "https://api.fal.ai/v1";

const IMAGE_MODEL: &str = "fal-ai/flux-2/klein/9b";
const VIDEO_T2V: &str = "fal-ai/veo3.1/fast";
const VIDEO_I2V: &str = "fal-ai/veo3.1/fast/image-to-video";

const STABLE_AUDIO: &str = "fal-ai/stable-audio";
const STABLE_AUDIO_25: &str = "fal-ai/stable-audio-25/text-to-audio";
const MMAUDIO: &str = "fal-ai/mmaudio/v2";
const CREATIVE_UPSCALER: &str = "fal-ai/creative-upscaler";
const CLARITY_UPSCALER: &str = "fal-ai/clarity-upscaler";
const TOPAZ_VIDEO_UPSCALE: &str = "fal-ai/topaz/upscale/video";
const LATENTSYNC: &str = "fal-ai/latentsync";
const SYNC_LIPSYNC: &str = "fal-ai/sync-lipsync";
/// OpenAI-compatible chat completion gateway (`fal-ai/any-llm`): the request
/// names the backing LLM in `params.llm` (e.g. `openai/gpt-4o-mini`).
const ANY_LLM: &str = "fal-ai/any-llm";
/// Backing model used when the request does not name one.
const ANY_LLM_DEFAULT: &str = "openai/gpt-4o-mini";
/// fal list price for `fal-ai/any-llm`: 0.001 USD per request. Source:
/// https://fal.ai/models/fal-ai/any-llm, read 2026-10-11. The page marks the endpoint deprecated by fal.
const ANY_LLM_USD_PER_REQUEST: f64 = 0.001;
/// ElevenLabs Scribe v2 on fal (`fal-ai/elevenlabs/speech-to-text/scribe-v2`): input `audio_url`,
/// optional `keyterms`. List price $0.008 per input audio minute, +30% when keyterms are sent.
/// Source: https://fal.ai/models/fal-ai/elevenlabs/speech-to-text/scribe-v2, read 2026-10-17.
const SCRIBE_V2: &str = "fal-ai/elevenlabs/speech-to-text/scribe-v2";
const SCRIBE_USD_PER_MIN: f64 = 0.008;
const SCRIBE_KEYTERM_MULT: f64 = 1.3;

/// USD per output megapixel; requests are sized at ~1 MP (square_hd = 1.05 MP).
const IMAGE_USD_PER_MP: f64 = 0.006;
const IMAGE_MP: f64 = 1.05;
const VIDEO_USD_PER_S: f64 = 0.10;
const VIDEO_USD_PER_S_AUDIO: f64 = 0.15;
/// `veo3.1/fast` and `veo3.1/fast/image-to-video` at `resolution: "4k"` (fal pages, read 2026-10-12).
const VIDEO_USD_PER_S_4K: f64 = 0.30;
const VIDEO_USD_PER_S_4K_AUDIO: f64 = 0.35;
/// Veo accepts only these durations.
const VEO_SECS: [u32; 3] = [4, 6, 8];
const DEFAULT_VIDEO_SECS: u32 = 4;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Single start frame (`image_url`).
    I2v,
    /// First and last frame (`image_url` + `end_image_url`).
    Flf,
    /// Reference images / ingredients (`image_urls`, 1-3).
    Reference,
}

/// A Veo 3.1 variant addressed by its own fal endpoint (the model id).
/// Rates are `(no_audio, with_audio)` USD/s at 720p and 1080p, and at `4k` where the endpoint offers it.
struct Variant {
    model: &'static str,
    mode: Mode,
    secs: &'static [u32],
    default_secs: u32,
    r720: (f64, f64),
    r1080: (f64, f64),
    /// `None`: the endpoint's schema has no `4k` resolution (Lite), so `4k` is refused.
    r4k: Option<(f64, f64)>,
}

/// Prices from the fal model pages (checked 2026-10-06) and the baseline
/// `tools/backends/falai.js` (standard no-audio $0.20/s invoice-verified
/// 2026-05-07). Reference-to-video is 8 s only (fal 422 "Input should be
/// '8s'", baseline 2026-05-04) and takes 16:9 or 9:16 and no seed or
/// negative prompt. Lite FLF takes 4/6/8 s on the live page (the baseline
/// recorded 8 s only) at $0.03/$0.05 per s (720p) and $0.05/$0.08 (1080p).
/// 4K (`resolution: "4k"`, checked 2026-10-12 on each endpoint's page and OpenAPI schema) is $0.40/$0.60
/// per s on the standard endpoints and $0.30/$0.35 on Fast; the Lite page and schema have no 4K.
static VARIANTS: [Variant; 5] = [
    Variant {
        model: "fal-ai/veo3.1/image-to-video",
        mode: Mode::I2v,
        secs: &VEO_SECS,
        default_secs: 4,
        r720: (0.20, 0.40),
        r1080: (0.20, 0.40),
        r4k: Some((0.40, 0.60)),
    },
    Variant {
        model: "fal-ai/veo3.1/first-last-frame-to-video",
        mode: Mode::Flf,
        secs: &VEO_SECS,
        default_secs: 4,
        r720: (0.20, 0.40),
        r1080: (0.20, 0.40),
        r4k: Some((0.40, 0.60)),
    },
    Variant {
        model: "fal-ai/veo3.1/reference-to-video",
        mode: Mode::Reference,
        secs: &[8],
        default_secs: 8,
        r720: (0.20, 0.40),
        r1080: (0.20, 0.40),
        r4k: Some((0.40, 0.60)),
    },
    Variant {
        model: "fal-ai/veo3.1/fast/first-last-frame-to-video",
        mode: Mode::Flf,
        secs: &VEO_SECS,
        default_secs: 4,
        r720: (0.10, 0.15),
        r1080: (0.10, 0.15),
        r4k: Some((0.30, 0.35)),
    },
    Variant {
        model: "fal-ai/veo3.1/lite/first-last-frame-to-video",
        mode: Mode::Flf,
        secs: &VEO_SECS,
        default_secs: 4,
        r720: (0.03, 0.05),
        r1080: (0.05, 0.08),
        r4k: None,
    },
];

fn variant(model: &str) -> Option<&'static Variant> {
    VARIANTS.iter().find(|v| v.model == model)
}

static MODELS: [ModelInfo; 18] = [
    ModelInfo {
        provider: "fal",
        model: IMAGE_MODEL,
        kind: Kind::Image,
        label: "FLUX.2 klein 9B (image, ~$0.006/MP)",
        est_usd: 0.0065,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: VIDEO_T2V,
        kind: Kind::Video,
        label: "Veo 3.1 Fast text-to-video (4 s, no audio, $0.10/s)",
        est_usd: 0.40,
        durations: Durations::set(&VEO_SECS, DEFAULT_VIDEO_SECS),
    },
    ModelInfo {
        provider: "fal",
        model: VIDEO_I2V,
        kind: Kind::Video,
        label: "Veo 3.1 Fast image-to-video (4 s, no audio, $0.10/s)",
        est_usd: 0.40,
        durations: Durations::set(&VEO_SECS, DEFAULT_VIDEO_SECS),
    },
    ModelInfo {
        provider: "fal",
        model: "fal-ai/veo3.1/image-to-video",
        kind: Kind::Video,
        label: "Veo 3.1 image-to-video (4/6/8 s, no audio, $0.20/s)",
        est_usd: 0.80,
        durations: Durations::set(&VEO_SECS, DEFAULT_VIDEO_SECS),
    },
    ModelInfo {
        provider: "fal",
        model: "fal-ai/veo3.1/first-last-frame-to-video",
        kind: Kind::Video,
        label: "Veo 3.1 first/last frame (4/6/8 s, no audio, $0.20/s)",
        est_usd: 0.80,
        durations: Durations::set(&VEO_SECS, DEFAULT_VIDEO_SECS),
    },
    ModelInfo {
        provider: "fal",
        model: "fal-ai/veo3.1/reference-to-video",
        kind: Kind::Video,
        label: "Veo 3.1 reference ingredients (1-3 images, 8 s only, no audio, $0.20/s)",
        est_usd: 1.60,
        durations: Durations::set(&[8], 8),
    },
    ModelInfo {
        provider: "fal",
        model: "fal-ai/veo3.1/fast/first-last-frame-to-video",
        kind: Kind::Video,
        label: "Veo 3.1 Fast first/last frame (4/6/8 s, no audio, $0.10/s)",
        est_usd: 0.40,
        durations: Durations::set(&VEO_SECS, DEFAULT_VIDEO_SECS),
    },
    ModelInfo {
        provider: "fal",
        model: "fal-ai/veo3.1/lite/first-last-frame-to-video",
        kind: Kind::Video,
        label: "Veo 3.1 Lite first/last frame (4/6/8 s, no audio, $0.03/s 720p)",
        est_usd: 0.12,
        durations: Durations::set(&VEO_SECS, DEFAULT_VIDEO_SECS),
    },
    ModelInfo {
        provider: "fal",
        model: STABLE_AUDIO,
        kind: Kind::Music,
        label: "Stable Audio text-to-music",
        est_usd: 0.05,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: STABLE_AUDIO_25,
        kind: Kind::Music,
        label: "Stable Audio 2.5 text-to-music",
        est_usd: 0.05,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: MMAUDIO,
        kind: Kind::Sfx,
        label: "MMAudio video-to-audio (synchronized sound from params.video_url)",
        est_usd: 0.05,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: CREATIVE_UPSCALER,
        kind: Kind::Upscale,
        label: "Creative Upscaler (image, needs params.image_url)",
        est_usd: 0.03,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: CLARITY_UPSCALER,
        kind: Kind::Upscale,
        label: "Clarity Upscaler (image, needs params.image_url)",
        est_usd: 0.03,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: TOPAZ_VIDEO_UPSCALE,
        kind: Kind::Upscale,
        label: "Topaz video upscale (needs params.video_url)",
        est_usd: 0.50,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: LATENTSYNC,
        kind: Kind::Lipsync,
        label: "LatentSync lipsync (needs params.video_url + params.audio_url)",
        est_usd: 0.35,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: SYNC_LIPSYNC,
        kind: Kind::Lipsync,
        label: "sync.so lipsync (needs params.video_url + params.audio_url)",
        est_usd: 0.50,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: ANY_LLM,
        kind: Kind::Text,
        label: "any-llm chat completion, deprecated by fal (params.llm picks the backing model)",
        est_usd: ANY_LLM_USD_PER_REQUEST,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "fal",
        model: SCRIBE_V2,
        kind: Kind::Transcribe,
        label: "ElevenLabs Scribe v2 transcription (needs params.audio_url, $0.008/audio min, +30% with keyterms)",
        est_usd: 0.48,
        durations: Durations::NONE,
    },
];

/// True for a model defined in the Rust table above (catalogue entries cannot redefine it).
pub(crate) fn is_builtin(model: &str) -> bool {
    MODELS.iter().any(|m| m.model == model)
}

pub struct Fal {
    queue_base: String,
    rest_base: String,
    agent: Http,
    catalogue: &'static Catalogue,
}

impl Fal {
    pub fn new(queue_base: &str, rest_base: &str) -> Self {
        let agent = Http::new(Duration::from_secs(10), Duration::from_secs(300));
        Self {
            queue_base: queue_base.trim_end_matches('/').to_string(),
            rest_base: rest_base.trim_end_matches('/').to_string(),
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
        self.catalogue.find("fal", model)
    }

    fn resolve(
        &self,
        req: &GenRequest,
    ) -> Result<(&'static str, &'static ModelInfo), ProviderError> {
        let info = MODELS
            .iter()
            .find(|m| m.model == req.model)
            .ok_or_else(|| {
                ProviderError::new(
                    "invalid_request",
                    format!("unknown fal model {}", req.model),
                    false,
                )
            })?;
        if info.kind != req.kind {
            return Err(ProviderError::new(
                "invalid_request",
                "model does not match request kind",
                false,
            ));
        }
        if let Some(v) = variant(info.model) {
            check_variant(v, req)?;
            return Ok((info.model, info));
        }
        let mut endpoint = info.model;
        if info.kind == Kind::Video && image_url(req).is_some() {
            endpoint = VIDEO_I2V;
        }
        if info.kind == Kind::Video && image_url(req).is_none() && endpoint == VIDEO_I2V {
            return Err(ProviderError::new(
                "invalid_request",
                "image-to-video requires params.image_url",
                false,
            ));
        }
        if info.kind == Kind::Video {
            duration_secs(req)?;
            // Both non-variant rows (Fast text-to-video and Fast image-to-video) offer 4k.
            resolution(req, true)?;
        }
        let need = |k: &str| {
            str_param(req, k).ok_or_else(|| {
                ProviderError::new(
                    "invalid_request",
                    format!("{} requires params.{k}", info.model),
                    false,
                )
            })
        };
        match info.kind {
            Kind::Music => {
                music_secs(req)?;
            }
            Kind::Sfx => {
                need("video_url")?;
            }
            Kind::Upscale => {
                if info.model == TOPAZ_VIDEO_UPSCALE {
                    need("video_url")?;
                } else {
                    need("image_url")?;
                }
            }
            Kind::Lipsync => {
                need("video_url")?;
                need("audio_url")?;
            }
            Kind::Text => {
                if req.prompt.trim().is_empty() {
                    return Err(ProviderError::new(
                        "invalid_request",
                        "text prompt is empty",
                        false,
                    ));
                }
            }
            Kind::Transcribe => {
                need("audio_url")?;
            }
            _ => {}
        }
        Ok((endpoint, info))
    }

    fn build_body(&self, req: &GenRequest, endpoint: &str) -> Value {
        let p = &req.params;
        if let Some(v) = variant(endpoint) {
            return variant_body(v, req);
        }
        let mut body = json!({ "prompt": req.prompt });
        match req.kind {
            Kind::Image => {
                body["num_images"] = json!(num_images(req));
                body["image_size"] = json!(image_size(p));
                body["output_format"] = json!(p
                    .get("output_format")
                    .and_then(Value::as_str)
                    .unwrap_or("png"));
            }
            Kind::Video => {
                // resolve() already validated these.
                body["duration"] = json!(format!(
                    "{}s",
                    duration_secs(req).unwrap_or(DEFAULT_VIDEO_SECS)
                ));
                body["resolution"] = json!(resolution(req, true).unwrap_or("720p"));
                body["generate_audio"] = json!(audio(req));
                let ar = p
                    .get("aspect")
                    .or_else(|| p.get("aspect_ratio"))
                    .and_then(Value::as_str);
                if endpoint == VIDEO_I2V {
                    body["image_url"] = json!(image_url(req).unwrap_or_default());
                    body["aspect_ratio"] = json!(ar.unwrap_or("auto"));
                } else {
                    body["aspect_ratio"] = json!(ar.unwrap_or("16:9"));
                }
            }
            Kind::Music => {
                // Stable Audio: prompt + optional length in seconds.
                if let Some(s) = music_secs(req).unwrap_or_default() {
                    body["seconds_total"] = json!(s);
                }
            }
            Kind::Sfx => {
                body["video_url"] = json!(str_param(req, "video_url").unwrap_or_default());
            }
            Kind::Upscale => {
                if endpoint == TOPAZ_VIDEO_UPSCALE {
                    body["video_url"] = json!(str_param(req, "video_url").unwrap_or_default());
                    if let Some(f) = req
                        .params
                        .get("scale")
                        .and_then(Value::as_f64)
                        .or_else(|| req.params.get("upscale_factor").and_then(Value::as_f64))
                    {
                        body["upscale_factor"] = json!(f);
                    }
                } else {
                    body["image_url"] = json!(str_param(req, "image_url").unwrap_or_default());
                    if let Some(f) = req.params.get("scale").and_then(Value::as_f64) {
                        body["scale"] = json!(f);
                    }
                }
            }
            Kind::Lipsync => {
                body["video_url"] = json!(str_param(req, "video_url").unwrap_or_default());
                body["audio_url"] = json!(str_param(req, "audio_url").unwrap_or_default());
                if let Some(s) = str_param(req, "sync_mode") {
                    body["sync_mode"] = json!(s);
                }
            }
            Kind::Text => {
                // fal-ai/any-llm: OpenAI-compatible chat shape — `model` is the
                // backing LLM id, `prompt` the user message; system_prompt,
                // temperature, max_tokens and reasoning_effort pass through.
                body["model"] = json!(str_param(req, "llm").unwrap_or(ANY_LLM_DEFAULT));
                for k in [
                    "system_prompt",
                    "temperature",
                    "max_tokens",
                    "reasoning_effort",
                ] {
                    if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
                        body[k] = v.clone();
                    }
                }
            }
            Kind::Transcribe => {
                // Scribe takes audio only: the prompt is not sent.
                if let Some(o) = body.as_object_mut() {
                    o.remove("prompt");
                }
                body["audio_url"] = json!(str_param(req, "audio_url").unwrap_or_default());
                if let Some(k) = p.get("keyterms").filter(|k| k.is_array()) {
                    body["keyterms"] = k.clone();
                }
            }
            // No Rust-table model has these kinds; declared models take another path.
            Kind::Voice
            | Kind::Avatar
            | Kind::Extend
            | Kind::MotionControl
            | Kind::ReplaceObject
            | Kind::Restyle => {}
        }
        if let Some(seed) = p.get("seed").and_then(Value::as_i64) {
            if req.kind == Kind::Image {
                body["seed"] = json!(seed);
            }
        }
        body
    }

    fn get(&self, url: &str, key: Option<&str>) -> Result<Resp, ProviderError> {
        let mut req = Request::get(url).max_response_bytes(BODY_LIMIT);
        if let Some(k) = key {
            req = req.secret_header("Authorization", format!("Key {k}"));
        }
        read(self.agent.send(req), key.unwrap_or(""))
    }

    /// Queue submit shared by Rust-table and declared models.
    fn post(
        &self,
        endpoint: &str,
        body: &Value,
        api_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        let url = format!("{}/{}", self.queue_base, endpoint);
        let req = Request::post(&url)
            .secret_header("Authorization", format!("Key {api_key}"))
            .max_response_bytes(BODY_LIMIT)
            .json(body);
        let sent = req.and_then(|req| self.agent.send(req));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(&r, api_key).ambiguous_if(status_may_have_run(r.status)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "fal submit reply was not JSON", true).ambiguous()
        })?;
        let rid = v.get("request_id").and_then(Value::as_str).ok_or_else(|| {
            ProviderError::new("bad_response", "fal submit reply had no request_id", true)
                .ambiguous()
        })?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
        Ok(SubmitOutcome::Accepted {
            provider_job_id: format!("{rid}|{}|{}", s("status_url"), s("response_url")),
        })
    }
}

fn str_param<'a>(req: &'a GenRequest, k: &str) -> Option<&'a str> {
    req.params
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn ref_urls(req: &GenRequest) -> Vec<&str> {
    req.params
        .get("image_urls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn end_url(req: &GenRequest) -> Option<&str> {
    str_param(req, "end_image_url").or_else(|| str_param(req, "last_frame_url"))
}

fn variant_secs(v: &Variant, req: &GenRequest) -> Result<u32, ProviderError> {
    let bad = || {
        ProviderError::new(
            "invalid_request",
            format!("{} duration must be one of {:?} seconds", v.model, v.secs),
            false,
        )
    };
    let Some(d) = req.params.get("duration").filter(|d| !d.is_null()) else {
        return Ok(v.default_secs);
    };
    let n = d
        .as_u64()
        .or_else(|| {
            d.as_str()
                .and_then(|s| s.trim().trim_end_matches('s').parse().ok())
        })
        .ok_or_else(bad)?;
    u32::try_from(n)
        .ok()
        .filter(|n| v.secs.contains(n))
        .ok_or_else(bad)
}

fn check_variant(v: &Variant, req: &GenRequest) -> Result<(), ProviderError> {
    let bad = |m: String| Err(ProviderError::new("invalid_request", m, false));
    if req.kind != Kind::Video {
        return bad("model does not match request kind".into());
    }
    variant_secs(v, req)?;
    resolution(req, v.r4k.is_some())?;
    let ar = req
        .params
        .get("aspect")
        .or_else(|| req.params.get("aspect_ratio"))
        .and_then(Value::as_str);
    match v.mode {
        Mode::I2v => {
            if image_url(req).is_none() {
                return bad(format!("{} requires params.image_url", v.model));
            }
        }
        Mode::Flf => {
            if image_url(req).is_none() || end_url(req).is_none() {
                return bad(format!(
                    "{} requires params.image_url and params.end_image_url",
                    v.model
                ));
            }
        }
        Mode::Reference => {
            let n = ref_urls(req).len();
            if !(1..=3).contains(&n) {
                return bad(format!(
                    "{} requires params.image_urls with 1-3 images, got {n}",
                    v.model
                ));
            }
            if matches!(ar, Some(a) if a != "16:9" && a != "9:16") {
                return bad(format!("{} aspect must be 16:9 or 9:16", v.model));
            }
            for k in ["seed", "negative_prompt"] {
                if req.params.get(k).is_some_and(|x| !x.is_null()) {
                    return bad(format!("{} does not accept {k}", v.model));
                }
            }
        }
    }
    Ok(())
}

fn variant_body(v: &Variant, req: &GenRequest) -> Value {
    let p = &req.params;
    let ar = p
        .get("aspect")
        .or_else(|| p.get("aspect_ratio"))
        .and_then(Value::as_str);
    let mut body = json!({
        "prompt": req.prompt,
        "duration": format!("{}s", variant_secs(v, req).unwrap_or(v.default_secs)),
        "resolution": resolution(req, v.r4k.is_some()).unwrap_or("720p"),
        "generate_audio": audio(req),
    });
    match v.mode {
        Mode::I2v => {
            body["image_url"] = json!(image_url(req).unwrap_or_default());
            body["aspect_ratio"] = json!(ar.unwrap_or("auto"));
        }
        Mode::Flf => {
            body["first_frame_url"] = json!(image_url(req).unwrap_or_default());
            body["last_frame_url"] = json!(end_url(req).unwrap_or_default());
            body["aspect_ratio"] = json!(ar.unwrap_or("auto"));
        }
        Mode::Reference => {
            body["image_urls"] = json!(ref_urls(req));
            body["aspect_ratio"] = json!(ar.unwrap_or("16:9"));
        }
    }
    if v.mode != Mode::Reference {
        if let Some(seed) = p.get("seed").and_then(Value::as_i64) {
            body["seed"] = json!(seed);
        }
        if let Some(n) = str_param(req, "negative_prompt") {
            body["negative_prompt"] = json!(n);
        }
    }
    body
}

fn image_url(req: &GenRequest) -> Option<&str> {
    req.params
        .get("image_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn num_images(req: &GenRequest) -> u64 {
    req.params
        .get("num_images")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 4)
}

/// Strict: only 4, 6 or 8 seconds (number, "6" or "6s"); no snapping.
fn duration_secs(req: &GenRequest) -> Result<u32, ProviderError> {
    let bad = || {
        ProviderError::new(
            "invalid_request",
            "Veo duration must be 4, 6 or 8 seconds",
            false,
        )
    };
    let Some(v) = req.params.get("duration").filter(|v| !v.is_null()) else {
        return Ok(DEFAULT_VIDEO_SECS);
    };
    let n = v
        .as_u64()
        .or_else(|| {
            v.as_str()
                .and_then(|s| s.trim().trim_end_matches('s').parse().ok())
        })
        .ok_or_else(bad)?;
    u32::try_from(n)
        .ok()
        .filter(|d| VEO_SECS.contains(d))
        .ok_or_else(bad)
}

/// `4k` is accepted only where the row has a 4K rate (`allow_4k`); the Lite row refuses it.
fn resolution(req: &GenRequest, allow_4k: bool) -> Result<&'static str, ProviderError> {
    match req.params.get("resolution").and_then(Value::as_str) {
        None | Some("720p") => Ok("720p"),
        Some("1080p") => Ok("1080p"),
        Some("4k") if allow_4k => Ok("4k"),
        Some(o) => {
            let offered = if allow_4k {
                "720p, 1080p or 4k"
            } else {
                "720p or 1080p"
            };
            Err(ProviderError::new(
                "invalid_request",
                format!("unsupported Veo resolution {o} ({offered})"),
                false,
            ))
        }
    }
}

fn audio(req: &GenRequest) -> bool {
    req.params
        .get("generate_audio")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Stable Audio length in seconds: `params.duration` or
/// `params.seconds_total` (number, or "12s"). Absent is fine — fal uses its
/// own default.
fn music_secs(req: &GenRequest) -> Result<Option<u64>, ProviderError> {
    let v = req
        .params
        .get("seconds_total")
        .or_else(|| req.params.get("duration"));
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let n = v
        .as_u64()
        .or_else(|| {
            v.as_str()
                .and_then(|s| s.trim().trim_end_matches('s').parse().ok())
        })
        .ok_or_else(|| {
            ProviderError::new(
                "invalid_request",
                "music duration must be a number of seconds",
                false,
            )
        })?;
    if !(1..=600).contains(&n) {
        return Err(ProviderError::new(
            "invalid_request",
            "music duration must be 1-600 seconds",
            false,
        ));
    }
    Ok(Some(n))
}

fn image_size(p: &Value) -> String {
    if let Some(s) = p.get("image_size").and_then(Value::as_str) {
        return s.to_string();
    }
    match p
        .get("aspect")
        .or_else(|| p.get("aspect_ratio"))
        .and_then(Value::as_str)
    {
        Some("4:3") => "landscape_4_3",
        Some("16:9") => "landscape_16_9",
        Some("3:4") => "portrait_4_3",
        Some("9:16") => "portrait_16_9",
        Some("1:1") => "square_hd",
        _ => "landscape_4_3",
    }
    .to_string()
}

/// Aspects the Veo bodies honour: `variant_body` and `build_body` send 16:9 or 9:16 and fall back to
/// `auto` / `16:9` for anything else.
const VEO_ASPECTS: [&str; 2] = ["16:9", "9:16"];
/// Aspects `image_size` maps to a fal `image_size`; any other value falls back to `landscape_4_3`.
const IMAGE_ASPECTS: [&str; 5] = ["1:1", "16:9", "9:16", "4:3", "3:4"];

/// Facts for a model in the Rust table, from the checks above (`resolve`, `check_variant`, `build_body`).
fn builtin_capabilities(model: &str) -> Capabilities {
    let Some(info) = MODELS.iter().find(|m| m.model == model) else {
        return Capabilities::default();
    };
    match info.kind {
        Kind::Image => Capabilities::default()
            .mode("prompt")
            .setting(
                "aspect",
                capabilities::aspect("aspect", &IMAGE_ASPECTS, false),
            )
            .setting("seed", capabilities::seed()),
        Kind::Video => {
            // `resolution()`: 720p or 1080p, plus 4k on rows with a 4K rate; 720p when absent.
            let allow_4k = variant(model).is_none_or(|v| v.r4k.is_some());
            let resolutions: &[&str] = if allow_4k {
                &["720p", "1080p", "4k"]
            } else {
                &["720p", "1080p"]
            };
            let c = Capabilities::default()
                .setting("duration", serde_json::json!({ "param": "duration" }))
                .setting(
                    "aspect",
                    capabilities::aspect("aspect", &VEO_ASPECTS, false),
                )
                .setting(
                    "resolution",
                    capabilities::resolution("resolution", resolutions, false, Some("720p")),
                );
            match variant(model) {
                Some(v) => match v.mode {
                    Mode::I2v => c
                        .mode("image")
                        .frames(Need::Required, "image_url", Need::None, "")
                        .setting("seed", capabilities::seed()),
                    // `end_url`: `end_image_url`, or `last_frame_url`.
                    Mode::Flf => c
                        .mode("image")
                        .frames(Need::Required, "image_url", Need::Required, "end_image_url")
                        .setting("seed", capabilities::seed()),
                    // `check_variant`: `image_urls` with 1-3 entries, no seed.
                    Mode::Reference => c
                        .mode("prompt")
                        .frames(Need::None, "", Need::None, "")
                        .references(ReferenceInput {
                            param: "image_urls",
                            max: Some(3),
                            local_upload: "none",
                        }),
                },
                // `resolve`: the image-to-video endpoint requires `image_url`; the text endpoint switches to it when one is given.
                None if model == VIDEO_I2V => {
                    c.mode("image")
                        .frames(Need::Required, "image_url", Need::None, "")
                }
                None => c
                    .mode("prompt")
                    .frames(Need::Optional, "image_url", Need::None, ""),
            }
        }
        // `music_secs`: whole seconds 1-600 in `seconds_total` (`duration` is read as well).
        Kind::Music => Capabilities::default().mode("prompt").setting(
            "length",
            capabilities::length("seconds_total", "s", 1.0, 600.0, true),
        ),
        // `resolve`: `video_url` is required.
        Kind::Sfx => Capabilities::default()
            .mode("video")
            .input("video", capabilities::io("video_url", "url", true)),
        Kind::Upscale if model == TOPAZ_VIDEO_UPSCALE => Capabilities::default()
            .mode("video")
            .input("video", capabilities::io("video_url", "url", true)),
        Kind::Upscale => Capabilities::default()
            .mode("image")
            .input("image", capabilities::io("image_url", "url", true)),
        // `resolve`: `video_url` and `audio_url` are required; `sync_mode` is forwarded unchecked.
        Kind::Lipsync => Capabilities::default()
            .mode("video_and_audio")
            .input("video", capabilities::io("video_url", "url", true))
            .input("audio", capabilities::io("audio_url", "url", true)),
        Kind::Transcribe => Capabilities::default()
            .mode("audio")
            .input("audio", capabilities::io("audio_url", "url", true)),
        _ => Capabilities::default().mode("prompt"),
    }
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    content_type: Option<String>,
}

const BODY_LIMIT: usize = 1 << 30;

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

fn read(r: Result<Response, HttpError>, key: &str) -> Result<Resp, ProviderError> {
    // Only an over-limit body reports as "read error"; every other send failure is "transport error".
    let resp = r.map_err(|e| match e {
        HttpError::BodyTooLarge { .. } => ProviderError::new(
            "transport",
            redact(&format!("fal read error: {e}"), key),
            http::retryable(&e),
        ),
        _ => ProviderError::new(
            "transport",
            redact(&format!("fal transport error: {e}"), key),
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
            v.get("detail")
                .or_else(|| v.get("error"))
                .or_else(|| v.get("message"))
        })
        .map(|d| match d {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    let msg: String = msg.chars().take(300).collect();
    redact(&msg, key)
}

fn map_status(r: &Resp, key: &str) -> ProviderError {
    let text = error_text(&r.body, key);
    match r.status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("fal rejected the API key ({}): {text}", r.status),
            false,
        ),
        402 => ProviderError::new("billing", format!("fal billing problem: {text}"), false),
        408 | 429 => ProviderError::new("rate_limited", format!("fal {}: {text}", r.status), true),
        500..=599 => ProviderError::new("unavailable", format!("fal {}: {text}", r.status), true),
        s => ProviderError::new("invalid_request", format!("fal {s}: {text}"), false),
    }
}

pub(crate) fn mime_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "flac" => "audio/flac",
            "m4a" => "audio/mp4",
            _ => return None,
        }
        .to_string(),
    )
}

fn file_output(f: &Value) -> Option<Output> {
    let url = f.get("url")?.as_str()?.to_string();
    let mime = f
        .get("content_type")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| mime_from_url(&url));
    Some(Output::Url { url, mime })
}

/// Scribe v2 list price for one request: per input audio minute, +30% with keyterms. Without
/// `params.duration_seconds` the estimate assumes 60 minutes, an upper bound for one request.
fn scribe_usd(req: &GenRequest) -> f64 {
    let mins = req
        .params
        .get("duration_seconds")
        .and_then(Value::as_f64)
        .map_or(60.0, |s| s / 60.0);
    let keyterms = req
        .params
        .get("keyterms")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    mins * SCRIBE_USD_PER_MIN * if keyterms { SCRIBE_KEYTERM_MULT } else { 1.0 }
}

fn parse_outputs(v: &Value, kind: Kind) -> Vec<Output> {
    if kind == Kind::Transcribe {
        // Scribe replies with `text` and a word list; the shared shape adds timed segments.
        return transcript_output(v).into_iter().collect();
    }
    if kind == Kind::Text {
        // any-llm returns an OpenAI-style completion; accept the documented
        // `output` string and tolerant shapes so mocks stay honest.
        let text = v
            .get("output")
            .and_then(Value::as_str)
            .or_else(|| {
                v.pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
            })
            .or_else(|| v.get("text").and_then(Value::as_str));
        return text
            .filter(|t| !t.is_empty())
            .map(|t| {
                vec![Output::Bytes {
                    bytes: t.as_bytes().to_vec(),
                    mime: "text/plain".into(),
                }]
            })
            .unwrap_or_default();
    }
    let mut out = Vec::new();
    if let Some(arr) = v.get("images").and_then(Value::as_array) {
        out.extend(arr.iter().filter_map(file_output));
    }
    for k in ["image", "video", "audio_file", "audio", "video_file"] {
        if let Some(f) = v.get(k) {
            out.extend(file_output(f));
        }
    }
    out
}

impl Provider for Fal {
    fn id(&self) -> &'static str {
        "fal"
    }

    fn models(&self) -> &'static [ModelInfo] {
        self.catalogue.rows("fal", &MODELS)
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
        let (_, info) = self.resolve(req)?;
        if info.kind == Kind::Transcribe {
            return Ok(scribe_usd(req));
        }
        if let Some(v) = variant(info.model) {
            let (off, on) = match resolution(req, v.r4k.is_some())? {
                "1080p" => v.r1080,
                "4k" => v.r4k.ok_or_else(|| {
                    ProviderError::new(
                        "invalid_request",
                        format!("{} has no 4k rate", v.model),
                        false,
                    )
                })?,
                _ => v.r720,
            };
            return Ok(variant_secs(v, req)? as f64 * if audio(req) { on } else { off });
        }
        Ok(match info.kind {
            Kind::Image => num_images(req) as f64 * IMAGE_USD_PER_MP * IMAGE_MP,
            Kind::Video => {
                let (off, on) = match resolution(req, true)? {
                    "4k" => (VIDEO_USD_PER_S_4K, VIDEO_USD_PER_S_4K_AUDIO),
                    _ => (VIDEO_USD_PER_S, VIDEO_USD_PER_S_AUDIO),
                };
                duration_secs(req)? as f64 * if audio(req) { on } else { off }
            }
            // Music/Sfx/Upscale/Lipsync are priced per call in the catalog.
            _ => info.est_usd,
        })
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        if let Some(entry) = self.declared(&req.model) {
            return self.post(&entry.endpoint, &entry.prepare(req)?.body, api_key);
        }
        let (endpoint, _) = self.resolve(req)?;
        let body = self.build_body(req, endpoint);
        self.post(endpoint, &body, api_key)
    }

    fn poll(
        &self,
        req: &GenRequest,
        provider_job_id: &str,
        api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        let mut parts = provider_job_id.splitn(3, '|');
        let rid = parts.next().unwrap_or("");
        let (endpoint, kind) = match self.declared(&req.model) {
            Some(entry) => (entry.endpoint.as_str(), entry.kind),
            None => {
                let (endpoint, info) = self.resolve(req)?;
                (endpoint, info.kind)
            }
        };
        // Only follow API-supplied URLs that stay on the queue host so the key
        // is never sent elsewhere.
        let trusted = |u: &str, fallback: String| {
            if !u.is_empty() && u.starts_with(&self.queue_base) {
                u.to_string()
            } else {
                fallback
            }
        };
        let status_url = trusted(
            parts.next().unwrap_or(""),
            format!("{}/{}/requests/{}/status", self.queue_base, endpoint, rid),
        );
        let response_url = trusted(
            parts.next().unwrap_or(""),
            format!("{}/{}/requests/{}", self.queue_base, endpoint, rid),
        );

        let r = self.get(&status_url, Some(api_key))?;
        // fal answers 200 for in-flight and 202 for some queued states.
        if !(200..300).contains(&r.status) {
            return Err(map_status(&r, api_key));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "fal status reply was not JSON", true)
        })?;
        // Confirmed-failure vocabulary (closed list): the status strings "FAILED" and "ERROR", and
        // nothing else. fal's queue documents IN_QUEUE / IN_PROGRESS / COMPLETED; the two failure
        // words are taken from the assignment, not from a fetched fal page (unverified), and are
        // the only replies that release the reservation. Every other reply that is not a plain
        // success (COMPLETED carrying `error`, a 422 result, a result with no media, any other
        // status string) is `Unknown`: the job is held with its reservation.
        match v.get("status").and_then(Value::as_str).unwrap_or("") {
            "IN_QUEUE" | "IN_PROGRESS" => Ok(PollOutcome::Running),
            "FAILED" | "ERROR" => {
                let detail = v.get("error").and_then(Value::as_str).unwrap_or("");
                let status = v.get("status").and_then(Value::as_str).unwrap_or("");
                Ok(PollOutcome::Failed {
                    message: redact(format!("fal status {status} {detail}").trim(), api_key),
                })
            }
            "COMPLETED" => {
                if v.get("error").is_some_and(|e| !e.is_null()) {
                    return Ok(unknown_outcome(
                        "COMPLETED with error",
                        &r.body,
                        api_key,
                        false,
                    ));
                }
                let r = self.get(&response_url, Some(api_key))?;
                if r.status == 422 {
                    // fal stores an application failure as a 422 result; whether it was billed is
                    // not stated, so it is held rather than released.
                    return Ok(unknown_outcome(
                        "COMPLETED, result 422",
                        &r.body,
                        api_key,
                        false,
                    ));
                }
                if !(200..300).contains(&r.status) {
                    return Err(map_status(&r, api_key));
                }
                let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
                    ProviderError::new("bad_response", "fal result was not JSON", true)
                })?;
                let outputs = parse_outputs(&v, kind);
                if outputs.is_empty() {
                    // fal finished the work; the reply just has no media this adapter can read.
                    Ok(unknown_outcome(
                        "COMPLETED, no usable media",
                        &r.body,
                        api_key,
                        true,
                    ))
                } else {
                    Ok(PollOutcome::Completed { outputs })
                }
            }
            "" => Err(ProviderError::new(
                "bad_response",
                "fal status reply had no status",
                true,
            )),
            other => Ok(unknown_outcome(other, &r.body, api_key, false)),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /v1/models is free and non-generating. fal serves it without a
        // key too, but rejects a malformed/invalid supplied key with 401, so
        // this proves the key is not rejected rather than that it can bill.
        let r = self.get(&format!("{}/models?limit=1", self.rest_base), Some(api_key))?;
        if (200..300).contains(&r.status) {
            Ok(())
        } else {
            Err(map_status(&r, api_key))
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
                // No credentials: fal media URLs are public CDN links.
                let r = self.get(url, None)?;
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
            provider: "fal".into(),
            model: model.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    const KEY: &str = concat!("fa", "l-secret-KEY-123");

    fn json_resp(v: Value) -> (u16, &'static str, Vec<u8>) {
        (200, "application/json", v.to_string().into_bytes())
    }

    #[test]
    fn image_happy_path_submit_poll_fetch() {
        let polls = Arc::new(Mutex::new(0));
        let p2 = polls.clone();
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/fal-ai/flux-2/klein/9b") => json_resp(json!({
                "request_id": "r1",
                "status_url": format!("{base}/fal-ai/flux-2/klein/9b/requests/r1/status"),
                "response_url": format!("{base}/fal-ai/flux-2/klein/9b/requests/r1"),
            })),
            ("GET", "/fal-ai/flux-2/klein/9b/requests/r1/status") => {
                let mut n = p2.lock().unwrap();
                *n += 1;
                json_resp(json!({"status": if *n == 1 { "IN_QUEUE" } else { "COMPLETED" }}))
            }
            ("GET", "/fal-ai/flux-2/klein/9b/requests/r1") => json_resp(json!({
                "images": [{"url": format!("{base}/files/a.png"), "content_type": "image/png"}]
            })),
            ("GET", "/files/a.png") => (200, "image/png", vec![1, 2, 3]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let fal = Fal::new(&base, &base);
        let r = req(
            Kind::Image,
            IMAGE_MODEL,
            json!({"aspect": "16:9", "num_images": 1}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = fal.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert!(provider_job_id.starts_with("r1|"));
        assert_eq!(
            fal.poll(&r, &provider_job_id, KEY).unwrap(),
            PollOutcome::Running
        );
        let PollOutcome::Completed { outputs } = fal.poll(&r, &provider_job_id, KEY).unwrap()
        else {
            panic!()
        };
        assert_eq!(outputs.len(), 1);
        let (bytes, mime) = fal.fetch(&outputs[0]).unwrap();
        assert_eq!((bytes, mime.as_str()), (vec![1, 2, 3], "image/png"));

        let seen = seen.lock().unwrap();
        let post = &seen[0];
        assert!(
            post.headers.contains(&format!("authorization: Key {KEY}"))
                || post.headers.contains(&format!("Authorization: Key {KEY}"))
        );
        let b: Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(b["image_size"], "landscape_16_9");
        assert_eq!(b["num_images"], 1);
        assert_eq!(b["prompt"], "a red fox");
        // the CDN fetch must not carry the key
        let get = seen.iter().find(|s| s.path == "/files/a.png").unwrap();
        assert!(!get.headers.contains(KEY));
    }

    #[test]
    fn video_uses_returned_urls_and_i2v_switch() {
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/fal-ai/veo3.1/fast/image-to-video") => json_resp(json!({
                "request_id": "v9",
                // app-root URLs differ from the endpoint path (real fal behaviour)
                "status_url": format!("{base}/fal-ai/veo3.1/requests/v9/status"),
                "response_url": format!("{base}/fal-ai/veo3.1/requests/v9"),
            })),
            ("GET", "/fal-ai/veo3.1/requests/v9/status") => {
                json_resp(json!({"status": "COMPLETED"}))
            }
            ("GET", "/fal-ai/veo3.1/requests/v9") => json_resp(json!({
                "video": {"url": format!("{base}/f/clip.mp4")}
            })),
            ("GET", "/f/clip.mp4") => (200, "application/octet-stream", vec![9; 4]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let fal = Fal::new(&base, &base);
        let r = req(
            Kind::Video,
            VIDEO_T2V,
            json!({"duration": 8, "aspect": "9:16", "image_url": "https://x/y.png"}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = fal.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let PollOutcome::Completed { outputs } = fal.poll(&r, &provider_job_id, KEY).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            outputs,
            vec![Output::Url {
                url: format!("{base}/f/clip.mp4"),
                mime: Some("video/mp4".into())
            }]
        );
        // octet-stream response falls back to the extension-derived mime
        let (bytes, mime) = fal.fetch(&outputs[0]).unwrap();
        assert_eq!((bytes.len(), mime.as_str()), (4, "video/mp4"));
        let b: Value = serde_json::from_str(&seen.lock().unwrap()[0].body).unwrap();
        assert_eq!(b["image_url"], "https://x/y.png");
        assert_eq!(b["duration"], "8s");
        assert_eq!(b["aspect_ratio"], "9:16");
        assert_eq!(b["resolution"], "720p");
        assert_eq!(b["generate_audio"], false);
        assert_eq!(
            seen.lock().unwrap()[0].path,
            "/fal-ai/veo3.1/fast/image-to-video"
        );
    }

    #[test]
    fn t2v_defaults_audio_off_and_validates_duration() {
        let (base, seen) = mock(move |base, _, _, _| {
            json_resp(json!({
                "request_id": "t1",
                "status_url": format!("{base}/fal-ai/veo3.1/requests/t1/status"),
                "response_url": format!("{base}/fal-ai/veo3.1/requests/t1"),
            }))
        });
        let fal = Fal::new(&base, &base);
        fal.submit(&req(Kind::Video, VIDEO_T2V, json!({})), KEY)
            .unwrap();
        fal.submit(
            &req(
                Kind::Video,
                VIDEO_T2V,
                json!({"duration": "6s", "generate_audio": true}),
            ),
            KEY,
        )
        .unwrap();
        let seen_log = seen.clone();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].path, "/fal-ai/veo3.1/fast");
        let b: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(b["generate_audio"], false);
        assert_eq!(b["duration"], "4s");
        assert_eq!(b["aspect_ratio"], "16:9");
        assert!(b.get("start_image_url").is_none() && b.get("image_url").is_none());
        let b: Value = serde_json::from_str(&seen[1].body).unwrap();
        assert_eq!(
            (b["duration"].as_str(), b["generate_audio"].as_bool()),
            (Some("6s"), Some(true))
        );
        drop(seen);
        for d in [
            json!(5),
            json!(3),
            json!(10),
            json!("5"),
            json!("abc"),
            json!(0),
        ] {
            let e = fal
                .submit(&req(Kind::Video, VIDEO_T2V, json!({"duration": d})), KEY)
                .unwrap_err();
            assert_eq!(e.code, "invalid_request");
        }
        assert_eq!(
            fal.estimate_usd(&req(Kind::Video, VIDEO_T2V, json!({"duration": 7})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        // rejected requests never reached the network
        assert_eq!(seen_log.lock().unwrap().len(), 2);
    }

    #[test]
    fn only_the_closed_failure_list_is_a_confirmed_failure() {
        let (base, _) = mock(move |_, m, path, _| match (m, path) {
            ("GET", p) if p.contains("/requests/bad1/status") => {
                json_resp(json!({"status": "COMPLETED", "error": "boom", "error_type": "x"}))
            }
            ("GET", p) if p.contains("/requests/bad2/status") => {
                json_resp(json!({"status": "COMPLETED"}))
            }
            ("GET", p) if p.contains("/requests/bad2") => (
                422,
                "application/json",
                br#"{"detail":"content policy"}"#.to_vec(),
            ),
            ("GET", p) if p.contains("/requests/bad3/status") => {
                json_resp(json!({"status": "FAILED"}))
            }
            ("GET", p) if p.contains("/requests/bad4/status") => {
                json_resp(json!({"status": "CANCELLED_BY_OPS"}))
            }
            ("GET", p) if p.contains("/requests/bad5/status") => {
                json_resp(json!({"status": "COMPLETED"}))
            }
            ("GET", p) if p.contains("/requests/bad5") => json_resp(json!({"unexpected": 1})),
            _ => (404, "text/plain", b"".to_vec()),
        });
        let fal = Fal::new(&base, &base);
        let r = req(Kind::Image, IMAGE_MODEL, json!({}));
        // old: every one of these was `Failed` (released, then resubmitted); new: only FAILED/ERROR is.
        match fal.poll(&r, "bad3||", KEY).unwrap() {
            PollOutcome::Failed { message } => assert!(message.contains("FAILED"), "{message}"),
            other => panic!("{other:?}"),
        }
        for (id, needle, status, completed) in [
            ("bad1||", "boom", "COMPLETED with error", false),
            ("bad2||", "content policy", "COMPLETED, result 422", false),
            ("bad4||", "CANCELLED_BY_OPS", "CANCELLED_BY_OPS", false),
            ("bad5||", "unexpected", "COMPLETED, no usable media", true),
        ] {
            match fal.poll(&r, id, KEY).unwrap() {
                PollOutcome::Unknown {
                    status: s,
                    detail,
                    completed: c,
                } => {
                    assert_eq!((s.as_str(), c), (status, completed), "{id}");
                    assert!(detail.contains(needle), "{detail}");
                }
                other => panic!("{id}: {other:?}"),
            }
        }
    }

    #[test]
    fn error_mapping_and_no_key_leak() {
        for (status, code, retry) in [
            (401, "auth", false),
            (403, "auth", false),
            (429, "rate_limited", true),
            (503, "unavailable", true),
            (422, "invalid_request", false),
        ] {
            let (base, _) = mock(move |_, _, _, _| {
                (
                    status,
                    "application/json",
                    format!(r#"{{"detail":"bad key {KEY}"}}"#).into_bytes(),
                )
            });
            let fal = Fal::new(&base, &base);
            let e = fal
                .submit(&req(Kind::Image, IMAGE_MODEL, json!({})), KEY)
                .unwrap_err();
            assert_eq!(
                (e.code.as_str(), e.retryable),
                (code, retry),
                "status {status}"
            );
            assert!(!e.message.contains(KEY), "{}", e.message);
        }
        // transport failure: retryable, key absent
        let fal = Fal::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        let e = fal
            .submit(&req(Kind::Image, IMAGE_MODEL, json!({})), KEY)
            .unwrap_err();
        assert!(e.retryable && e.code == "transport" && !e.message.contains(KEY));
    }

    /// A server that reads the whole request head, then closes without replying:
    /// the request was delivered, the answer is lost.
    fn hang_up_after_request() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut st) = stream else { continue };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = st.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
            }
        });
        base
    }

    #[test]
    fn submit_errors_are_ambiguous_only_when_the_request_may_have_been_delivered() {
        // Definite rejections: the provider answered and refused.
        for status in [400, 401, 402, 403, 422] {
            let (base, _) = mock(move |_, _, _, _| (status, "application/json", b"{}".to_vec()));
            let e = Fal::new(&base, &base)
                .submit(&req(Kind::Image, IMAGE_MODEL, json!({})), KEY)
                .unwrap_err();
            assert!(!e.ambiguous && !e.retryable, "status {status}: {e:?}");
        }
        // The answer says nothing about whether work was queued.
        for status in [408, 429, 500, 503] {
            let (base, _) = mock(move |_, _, _, _| (status, "application/json", b"{}".to_vec()));
            let e = Fal::new(&base, &base)
                .submit(&req(Kind::Image, IMAGE_MODEL, json!({})), KEY)
                .unwrap_err();
            assert!(e.ambiguous && e.retryable, "status {status}: {e:?}");
        }
        // An unreadable success reply.
        let (base, _) = mock(|_, _, _, _| (200, "text/plain", b"not json".to_vec()));
        let e = Fal::new(&base, &base)
            .submit(&req(Kind::Image, IMAGE_MODEL, json!({})), KEY)
            .unwrap_err();
        assert!(e.ambiguous && e.code == "bad_response", "{e:?}");
        // The connection dropped after the request was sent.
        let base = hang_up_after_request();
        let e = Fal::new(&base, &base)
            .submit(&req(Kind::Image, IMAGE_MODEL, json!({})), KEY)
            .unwrap_err();
        assert!(e.ambiguous && e.retryable && e.code == "transport", "{e:?}");
        // Connect refused: nothing was sent.
        let e = Fal::new("http://127.0.0.1:1", "http://127.0.0.1:1")
            .submit(&req(Kind::Image, IMAGE_MODEL, json!({})), KEY)
            .unwrap_err();
        assert!(
            !e.ambiguous && e.retryable && e.code == "transport",
            "{e:?}"
        );
        // Validation fails before any request is built.
        let e = Fal::new("http://127.0.0.1:1", "http://127.0.0.1:1")
            .submit(&req(Kind::Image, "nope", json!({})), KEY)
            .unwrap_err();
        assert!(!e.ambiguous && !e.retryable);
        // Poll failures never carry the flag.
        let (base, _) = mock(|_, _, _, _| (503, "application/json", b"{}".to_vec()));
        let e = Fal::new(&base, &base)
            .poll(
                &req(Kind::Image, IMAGE_MODEL, json!({})),
                "rid|http://x/s|http://x/r",
                KEY,
            )
            .unwrap_err();
        assert!(e.retryable && !e.ambiguous, "{e:?}");
    }
    #[test]
    fn key_check_and_estimates() {
        let (base, seen) = mock(|_, _, _, _| (200, "application/json", b"{}".to_vec()));
        let fal = Fal::new(&base, &base);
        fal.test_key(KEY).unwrap();
        assert!(seen.lock().unwrap()[0].path.starts_with("/models"));
        let (base, _) = mock(|_, _, _, _| (401, "application/json", b"{}".to_vec()));
        let e = Fal::new(&base, &base).test_key(KEY).unwrap_err();
        assert_eq!(e.code, "auth");

        let fal = Fal::new("http://x", "http://x");
        let img = fal
            .estimate_usd(&req(Kind::Image, IMAGE_MODEL, json!({"num_images": 2})))
            .unwrap();
        assert!((img - 0.0126).abs() < 1e-9);
        let vid = fal
            .estimate_usd(&req(Kind::Video, VIDEO_T2V, json!({})))
            .unwrap();
        assert!((vid - 0.40).abs() < 1e-9);
        let vida = fal
            .estimate_usd(&req(
                Kind::Video,
                VIDEO_T2V,
                json!({"duration": "8s", "generate_audio": true}),
            ))
            .unwrap();
        assert!((vida - 1.20).abs() < 1e-9);
        assert_eq!(
            fal.estimate_usd(&req(Kind::Image, "nope", json!({})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        // 18 built in plus the 41 fal entries of the embedded data catalogue.
        assert_eq!(fal.models().len(), 59);
    }

    fn body_of(seen: &Arc<Mutex<Vec<Seen>>>, i: usize) -> Value {
        serde_json::from_str(&seen.lock().unwrap()[i].body).unwrap()
    }

    fn vreq(model: &str, params: Value) -> GenRequest {
        req(Kind::Video, model, params)
    }

    #[test]
    fn veo_variants_bodies_prices_and_validation() {
        let (base, seen) = mock(|_, _, _, _| {
            json_resp(json!({"request_id": "r1", "status_url": "", "response_url": ""}))
        });
        let fal = Fal::new(&base, &base);
        let a = "https://x/a.png";
        let b = "https://x/b.png";
        let cases = [
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "duration": 6}),
                6.0 * 0.20,
            ),
            (
                "fal-ai/veo3.1/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "seed": 5}),
                4.0 * 0.20,
            ),
            (
                "fal-ai/veo3.1/fast/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "duration": 8, "generate_audio": true}),
                8.0 * 0.15,
            ),
            (
                "fal-ai/veo3.1/lite/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "1080p"}),
                4.0 * 0.05,
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a, b], "aspect": "9:16"}),
                8.0 * 0.20,
            ),
        ];
        for (m, p, usd) in &cases {
            let r = vreq(m, p.clone());
            assert!((fal.estimate_usd(&r).unwrap() - usd).abs() < 1e-9, "{m}");
            fal.submit(&r, KEY).unwrap();
        }
        let s = seen.lock().unwrap();
        for (i, (m, _, _)) in cases.iter().enumerate() {
            assert_eq!(s[i].path, format!("/{m}"));
            assert!(s[i]
                .headers
                .to_ascii_lowercase()
                .contains(&format!("authorization: key {KEY}").to_ascii_lowercase()));
        }
        drop(s);
        let i2v = body_of(&seen, 0);
        assert_eq!(
            (
                i2v["image_url"].as_str(),
                i2v["duration"].as_str(),
                i2v["generate_audio"].as_bool(),
                i2v["aspect_ratio"].as_str()
            ),
            (Some(a), Some("6s"), Some(false), Some("auto"))
        );
        let flf = body_of(&seen, 1);
        assert_eq!(
            (
                flf["first_frame_url"].as_str(),
                flf["last_frame_url"].as_str(),
                flf["seed"].as_i64()
            ),
            (Some(a), Some(b), Some(5))
        );
        assert!(flf.get("image_url").is_none());
        assert_eq!(body_of(&seen, 2)["generate_audio"], true);
        assert_eq!(body_of(&seen, 3)["resolution"], "1080p");
        let rf = body_of(&seen, 4);
        assert_eq!(
            (
                rf["image_urls"].clone(),
                rf["duration"].as_str(),
                rf["aspect_ratio"].as_str()
            ),
            (json!([a, b]), Some("8s"), Some("9:16"))
        );
        assert!(rf.get("seed").is_none() && rf.get("negative_prompt").is_none());
        let lite = vreq(
            "fal-ai/veo3.1/lite/first-last-frame-to-video",
            json!({"image_url": a, "end_image_url": b}),
        );
        assert!((fal.estimate_usd(&lite).unwrap() - 0.12).abs() < 1e-9);
        let n = cases.len();
        for (m, p) in [
            ("fal-ai/veo3.1/image-to-video", json!({})),
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "duration": 5}),
            ),
            (
                "fal-ai/veo3.1/first-last-frame-to-video",
                json!({"image_url": a}),
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": []}),
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a, a, a, a]}),
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a], "duration": 4}),
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a], "seed": 1}),
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a], "negative_prompt": "x"}),
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a], "aspect": "1:1"}),
            ),
            // Lite has no 4K; every other row does (see `veo_4k_rates_bodies_and_the_row_that_refuses_it`).
            (
                "fal-ai/veo3.1/lite/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "4k"}),
            ),
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "resolution": "2160p"}),
            ),
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "resolution": "4K"}),
            ),
        ] {
            assert!(fal.submit(&vreq(m, p.clone()), KEY).is_err(), "{m} {p}");
        }
        assert!(fal
            .submit(
                &req(Kind::Image, "fal-ai/veo3.1/image-to-video", json!({})),
                KEY
            )
            .is_err());
        assert_eq!(seen.lock().unwrap().len(), n);
    }

    /// 4K (`resolution: "4k"`, fal pages and OpenAPI schemas read 2026-10-12): the per-second rates by row,
    /// the body that is sent, and the Lite row that still refuses it. Every pre-existing price is unchanged.
    #[test]
    fn veo_4k_rates_bodies_and_the_row_that_refuses_it() {
        let (base, seen) = mock(|_, _, _, _| {
            json_resp(json!({"request_id": "r1", "status_url": "", "response_url": ""}))
        });
        let fal = Fal::new(&base, &base);
        let a = "https://x/a.png";
        let b = "https://x/b.png";
        // (model, params, expected USD): no audio then audio, 4s default or the stated duration.
        let cases = [
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "resolution": "4k"}),
                4.0 * 0.40,
            ),
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "resolution": "4k", "generate_audio": true, "duration": 6}),
                6.0 * 0.60,
            ),
            (
                "fal-ai/veo3.1/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "4k", "duration": "8s"}),
                8.0 * 0.40,
            ),
            (
                "fal-ai/veo3.1/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "4k", "generate_audio": true}),
                4.0 * 0.60,
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a, b], "resolution": "4k"}),
                8.0 * 0.40,
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a], "resolution": "4k", "generate_audio": true}),
                8.0 * 0.60,
            ),
            (
                "fal-ai/veo3.1/fast/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "4k"}),
                4.0 * 0.30,
            ),
            (
                "fal-ai/veo3.1/fast/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "4k", "generate_audio": true, "duration": 8}),
                8.0 * 0.35,
            ),
            (VIDEO_T2V, json!({"resolution": "4k"}), 4.0 * 0.30),
            (
                VIDEO_T2V,
                json!({"resolution": "4k", "generate_audio": true, "duration": "6s"}),
                6.0 * 0.35,
            ),
            (
                VIDEO_I2V,
                json!({"image_url": a, "resolution": "4k", "duration": 8}),
                8.0 * 0.30,
            ),
            // The T2V row switches to the image endpoint when an image is given; the 4k rate is the same.
            (
                VIDEO_T2V,
                json!({"image_url": a, "resolution": "4k", "generate_audio": true}),
                4.0 * 0.35,
            ),
        ];
        for (m, p, usd) in &cases {
            let r = vreq(m, p.clone());
            assert!(
                (fal.estimate_usd(&r).unwrap() - usd).abs() < 1e-9,
                "{m} {p}"
            );
            fal.submit(&r, KEY).unwrap();
        }
        let s = seen.lock().unwrap();
        assert_eq!(s.len(), cases.len());
        drop(s);
        for i in 0..cases.len() {
            assert_eq!(
                body_of(&seen, i)["resolution"],
                "4k",
                "case {i} sends the same resolution value fal's schema names"
            );
        }
        assert_eq!(
            seen.lock().unwrap()[11].path,
            "/fal-ai/veo3.1/fast/image-to-video"
        );

        // Pre-existing prices at 720p and 1080p are exactly what they were.
        for (m, p, usd) in [
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "resolution": "1080p"}),
                4.0 * 0.20,
            ),
            (
                "fal-ai/veo3.1/image-to-video",
                json!({"image_url": a, "generate_audio": true}),
                4.0 * 0.40,
            ),
            (
                "fal-ai/veo3.1/reference-to-video",
                json!({"image_urls": [a], "resolution": "720p"}),
                8.0 * 0.20,
            ),
            (
                "fal-ai/veo3.1/fast/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b}),
                4.0 * 0.10,
            ),
            (
                "fal-ai/veo3.1/fast/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "1080p", "generate_audio": true}),
                4.0 * 0.15,
            ),
            (
                "fal-ai/veo3.1/lite/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b}),
                4.0 * 0.03,
            ),
            (
                "fal-ai/veo3.1/lite/first-last-frame-to-video",
                json!({"image_url": a, "end_image_url": b, "resolution": "1080p", "generate_audio": true}),
                4.0 * 0.08,
            ),
            (VIDEO_T2V, json!({"resolution": "1080p"}), 4.0 * 0.10),
            (
                VIDEO_T2V,
                json!({"duration": "8s", "generate_audio": true}),
                8.0 * 0.15,
            ),
        ] {
            assert!(
                (fal.estimate_usd(&vreq(m, p.clone())).unwrap() - usd).abs() < 1e-9,
                "{m} {p}"
            );
        }

        // The Lite row has no 4K in its schema or on its page: refused in the estimate and the submit, never sent.
        let lite = "fal-ai/veo3.1/lite/first-last-frame-to-video";
        let before = seen.lock().unwrap().len();
        let p = json!({"image_url": a, "end_image_url": b, "resolution": "4k"});
        let e = fal.estimate_usd(&vreq(lite, p.clone())).unwrap_err();
        assert_eq!(e.code, "invalid_request");
        assert!(
            e.message
                .contains("unsupported Veo resolution 4k (720p or 1080p)"),
            "{}",
            e.message
        );
        assert_eq!(
            fal.submit(&vreq(lite, p), KEY).unwrap_err().code,
            "invalid_request"
        );
        // 4k is the only extra value: no other spelling or size is accepted on any row.
        for res in ["2160p", "4K", "4096p", ""] {
            let p = json!({"image_url": a, "resolution": res});
            assert!(
                fal.estimate_usd(&vreq("fal-ai/veo3.1/image-to-video", p))
                    .is_err(),
                "{res}"
            );
        }
        assert_eq!(
            seen.lock().unwrap().len(),
            before,
            "refused requests never reached the network"
        );

        // What the model list advertises follows the same rule.
        let caps = |m: &str| fal.capabilities(m).settings["resolution"]["values"].clone();
        assert_eq!(
            caps("fal-ai/veo3.1/image-to-video"),
            json!(["720p", "1080p", "4k"])
        );
        assert_eq!(caps(VIDEO_T2V), json!(["720p", "1080p", "4k"]));
        assert_eq!(caps(lite), json!(["720p", "1080p"]));
    }

    #[test]
    fn music_sfx_upscale_lipsync_routes() {
        // One submit per route against a single mock that records bodies.
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", p) => json_resp(json!({
                "request_id": "q1",
                "status_url": format!("{base}{p}/requests/q1/status"),
                "response_url": format!("{base}{p}/requests/q1"),
            })),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let fal = Fal::new(&base, &base);
        fal.submit(
            &req(Kind::Music, STABLE_AUDIO, json!({"seconds_total": 12})),
            KEY,
        )
        .unwrap();
        fal.submit(&req(Kind::Music, STABLE_AUDIO_25, json!({})), KEY)
            .unwrap();
        fal.submit(
            &req(Kind::Sfx, MMAUDIO, json!({"video_url": "https://x/v.mp4"})),
            KEY,
        )
        .unwrap();
        fal.submit(
            &req(
                Kind::Upscale,
                CREATIVE_UPSCALER,
                json!({"image_url": "https://x/i.png", "scale": 2}),
            ),
            KEY,
        )
        .unwrap();
        fal.submit(
            &req(
                Kind::Upscale,
                TOPAZ_VIDEO_UPSCALE,
                json!({"video_url": "https://x/v.mp4", "scale": 2}),
            ),
            KEY,
        )
        .unwrap();
        fal.submit(
            &req(
                Kind::Lipsync,
                SYNC_LIPSYNC,
                json!({"video_url": "https://x/v.mp4", "audio_url": "https://x/a.mp3"}),
            ),
            KEY,
        )
        .unwrap();
        let s = seen.lock().unwrap();
        assert_eq!(s[0].path, "/fal-ai/stable-audio");
        assert_eq!(s[1].path, "/fal-ai/stable-audio-25/text-to-audio");
        assert_eq!(s[2].path, "/fal-ai/mmaudio/v2");
        assert_eq!(s[3].path, "/fal-ai/creative-upscaler");
        assert_eq!(s[4].path, "/fal-ai/topaz/upscale/video");
        assert_eq!(s[5].path, "/fal-ai/sync-lipsync");
        for req_line in s.iter() {
            assert!(req_line
                .headers
                .to_ascii_lowercase()
                .contains(&format!("authorization: key {KEY}").to_ascii_lowercase()));
        }
        drop(s);
        let b = body_of(&seen, 0);
        assert_eq!(
            (b["prompt"].as_str(), b["seconds_total"].as_u64()),
            (Some("a red fox"), Some(12))
        );
        let b = body_of(&seen, 2);
        assert_eq!(b["video_url"], "https://x/v.mp4");
        let b = body_of(&seen, 3);
        assert_eq!(
            (b["image_url"].as_str(), b["scale"].as_f64()),
            (Some("https://x/i.png"), Some(2.0))
        );
        let b = body_of(&seen, 4);
        assert_eq!(
            (b["video_url"].as_str(), b["upscale_factor"].as_f64()),
            (Some("https://x/v.mp4"), Some(2.0))
        );
        let b = body_of(&seen, 5);
        assert_eq!(
            (b["video_url"].as_str(), b["audio_url"].as_str()),
            (Some("https://x/v.mp4"), Some("https://x/a.mp3"))
        );
    }

    #[test]
    fn new_kind_validation_never_reaches_network() {
        let (base, seen) = mock(|_, _, _, _| {
            json_resp(json!({"request_id": "q", "status_url": "", "response_url": ""}))
        });
        let fal = Fal::new(&base, &base);
        let cases = [
            req(Kind::Sfx, MMAUDIO, json!({})), // missing video_url
            req(Kind::Upscale, CREATIVE_UPSCALER, json!({})), // missing image_url
            req(Kind::Upscale, TOPAZ_VIDEO_UPSCALE, json!({})), // missing video_url
            req(
                Kind::Lipsync,
                LATENTSYNC,
                json!({"video_url": "https://x/v"}),
            ), // missing audio_url
            req(
                Kind::Lipsync,
                SYNC_LIPSYNC,
                json!({"audio_url": "https://x/a"}),
            ), // missing video_url
            req(Kind::Music, STABLE_AUDIO, json!({"duration": "abc"})),
            req(Kind::Music, STABLE_AUDIO, json!({"seconds_total": 9000})),
            req(
                Kind::Image,
                SYNC_LIPSYNC,
                json!({"video_url": "https://x/v", "audio_url": "https://x/a"}),
            ),
        ];
        for r in cases {
            assert_eq!(
                fal.submit(&r, KEY).unwrap_err().code,
                "invalid_request",
                "{}",
                r.model
            );
            assert_eq!(fal.estimate_usd(&r).unwrap_err().code, "invalid_request");
        }
        assert_eq!(seen.lock().unwrap().len(), 0);
        // kind mismatch on an existing model
        assert_eq!(
            fal.submit(&req(Kind::Video, MMAUDIO, json!({})), KEY)
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn audio_output_keys_parse() {
        // stable-audio returns `audio_file`; mmaudio may return `video`.
        let (base, _) = mock(|base, _, path, _| {
            if path.ends_with("/status") {
                return json_resp(json!({"status": "COMPLETED"}));
            }
            json_resp(
                json!({"audio_file": {"url": format!("{base}/f/song.wav", ), "content_type": "audio/wav"}}),
            )
        });
        let fal = Fal::new(&base, &base);
        let r = req(Kind::Music, STABLE_AUDIO, json!({}));
        let PollOutcome::Completed { outputs } = fal.poll(&r, "rid||", KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            outputs,
            vec![Output::Url {
                url: format!("{base}/f/song.wav"),
                mime: Some("audio/wav".into())
            }]
        );
    }

    #[test]
    fn veo_variant_durations_are_advertised() {
        let fal = Fal::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        let m = |id: &str| {
            serde_json::to_value(fal.models().iter().find(|m| m.model == id).unwrap()).unwrap()
        };
        assert_eq!(
            m("fal-ai/veo3.1/reference-to-video")["durations_s"],
            json!([8])
        );
        assert_eq!(
            m("fal-ai/veo3.1/lite/first-last-frame-to-video")["durations_s"],
            json!([4, 6, 8])
        );
        // 7 built in plus the 12 fal video entries of the embedded data catalogue.
        assert_eq!(
            fal.models()
                .iter()
                .filter(|m| m.kind == Kind::Video)
                .count(),
            19
        );
    }

    #[test]
    fn any_llm_submits_chat_shape_and_returns_text_bytes() {
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/fal-ai/any-llm") => json_resp(json!({
                "request_id": "t1",
                "status_url": format!("{base}/fal-ai/any-llm/requests/t1/status"),
                "response_url": format!("{base}/fal-ai/any-llm/requests/t1"),
            })),
            ("GET", "/fal-ai/any-llm/requests/t1/status") => {
                json_resp(json!({"status": "COMPLETED"}))
            }
            ("GET", "/fal-ai/any-llm/requests/t1") => json_resp(json!({
                "choices": [{"message": {"role": "assistant", "content": "Hallo Welt"}}],
                "usage": {"total_tokens": 12}
            })),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let fal = Fal::new(&base, &base);
        let r = req(
            Kind::Text,
            ANY_LLM,
            json!({"llm": "openai/gpt-4o-mini", "system_prompt": "translate to de"}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = fal.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let PollOutcome::Completed { outputs } = fal.poll(&r, &provider_job_id, KEY).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            outputs,
            vec![Output::Bytes {
                bytes: b"Hallo Welt".to_vec(),
                mime: "text/plain".into()
            }]
        );
        let (bytes, mime) = fal.fetch(&outputs[0]).unwrap();
        assert_eq!(
            (bytes, mime.as_str()),
            (b"Hallo Welt".to_vec(), "text/plain")
        );

        let seen = seen.lock().unwrap();
        let post = seen.iter().find(|s| s.method == "POST").unwrap();
        assert!(post.headers.contains(&format!("Key {KEY}")));
        let b: Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(b["model"], "openai/gpt-4o-mini");
        assert_eq!(b["prompt"], "a red fox");
        assert_eq!(b["system_prompt"], "translate to de");
    }

    #[test]
    fn any_llm_defaults_and_output_string_shape() {
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("GET", "/s") => json_resp(json!({"status": "COMPLETED"})),
            ("GET", "/r") => json_resp(json!({"output": "Bonjour"})),
            ("POST", "/fal-ai/any-llm") => json_resp(json!({
                "request_id": "t2",
                "status_url": format!("{base}/s"),
                "response_url": format!("{base}/r"),
            })),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let fal = Fal::new(&base, &base);
        let r = req(Kind::Text, ANY_LLM, json!({}));
        let SubmitOutcome::Accepted { provider_job_id } = fal.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let PollOutcome::Completed { outputs } = fal.poll(&r, &provider_job_id, KEY).unwrap()
        else {
            panic!()
        };
        let Output::Bytes { bytes, .. } = &outputs[0] else {
            panic!()
        };
        assert_eq!(bytes, b"Bonjour");
        let post = seen
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.method == "POST")
            .unwrap()
            .body
            .clone();
        let b: Value = serde_json::from_str(&post).unwrap();
        assert_eq!(b["model"], ANY_LLM_DEFAULT);
        assert!(
            (fal.estimate_usd(&r).unwrap() - 0.001).abs() < 1e-9,
            "one request at 0.001 USD"
        );
    }

    #[test]
    fn scribe_v2_submits_audio_url_and_returns_transcript_json() {
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/fal-ai/elevenlabs/speech-to-text/scribe-v2") => json_resp(json!({
                "request_id": "s1",
                "status_url": format!("{base}/fal-ai/elevenlabs/speech-to-text/requests/s1/status"),
                "response_url": format!("{base}/fal-ai/elevenlabs/speech-to-text/requests/s1"),
            })),
            ("GET", "/fal-ai/elevenlabs/speech-to-text/requests/s1/status") => {
                json_resp(json!({"status": "COMPLETED"}))
            }
            ("GET", "/fal-ai/elevenlabs/speech-to-text/requests/s1") => json_resp(json!({
                "language_code": "eng",
                "language_probability": 1,
                "text": "Hello there. Bye now.",
                "words": [
                    {"text": "Hello", "start": 0.0, "end": 0.4, "type": "word", "speaker_id": "speaker_0"},
                    {"text": " ", "start": 0.4, "end": 0.5, "type": "spacing", "speaker_id": "speaker_0"},
                    {"text": "there.", "start": 0.5, "end": 0.9, "type": "word", "speaker_id": "speaker_0"},
                    {"text": "Bye", "start": 2.5, "end": 2.8, "type": "word", "speaker_id": "speaker_1"},
                    {"text": "now.", "start": 2.8, "end": 3.1, "type": "word", "speaker_id": "speaker_1"}
                ]
            })),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let fal = Fal::new(&base, &base);
        let r = req(
            Kind::Transcribe,
            SCRIBE_V2,
            json!({"audio_url": "https://x/a.mp3", "keyterms": ["fal"], "duration_seconds": 120}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = fal.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let PollOutcome::Completed { outputs } = fal.poll(&r, &provider_job_id, KEY).unwrap()
        else {
            panic!()
        };
        let (bytes, mime) = fal.fetch(&outputs[0]).unwrap();
        assert_eq!(mime, "application/json");
        let t: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(t["text"], "Hello there. Bye now.");
        assert_eq!(t["language_code"], "eng");
        assert_eq!(
            t["segments"],
            json!([
                {"start": 0.0, "end": 0.9, "text": "Hello there.", "speaker": "speaker_0"},
                {"start": 2.5, "end": 3.1, "text": "Bye now.", "speaker": "speaker_1"}
            ])
        );
        {
            let seen = seen.lock().unwrap();
            let post = seen.iter().find(|s| s.method == "POST").unwrap();
            assert!(post.headers.contains(&format!("Key {KEY}")));
            let b: Value = serde_json::from_str(&post.body).unwrap();
            assert_eq!(b["audio_url"], "https://x/a.mp3");
            assert_eq!(b["keyterms"], json!(["fal"]));
            assert!(b.get("prompt").is_none());
        }
        // 2 audio minutes at $0.008/min, +30% with keyterms.
        assert!((fal.estimate_usd(&r).unwrap() - 2.0 * 0.008 * 1.3).abs() < 1e-9);
        let no_audio = req(Kind::Transcribe, SCRIBE_V2, json!({}));
        assert_eq!(
            fal.submit(&no_audio, KEY).unwrap_err().code,
            "invalid_request"
        );
        let wrong_kind = req(
            Kind::Text,
            SCRIBE_V2,
            json!({"audio_url": "https://x/a.mp3"}),
        );
        assert_eq!(
            fal.submit(&wrong_kind, KEY).unwrap_err().code,
            "invalid_request"
        );
    }

    #[test]
    fn text_kind_rejects_empty_prompt_and_wrong_kind() {
        let fal = Fal::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        let mut empty = req(Kind::Text, ANY_LLM, json!({}));
        empty.prompt = "  ".into();
        assert_eq!(fal.submit(&empty, KEY).unwrap_err().code, "invalid_request");
        assert_eq!(
            fal.submit(&req(Kind::Image, ANY_LLM, json!({})), KEY)
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    // ---- declared models (catalogue entries) ----

    use crate::catalogue::testing;

    const DECLARED: &str = "fal-ai/test/declared";

    fn declared_req(params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::MotionControl,
            provider: "fal".into(),
            model: DECLARED.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    fn declared_params() -> Value {
        json!({"video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"]})
    }

    fn declared_fal(base: &str, models: Vec<Value>) -> Fal {
        Fal::new(base, base).with_catalogue(testing::catalogue(models))
    }

    #[test]
    fn declared_model_lists_its_reference_images_and_built_ins_declare_none() {
        let a = declared_fal(
            "http://unused.invalid",
            vec![testing::sample("fal", DECLARED)],
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
    fn declared_model_posts_exact_body_to_its_endpoint_and_extracts_outputs() {
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/fal-ai/test/declared/v1") => json_resp(json!({
                "request_id": "d1",
                "status_url": format!("{base}/fal-ai/test/requests/d1/status"),
                "response_url": format!("{base}/fal-ai/test/requests/d1"),
            })),
            ("GET", "/fal-ai/test/requests/d1/status") => json_resp(json!({"status": "COMPLETED"})),
            ("GET", "/fal-ai/test/requests/d1") => {
                json_resp(json!({"video": {"url": format!("{base}/f/out.mp4")}}))
            }
            ("GET", "/f/out.mp4") => (200, "video/mp4", vec![5, 6, 7]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let fal = declared_fal(&base, vec![testing::sample("fal", DECLARED)]);
        let r = declared_req(json!({
            "video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"], "duration": 8, "resolution": "1080p"
        }));
        assert!((fal.estimate_usd(&r).unwrap() - 1.6).abs() < 1e-12);
        let SubmitOutcome::Accepted { provider_job_id } = fal.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let PollOutcome::Completed { outputs } = fal.poll(&r, &provider_job_id, KEY).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            fal.fetch(&outputs[0]).unwrap(),
            (vec![5, 6, 7], "video/mp4".to_string())
        );

        let seen = seen.lock().unwrap();
        let post = &seen[0];
        assert_eq!(
            (post.method.as_str(), post.path.as_str()),
            ("POST", "/fal-ai/test/declared/v1")
        );
        assert!(
            post.headers.contains(&format!("authorization: Key {KEY}"))
                || post.headers.contains(&format!("Authorization: Key {KEY}"))
        );
        assert_eq!(
            serde_json::from_str::<Value>(&post.body).unwrap(),
            json!({
                "mode": "std", "prompt": "a red fox",
                "video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"],
                "duration": 8, "resolution": "1080p", "keep_original_sound": true
            })
        );
        assert_eq!(seen.iter().filter(|s| s.method == "POST").count(), 1);
    }

    #[test]
    fn declared_model_rejects_bad_requests_before_any_http_call() {
        let (base, seen) = mock(|_, _, _, _| (500, "text/plain", b"must not be called".to_vec()));
        let fal = declared_fal(&base, vec![testing::sample("fal", DECLARED)]);
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
            (
                declared_req(json!({"video_url": "https://x/v.mp4"})),
                "params.image_urls is required",
            ),
            (with(json!({"resolution": "4k"})), "must be one of"),
            (with(json!({"duration": 2})), "must be at least 3"),
            (with(json!({"duration": 16})), "must be at most 15"),
            (with(json!({"duration": "8"})), "must be an integer"),
            (with(json!({"duration": 7.5})), "must be an integer"),
            (with(json!({"keep_audio": "yes"})), "must be a boolean"),
            (with(json!({"strength": 1.5})), "must be at most 1"),
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
                with(json!({"image_urls": ["https://x/a.png", "ftp://x/b.png"]})),
                "item 1 must be an https URL",
            ),
            (
                with(json!({"strength": 0.5, "intensity": 0.6})),
                "are the same input",
            ),
            (
                declared_req(json!("not an object")),
                "params must be an object",
            ),
            (
                GenRequest {
                    kind: Kind::Video,
                    ..with(json!({}))
                },
                "does not match request kind",
            ),
        ];
        for (r, needle) in cases {
            for e in [
                fal.submit(&r, KEY).unwrap_err(),
                fal.estimate_usd(&r).unwrap_err(),
            ] {
                assert_eq!(e.code, "invalid_request", "{needle}");
                assert!(
                    e.message.contains(needle),
                    "{:?} lacks {needle:?}",
                    e.message
                );
                assert!(!e.retryable);
            }
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "rejected requests never reached the network"
        );
    }

    #[test]
    fn declared_model_applies_defaults_aliases_and_null_params() {
        let (base, seen) = mock(|_, _, _, _| json_resp(json!({"request_id": "d2"})));
        let fal = declared_fal(&base, vec![testing::sample("fal", DECLARED)]);
        let mut p = declared_params();
        p["intensity"] = json!(0.25);
        p["duration"] = Value::Null;
        fal.submit(&declared_req(p), KEY).unwrap();
        assert_eq!(
            body_of(&seen, 0),
            json!({
                "mode": "std", "prompt": "a red fox",
                "video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"],
                "duration": 5, "resolution": "720p", "keep_original_sound": true, "strength": 0.25
            })
        );
        assert!((fal.estimate_usd(&declared_req(declared_params())).unwrap() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn declared_model_estimates_each_pricing_kind() {
        let priced = |name: &str, pricing: Value| {
            let mut v = testing::sample("fal", name);
            v["inputs"].as_array_mut().unwrap().push(json!({"param": "num_images", "json_key": "num_images", "type": "integer", "default": 1, "min": 1, "max": 4}));
            v["pricing"] = pricing;
            v
        };
        let fal = declared_fal(
            &base_unused(),
            vec![
                priced("m/flat", json!({"kind": "flat", "usd": 0.75})),
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
            fal.estimate_usd(&GenRequest {
                model: model.into(),
                ..declared_req(p)
            })
            .unwrap()
        };
        assert_eq!(est("m/flat", json!({})), 0.75);
        assert_eq!(est("m/second", json!({"duration": 8})), 1.0);
        assert_eq!(est("m/second", json!({})), 0.625);
        assert_eq!(
            est("m/tier", json!({"duration": 8, "resolution": "1080p"})),
            1.6
        );
        assert_eq!(est("m/tier", json!({"duration": 10})), 1.0);
        assert_eq!(est("m/item", json!({"num_images": 3})), 0.04 * 3.0);
        assert_eq!(est("m/mp", json!({})), 0.006 * 1.05);
    }

    fn base_unused() -> String {
        "http://127.0.0.1:1".into()
    }

    #[test]
    fn declared_models_follow_the_rust_rows_and_leave_them_untouched() {
        let fal = declared_fal(
            &base_unused(),
            vec![
                testing::sample("fal", DECLARED),
                testing::sample("wavespeed", "vendor/not-fal"),
            ],
        );
        let rows = fal.models();
        assert_eq!(rows.len(), MODELS.len() + 1);
        for (a, b) in rows.iter().zip(MODELS.iter()) {
            assert_eq!((a.model, a.kind, a.est_usd), (b.model, b.kind, b.est_usd));
        }
        let last = rows.last().unwrap();
        assert_eq!(
            (last.provider, last.model, last.kind),
            ("fal", DECLARED, Kind::MotionControl)
        );
        assert!((last.est_usd - 0.5).abs() < 1e-12);
        // Built once: later calls return the same slice.
        assert!(std::ptr::eq(rows.as_ptr(), fal.models().as_ptr()));
        // With nothing declared the Rust table itself is returned.
        let none = declared_fal(
            &base_unused(),
            vec![testing::sample("wavespeed", "vendor/not-fal")],
        );
        assert!(std::ptr::eq(none.models().as_ptr(), MODELS.as_ptr()));
    }

    #[test]
    fn declared_entry_cannot_change_a_rust_table_model() {
        // The catalogue holds an entry with the id of a built-in model; the adapter ignores it.
        let (base, seen) = mock(|_, _, _, _| json_resp(json!({"request_id": "r"})));
        let mut shadow = testing::sample("fal", IMAGE_MODEL);
        shadow["kind"] = json!("image");
        let fal = declared_fal(&base, vec![shadow, testing::sample("fal", DECLARED)]);
        let img = req(
            Kind::Image,
            IMAGE_MODEL,
            json!({"aspect": "16:9", "num_images": 1}),
        );
        fal.submit(&img, KEY).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].path, "/fal-ai/flux-2/klein/9b");
        assert_eq!(
            serde_json::from_str::<Value>(&seen[0].body).unwrap(),
            json!({"prompt": "a red fox", "num_images": 1, "image_size": "landscape_16_9", "output_format": "png"})
        );
        assert!(
            (fal.estimate_usd(&req(Kind::Image, IMAGE_MODEL, json!({"num_images": 2})))
                .unwrap()
                - 0.0126)
                .abs()
                < 1e-9
        );
        assert!(
            (fal.estimate_usd(&req(Kind::Video, VIDEO_T2V, json!({})))
                .unwrap()
                - 0.40)
                .abs()
                < 1e-9
        );
        assert!(
            (fal.estimate_usd(&req(
                Kind::Video,
                VIDEO_T2V,
                json!({"duration": "8s", "generate_audio": true})
            ))
            .unwrap()
                - 1.20)
                .abs()
                < 1e-9
        );
        assert_eq!(
            fal.estimate_usd(&req(Kind::Image, "nope", json!({})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn declared_model_with_a_stale_price_is_refused_with_price_stale() {
        let (base, seen) = mock(|_, _, _, _| (500, "text/plain", b"must not be called".to_vec()));
        let mut old = testing::sample("fal", DECLARED);
        old["priced_at"] = json!(testing::days_ago(61));
        let fal = declared_fal(&base, vec![old]);
        let r = declared_req(declared_params());
        for e in [
            fal.estimate_usd(&r).unwrap_err(),
            fal.submit(&r, KEY).unwrap_err(),
        ] {
            assert_eq!(e.code, "price_stale");
            assert!(e.message.contains(DECLARED), "{}", e.message);
        }
        assert!(seen.lock().unwrap().is_empty());
        // Listed, not removed.
        assert_eq!(fal.models().len(), MODELS.len() + 1);
    }
}
