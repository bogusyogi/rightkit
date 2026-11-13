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
                    body["aspect_ratio"] = json!(video_aspect(ar, "auto"));
                } else {
                    body["aspect_ratio"] = json!(video_aspect(ar, "16:9"));
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
            body["aspect_ratio"] = json!(video_aspect(ar, "auto"));
        }
        Mode::Flf => {
            body["first_frame_url"] = json!(image_url(req).unwrap_or_default());
            body["last_frame_url"] = json!(end_url(req).unwrap_or_default());
            body["aspect_ratio"] = json!(video_aspect(ar, "auto"));
        }
        Mode::Reference => {
            body["image_urls"] = json!(ref_urls(req));
            body["aspect_ratio"] = json!(video_aspect(ar, "16:9"));
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

/// The `aspect_ratio` sent for a video: only `16:9` and `9:16` pass through; anything else
/// (including a value fal would refuse) becomes `default`.
fn video_aspect<'a>(ar: Option<&'a str>, default: &'a str) -> &'a str {
    ar.filter(|a| matches!(*a, "16:9" | "9:16"))
        .unwrap_or(default)
}
