//! Higgsfield developer API adapter (direct, prepaid): Genjutsu and Kling 3.0.
//!
//! Sources (fetched 2026-10-17, <https://docs.higgsfield.ai>):
//! - Base `https://api.higgsfield.ai`; header `Authorization: Key <id>:<secret>`; submit is
//!   `POST /<endpoint-id>` and returns `{status:"queued", request_id, status_url, cancel_url}`
//!   (`/docs/concepts/requests`, `/docs/how-to/introduction`). Submissions accept an optional
//!   `Idempotency-Key` header (`/docs/api-reference/overview`).
//! - Status: `GET /requests/{id}/status`; statuses queued, in_progress, completed, failed, nsfw, canceled;
//!   completed video output is `video.url`, failed may carry `error` (restyle page and requests page).
//! - Estimate: `POST /estimate/<endpoint-id>` with the SAME body as the generation returns
//!   `{"credits":"1.500","usd":"0.094"}` (`/docs/concepts/billing-and-retention.md`; the page shows the soul/v2
//!   endpoint; applying it to every endpoint is UNVERIFIED). Failed and NSFW requests are not charged (FAQ,
//!   `/docs/help/faq`). Canceled being uncharged is UNVERIFIED (never started per the requests page).
//! - Genjutsu endpoints (`/docs/models/genjutsu`, `.../object-swap`, `.../restyle`):
//!   `higgsfield/genjutsu/{motion-transfer|object-swap|restyle}/v1.0`. Motion transfer takes `video_url` and
//!   `image_urls` (only seen in the quick-start; its own page was not fetched, so `resolution` is NOT sent: UNVERIFIED).
//!   Object swap: `video_url`, `image_urls` (1-8), `prompt`, `resolution` 480p|720p|1080p. Restyle: `video_url`,
//!   `preset_id` (uuid from the presets catalogue), `image_urls` (max 5), `prompt`, `resolution`.
//! - Kling 3.0 (`/docs/models/kling-3/...`): `kling-video/v3.0/std/text-to-video` (prompt, sound on|off, duration 3-15,
//!   cfg_scale 0-1, aspect_ratio 16:9|9:16|1:1) and `kling-video/v3.0/std/image-to-video` (`image_url`, optional
//!   `last_image_url`); `kling-video/v3.0-turbo/image-to-video` (duration 3-15, resolution 720p|1080p, no sound).
//!   The pro paths `kling-video/v3.0/pro/{text,image}-to-video` are INFERRED from the std paths and the existence of
//!   the pro text-to-video page: UNVERIFIED. Image-to-video sends no aspect_ratio (not documented there).
//! - NOT covered: file upload (`/docs/concepts/file-uploads`; only public http(s) URLs are accepted), Kling elements and
//!   multi-shot, Kling Omni, webhooks. No free key-check endpoint is documented: `test_key` reads the restyle presets
//!   catalogue (documented, authenticated, free): UNVERIFIED as a billing-free call.
//!
//! Credential: one string `<key id>:<key secret>` under provider `higgsfield` (env `HIGGSFIELD_API_KEY`). Absent key =
//! provider not configured.
//!
//! Pricing: `estimate_usd` has no key parameter in the `Provider` trait, so it reads a positive `params.estimate_usd`
//! (stamped by the caller from [`Higgsfield::quote_usd`]) or else asks the estimate endpoint with the key found in the
//! process environment. With neither it returns `price_unknown`; nothing is hard-coded.
use super::{
    request_may_have_reached, status_may_have_run, unknown_outcome, Durations, GenRequest, Kind,
    ModelInfo, Output, PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Map, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.higgsfield.ai";
const BODY_LIMIT: usize = 1 << 30;
/// One request may not reserve more than this; guards a mistyped caller estimate.
const MAX_ESTIMATE_USD: f64 = 200.0;

pub const MOTION: &str = "genjutsu-motion-transfer";
pub const SWAP: &str = "genjutsu-object-swap";
pub const RESTYLE: &str = "genjutsu-restyle";
pub const KLING_STD_T2V: &str = "kling-3-std-text-to-video";
pub const KLING_STD_I2V: &str = "kling-3-std-image-to-video";
pub const KLING_PRO_T2V: &str = "kling-3-pro-text-to-video";
pub const KLING_PRO_I2V: &str = "kling-3-pro-image-to-video";
pub const KLING_TURBO_I2V: &str = "kling-3-turbo-image-to-video";

const KLING_DURATIONS: Durations = Durations::range(3, 15, Some(5));

macro_rules! row {
    ($model:expr, $kind:expr, $label:expr, $dur:expr) => {
        ModelInfo {
            provider: "higgsfield",
            model: $model,
            kind: $kind,
            label: $label,
            est_usd: 0.0,
            durations: $dur,
        }
    };
}

static MODELS: [ModelInfo; 8] = [
    row!(MOTION, Kind::MotionControl, "Higgsfield Genjutsu motion transfer (params.video_url + params.image_urls; priced by estimate endpoint at run)", Durations::NONE),
    row!(SWAP, Kind::ReplaceObject, "Higgsfield Genjutsu object swap (params.video_url + params.image_urls; priced by estimate endpoint at run)", Durations::NONE),
    row!(RESTYLE, Kind::Restyle, "Higgsfield Genjutsu restyle (params.video_url + params.preset_id; priced by estimate endpoint at run)", Durations::NONE),
    row!(KLING_STD_T2V, Kind::Video, "Higgsfield Kling 3.0 Standard text-to-video (priced by estimate endpoint at run)", KLING_DURATIONS),
    row!(KLING_STD_I2V, Kind::Video, "Higgsfield Kling 3.0 Standard image-to-video (params.image_url; priced by estimate endpoint at run)", KLING_DURATIONS),
    row!(KLING_PRO_T2V, Kind::Video, "Higgsfield Kling 3.0 Pro text-to-video (endpoint path inferred; priced by estimate endpoint at run)", KLING_DURATIONS),
    row!(KLING_PRO_I2V, Kind::Video, "Higgsfield Kling 3.0 Pro image-to-video (endpoint path inferred; priced by estimate endpoint at run)", KLING_DURATIONS),
    row!(KLING_TURBO_I2V, Kind::Video, "Higgsfield Kling 3.0 Turbo image-to-video (params.image_url; priced by estimate endpoint at run)", KLING_DURATIONS),
];

pub struct Higgsfield {
    base: String,
    agent: Http,
}

impl Higgsfield {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent: Http::new(Duration::from_secs(10), Duration::from_secs(300)),
        }
    }

    /// Ask `POST /estimate/<endpoint>` for this request's price in USD.
    pub fn quote_usd(&self, req: &GenRequest, api_key: &str) -> Result<f64, ProviderError> {
        let (endpoint, body) = plan(req)?;
        check_key(api_key)?;
        let r = self.post(
            &format!("{}/estimate/{endpoint}", self.base),
            &body,
            api_key,
            None,
        )??;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new(
                "bad_response",
                "higgsfield estimate reply was not JSON",
                true,
            )
        })?;
        let usd = match v.get("usd") {
            Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
            Some(n) => n.as_f64(),
            None => None,
        };
        match usd {
            Some(u) if u.is_finite() && u > 0.0 && u <= MAX_ESTIMATE_USD => Ok(u),
            _ => Err(ProviderError::new(
                "price_unknown",
                "higgsfield estimate reply had no usable usd price",
                false,
            )),
        }
    }

    /// POST JSON; the outer Result is the transport result (already redacted), the inner the reply.
    fn post(
        &self,
        url: &str,
        body: &Value,
        key: &str,
        idempotency: Option<&str>,
    ) -> Result<Result<Response, ProviderError>, ProviderError> {
        let mut post = Request::post(url)
            .secret_header("Authorization", format!("Key {key}"))
            .max_response_bytes(BODY_LIMIT);
        if let Some(k) = idempotency {
            post = post.header("Idempotency-Key", k);
        }
        let sent = post.json(body).and_then(|r| self.agent.send(r));
        let reached = request_may_have_reached(&sent);
        Ok(read(sent, key).map_err(|e| e.ambiguous_if(reached)))
    }

    fn get(&self, url: &str, key: Option<&str>) -> Result<Response, ProviderError> {
        let mut req = Request::get(url).max_response_bytes(BODY_LIMIT);
        if let Some(k) = key {
            req = req.secret_header("Authorization", format!("Key {k}"));
        }
        read(self.agent.send(req), key.unwrap_or(""))
    }

    fn create(
        &self,
        req: &GenRequest,
        api_key: &str,
        idempotency: Option<&str>,
    ) -> Result<SubmitOutcome, ProviderError> {
        let (endpoint, body) = plan(req)?;
        check_key(api_key)?;
        let r = self.post(
            &format!("{}/{endpoint}", self.base),
            &body,
            api_key,
            idempotency,
        )??;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key))
                .ambiguous_if(status_may_have_run(r.status)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "higgsfield submit reply was not JSON", true)
                .ambiguous()
        })?;
        let id = v
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|i| safe_id(i))
            .ok_or_else(|| {
                ProviderError::new(
                    "bad_response",
                    "higgsfield submit reply had no usable request_id",
                    true,
                )
                .ambiguous()
            })?;
        Ok(SubmitOutcome::Accepted {
            provider_job_id: id.to_string(),
        })
    }
}

fn bad(msg: impl Into<String>) -> ProviderError {
    ProviderError::new("invalid_request", msg, false)
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn check_key(key: &str) -> Result<(), ProviderError> {
    match key.split_once(':') {
        Some((id, secret)) if !id.is_empty() && !secret.is_empty() => Ok(()),
        _ => Err(ProviderError::new(
            "auth",
            "higgsfield credential must be \"<key id>:<key secret>\"",
            false,
        )),
    }
}

fn redact(s: &str, key: &str) -> String {
    // The whole `id:secret` key and its secret half are both removed (`rightkit_logs::KnownSecrets`).
    let secret = key.split_once(':').map(|(_, secret)| secret).unwrap_or("");
    rightkit_logs::KnownSecrets::from_values([key, secret]).redact(s)
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
        .map(|e| {
            e.as_str()
                .map(String::from)
                .unwrap_or_else(|| e.to_string())
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    redact(&msg.chars().take(300).collect::<String>(), key)
}

fn map_status(status: u16, text: String) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let balance = ["insufficient", "balance", "credits", "top up", "top-up"]
        .iter()
        .any(|n| lower.contains(n));
    match status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("higgsfield rejected the API key ({status}): {text}"),
            false,
        ),
        402 => ProviderError::new(
            "billing",
            format!("higgsfield balance problem: {text}"),
            false,
        ),
        s if balance && !(500..=599).contains(&s) => ProviderError::new(
            "billing",
            format!("higgsfield balance problem: {text}"),
            false,
        ),
        408 | 429 => {
            ProviderError::new("rate_limited", format!("higgsfield {status}: {text}"), true)
        }
        500..=599 => {
            ProviderError::new("unavailable", format!("higgsfield {status}: {text}"), true)
        }
        s => ProviderError::new("invalid_request", format!("higgsfield {s}: {text}"), false),
    }
}

fn read(r: Result<Response, HttpError>, key: &str) -> Result<Response, ProviderError> {
    r.map_err(|e| {
        ProviderError::new(
            "transport",
            redact(&format!("higgsfield transport error: {e}"), key),
            http::retryable(&e),
        )
    })
}

fn str_param<'a>(req: &'a GenRequest, k: &str) -> Option<&'a str> {
    req.params
        .get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn url_ok(u: &str) -> bool {
    (u.starts_with("https://") || u.starts_with("http://")) && u.len() <= 2083
}

fn url_param(req: &GenRequest, k: &str) -> Result<String, ProviderError> {
    let u = str_param(req, k)
        .ok_or_else(|| bad(format!("params.{k} is required (a public http(s) URL)")))?;
    if !url_ok(u) {
        return Err(bad(format!("params.{k} must be a public http(s) URL (local files are not uploaded by this adapter)")));
    }
    Ok(u.to_string())
}

/// `params.image_urls` (array) or a single `params.image_url`; each a public URL; `min..=max` items.
fn image_urls(req: &GenRequest, min: usize, max: usize) -> Result<Vec<String>, ProviderError> {
    let mut out: Vec<String> = req
        .params
        .get("image_urls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if out.is_empty() {
        if let Some(u) = str_param(req, "image_url") {
            out.push(u.to_string());
        }
    }
    if out.len() < min || out.len() > max {
        return Err(bad(format!(
            "params.image_urls needs {min} to {max} public URLs, got {}",
            out.len()
        )));
    }
    if let Some(u) = out.iter().find(|u| !url_ok(u)) {
        return Err(bad(format!("image URL must be public http(s): {u}")));
    }
    Ok(out)
}

fn resolution(req: &GenRequest, allowed: &[&str]) -> Result<Option<String>, ProviderError> {
    match str_param(req, "resolution") {
        None => Ok(None),
        Some(r) if allowed.contains(&r) => Ok(Some(r.to_string())),
        Some(r) => Err(bad(format!(
            "resolution must be one of {}, got {r}",
            allowed.join(", ")
        ))),
    }
}

fn uuid_ok(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    p.len() == 5
        && p.iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(x, n)| x.len() == n && x.chars().all(|c| c.is_ascii_hexdigit()))
}

fn kling(
    req: &GenRequest,
    endpoint: &'static str,
    i2v: bool,
    turbo: bool,
) -> Result<(&'static str, Value), ProviderError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(bad("prompt is required"));
    }
    if prompt.chars().count() > 2500 {
        return Err(bad("Kling prompts are limited to 2500 characters"));
    }
    let duration = match req.params.get("duration") {
        None => 5,
        Some(v) => v
            .as_u64()
            .or_else(|| {
                v.as_str()
                    .and_then(|s| s.trim_end_matches('s').parse().ok())
            })
            .ok_or_else(|| bad("duration must be a whole number of seconds"))?,
    };
    if !(3..=15).contains(&duration) {
        return Err(bad("Kling 3.0 duration is 3 to 15 seconds"));
    }
    let mut b = Map::new();
    b.insert("prompt".into(), json!(prompt));
    b.insert("duration".into(), json!(duration));
    if i2v {
        b.insert(
            "image_url".into(),
            json!(url_param(req, "image_url")
                .or_else(|_| image_urls(req, 1, 1).map(|v| v[0].clone()))?),
        );
        if let Some(u) =
            str_param(req, "last_image_url").or_else(|| str_param(req, "end_image_url"))
        {
            if turbo {
                return Err(bad("Kling 3.0 Turbo takes no last frame"));
            }
            if !url_ok(u) {
                return Err(bad("last_image_url must be a public http(s) URL"));
            }
            b.insert("last_image_url".into(), json!(u));
        }
    } else {
        let a = str_param(req, "aspect")
            .or_else(|| str_param(req, "aspect_ratio"))
            .unwrap_or("16:9");
        if !["16:9", "9:16", "1:1"].contains(&a) {
            return Err(bad(format!("aspect must be 16:9, 9:16 or 1:1, got {a}")));
        }
        b.insert("aspect_ratio".into(), json!(a));
    }
    if turbo {
        if let Some(r) = resolution(req, &["720p", "1080p"])? {
            b.insert("resolution".into(), json!(r));
        }
    } else {
        match req.params.get("sound") {
            None => {}
            Some(Value::Bool(on)) => {
                b.insert("sound".into(), json!(if *on { "on" } else { "off" }));
            }
            Some(Value::String(s)) if s == "on" || s == "off" => {
                b.insert("sound".into(), json!(s));
            }
            Some(_) => return Err(bad("sound must be on or off")),
        }
        if let Some(c) = req.params.get("cfg_scale") {
            let c = c
                .as_f64()
                .filter(|c| (0.0..=1.0).contains(c))
                .ok_or_else(|| bad("cfg_scale must be a number from 0 to 1"))?;
            b.insert("cfg_scale".into(), json!(c));
        }
    }
    Ok((endpoint, Value::Object(b)))
}

/// Validate a request and build its (endpoint id, body); the body is also the estimate body.
fn plan(req: &GenRequest) -> Result<(&'static str, Value), ProviderError> {
    let info = MODELS
        .iter()
        .find(|m| m.model == req.model)
        .ok_or_else(|| bad(format!("unknown higgsfield model {}", req.model)))?;
    if info.kind != req.kind {
        return Err(bad("model does not match request kind"));
    }
    match req.model.as_str() {
        MOTION => Ok((
            "higgsfield/genjutsu/motion-transfer/v1.0",
            json!({"video_url": url_param(req, "video_url")?, "image_urls": image_urls(req, 1, 8)?}),
        )),
        SWAP => {
            let mut b = json!({"video_url": url_param(req, "video_url")?, "image_urls": image_urls(req, 1, 8)?});
            if !req.prompt.trim().is_empty() {
                b["prompt"] = json!(req.prompt.trim());
            }
            if let Some(r) = resolution(req, &["480p", "720p", "1080p"])? {
                b["resolution"] = json!(r);
            }
            Ok(("higgsfield/genjutsu/object-swap/v1.0", b))
        }
        RESTYLE => {
            let preset = str_param(req, "preset_id").ok_or_else(|| {
                bad("params.preset_id is required (an id from the restyle presets catalogue)")
            })?;
            if !uuid_ok(preset) {
                return Err(bad("params.preset_id must be the preset's uuid"));
            }
            let refs = if req.params.get("image_urls").is_some()
                || req.params.get("image_url").is_some()
            {
                image_urls(req, 1, 5)?
            } else {
                vec![]
            };
            let mut b = json!({"video_url": url_param(req, "video_url")?, "preset_id": preset, "image_urls": refs});
            if !req.prompt.trim().is_empty() {
                b["prompt"] = json!(req.prompt.trim());
            }
            if let Some(r) = resolution(req, &["480p", "720p", "1080p"])? {
                b["resolution"] = json!(r);
            }
            Ok(("higgsfield/genjutsu/restyle/v1.0", b))
        }
        KLING_STD_T2V => kling(req, "kling-video/v3.0/std/text-to-video", false, false),
        KLING_STD_I2V => kling(req, "kling-video/v3.0/std/image-to-video", true, false),
        KLING_PRO_T2V => kling(req, "kling-video/v3.0/pro/text-to-video", false, false),
        KLING_PRO_I2V => kling(req, "kling-video/v3.0/pro/image-to-video", true, false),
        KLING_TURBO_I2V => kling(req, "kling-video/v3.0-turbo/image-to-video", true, true),
        _ => Err(bad("unknown higgsfield model")),
    }
}

impl Provider for Higgsfield {
    fn id(&self) -> &'static str {
        "higgsfield"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        plan(req)?;
        if let Some(u) = req.params.get("estimate_usd").and_then(Value::as_f64) {
            if u.is_finite() && u > 0.0 && u <= MAX_ESTIMATE_USD {
                return Ok(u);
            }
            return Err(bad("params.estimate_usd must be a positive price"));
        }
        match crate::credentials::env_key(&|k| std::env::var(k).ok(), "higgsfield") {
            Some(key) => self.quote_usd(req, &key),
            None => Err(ProviderError::new(
                "price_unknown",
                "higgsfield prices come from its estimate endpoint, which needs the API key",
                false,
            )),
        }
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.create(req, api_key, None)
    }

    /// Sends the attempt key as `Idempotency-Key` (documented as accepted). Whether it prevents a duplicate
    /// charge is not established, so `submit_is_idempotent` stays false.
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
        if !safe_id(id) {
            return Err(bad("invalid higgsfield request id"));
        }
        // The docs advise using the returned status_url; it is `<base>/requests/{id}/status`, rebuilt here from the id.
        let r = self.get(
            &format!("{}/requests/{id}/status", self.base),
            Some(api_key),
        )?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "higgsfield status reply was not JSON", true)
        })?;
        match v.get("status").and_then(Value::as_str).unwrap_or("") {
            "queued" | "in_progress" => Ok(PollOutcome::Running),
            "completed" => {
                let url = v
                    .pointer("/video/url")
                    .or_else(|| v.pointer("/images/0/url"))
                    .and_then(Value::as_str)
                    .filter(|u| u.starts_with("https://") || u.starts_with("http://"));
                match url {
                    Some(u) => Ok(PollOutcome::Completed {
                        outputs: vec![Output::Url {
                            url: u.to_string(),
                            mime: super::fal::mime_from_url(u).or_else(|| Some("video/mp4".into())),
                        }],
                    }),
                    None => Ok(unknown_outcome(
                        "completed, no video url",
                        &r.body,
                        api_key,
                        true,
                    )),
                }
            }
            // Confirmed-failure vocabulary: failed and nsfw are documented as not charged; canceled never started.
            s @ ("failed" | "nsfw" | "canceled") => {
                let m = v
                    .get("error")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or_else(|| s.to_string());
                Ok(PollOutcome::Failed {
                    message: redact(&format!("higgsfield request {s}: {m}"), api_key),
                })
            }
            "" => Err(ProviderError::new(
                "bad_response",
                "higgsfield status reply had no status",
                true,
            )),
            other => Ok(unknown_outcome(other, &r.body, api_key, false)),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        check_key(api_key)?;
        let r = self.get(
            &format!(
                "{}/models/higgsfield/genjutsu/restyle/v1.0/presets",
                self.base
            ),
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
                // No credentials: result files are delivery links.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn req(model: &str, kind: Kind, params: Value) -> GenRequest {
        GenRequest {
            kind,
            provider: "higgsfield".into(),
            model: model.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    #[test]
    fn key_must_be_id_and_secret() {
        assert!(check_key("id:secret").is_ok());
        assert!(check_key("secret-only").is_err());
        assert!(check_key(":x").is_err());
    }

    #[test]
    fn bodies_follow_the_documented_fields() {
        let (ep, b) = plan(&req(SWAP, Kind::ReplaceObject, json!({"video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"], "resolution": "1080p"}))).unwrap();
        assert_eq!(ep, "higgsfield/genjutsu/object-swap/v1.0");
        assert_eq!(
            (b["resolution"].as_str(), b["image_urls"][0].as_str()),
            (Some("1080p"), Some("https://x/a.png"))
        );
        let (ep, b) = plan(&req(
            KLING_TURBO_I2V,
            Kind::Video,
            json!({"image_url": "https://x/a.png", "duration": 8, "resolution": "720p"}),
        ))
        .unwrap();
        assert_eq!(ep, "kling-video/v3.0-turbo/image-to-video");
        assert!(b.get("sound").is_none() && b["duration"] == 8);
    }

    #[test]
    fn invalid_requests_are_refused_before_the_network() {
        let h = Higgsfield::new("http://127.0.0.1:1");
        for r in [
            req(
                RESTYLE,
                Kind::Restyle,
                json!({"video_url": "https://x/v.mp4", "preset_id": "not-a-uuid"}),
            ),
            req(KLING_STD_T2V, Kind::Video, json!({"duration": 30})),
            req(KLING_STD_I2V, Kind::Video, json!({})),
            req(
                MOTION,
                Kind::MotionControl,
                json!({"video_url": "/local/file.mp4", "image_urls": ["https://x/a.png"]}),
            ),
            req(KLING_STD_T2V, Kind::Image, json!({})),
        ] {
            assert!(h.submit(&r, "id:secret").is_err());
        }
    }

    #[test]
    fn estimate_is_never_guessed() {
        let h = Higgsfield::new("http://127.0.0.1:1");
        let r = req(KLING_STD_T2V, Kind::Video, json!({}));
        if std::env::var("HIGGSFIELD_API_KEY").is_err() {
            assert_eq!(h.estimate_usd(&r).unwrap_err().code, "price_unknown");
        }
        let r = req(KLING_STD_T2V, Kind::Video, json!({"estimate_usd": 0.25}));
        assert!((h.estimate_usd(&r).unwrap() - 0.25).abs() < 1e-12);
    }
}
