//! EvoLink adapter (image): GPT Image 2 and Midjourney v7.
//!
//! Sources (checked 2026-10-06):
//! - Reference JS (read-only): `tools/backends/evolink-image.js`.
//! - `POST {base}/v1/images/generations` with `Authorization: Bearer <key>`
//!   returns `{id, status, ...}`; `GET {base}/v1/tasks/{id}` returns
//!   `{status: pending|processing|completed|failed, results: [url],
//!   error: {message}}`.
//! - GPT Image 2 (`gpt-image-2`): `prompt`, `size` (ratio such as 1:1, 16:9,
//!   2:3, 9:16, 3:2, 4:5, 21:9), `quality` low|medium|high (default medium),
//!   `resolution` 1K|2K|4K, `n`, `image_urls` (<= 16). Per-image estimates at
//!   1K: low $0.015, medium $0.022 (doc-asserted), high $0.14
//!   (invoice-verified 2026-05-07); 2K x1.6 and 4K x2.5 are doc-asserted.
//! - Midjourney v7 (`mj-v7`):
//!   <https://evolink.ai/docs/en/api-manual/image-series/midjourney/mj-v7-image-generate>
//!   takes `prompt` (<= 8192 chars, native syntax such as `--ar 16:9`; reference
//!   image URLs go at the start of the prompt) and
//!   `model_params.speed` draft|fast|turbo (default fast). One call returns
//!   four images. Per call: draft $0.040, fast $0.080, turbo $0.159
//!   (<https://evolink.ai/midjourney-v7>).
//! - Free key check: `GET {base}/v1/credits`.
//!
//! The API key is sent only to the configured EvoLink host; result URLs are
//! fetched without credentials.
use super::{
    request_may_have_reached, status_may_have_run, unknown_outcome, Durations, GenRequest, Kind,
    ModelInfo, Output, PollOutcome, Provider, ProviderError, ReferenceInput, SubmitOutcome,
};
use crate::capabilities::{self, Capabilities};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.evolink.ai";

const GPT: &str = "gpt-image-2";
const MJ: &str = "mj-v7";

static MODELS: [ModelInfo; 2] = [
    ModelInfo {
        provider: "evolink",
        model: GPT,
        kind: Kind::Image,
        label: "GPT Image 2 (text rendering; medium ~$0.022, high ~$0.14 at 1K)",
        est_usd: 0.022,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "evolink",
        model: MJ,
        kind: Kind::Image,
        label: "Midjourney v7 (4 images per call; fast $0.08, turbo $0.159, draft $0.04)",
        est_usd: 0.08,
        durations: Durations::NONE,
    },
];

pub struct Evolink {
    base: String,
    agent: Http,
}

impl Evolink {
    pub fn new(base: &str) -> Self {
        let agent = Http::new(Duration::from_secs(10), Duration::from_secs(300));
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent,
        }
    }

    fn get(&self, url: &str, key: Option<&str>) -> Result<Resp, ProviderError> {
        let mut req = Request::get(url).max_response_bytes(BODY_LIMIT);
        if let Some(k) = key {
            req = req.secret_header("Authorization", format!("Bearer {k}"));
        }
        read(self.agent.send(req), key.unwrap_or(""))
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

fn quality(req: &GenRequest) -> Result<&'static str, ProviderError> {
    match str_param(req, "quality") {
        None | Some("medium") => Ok("medium"),
        Some("low") => Ok("low"),
        Some("high") => Ok("high"),
        Some(o) => Err(bad(format!(
            "gpt-image-2 quality must be low, medium or high, got {o}"
        ))),
    }
}

fn gpt_resolution(req: &GenRequest) -> Result<(&'static str, f64), ProviderError> {
    match str_param(req, "resolution") {
        None | Some("1K") => Ok(("1K", 1.0)),
        Some("2K") => Ok(("2K", 1.6)),
        Some("4K") => Ok(("4K", 2.5)),
        Some(o) => Err(bad(format!(
            "gpt-image-2 resolution must be 1K, 2K or 4K, got {o}"
        ))),
    }
}

fn speed(req: &GenRequest) -> Result<(&'static str, f64), ProviderError> {
    match str_param(req, "speed") {
        None | Some("fast") => Ok(("fast", 0.08)),
        Some("draft") => Ok(("draft", 0.04)),
        Some("turbo") => Ok(("turbo", 0.159)),
        Some(o) => Err(bad(format!(
            "mj-v7 speed must be draft, fast or turbo, got {o}"
        ))),
    }
}

fn urls(req: &GenRequest, k: &str) -> Result<Vec<String>, ProviderError> {
    let Some(v) = req.params.get(k).filter(|v| !v.is_null()) else {
        return Ok(vec![]);
    };
    let a = v
        .as_array()
        .ok_or_else(|| bad(format!("params.{k} must be an array of https URLs")))?;
    a.iter()
        .map(|u| {
            u.as_str()
                .filter(|s| s.starts_with("https://"))
                .map(String::from)
                .ok_or_else(|| bad(format!("params.{k} entries must be https URLs")))
        })
        .collect()
}

fn aspect(req: &GenRequest) -> Option<&str> {
    str_param(req, "aspect")
        .or_else(|| str_param(req, "aspect_ratio"))
        .or_else(|| str_param(req, "size"))
}

fn ratio(a: &str) -> &str {
    match a {
        "square" => "1:1",
        "portrait" => "2:3",
        "landscape" => "3:2",
        o => o,
    }
}

fn valid_ratio(a: &str) -> bool {
    let mut it = a.split(':');
    matches!((it.next(), it.next(), it.next()), (Some(w), Some(h), None) if w.parse::<u32>().is_ok_and(|n| n > 0) && h.parse::<u32>().is_ok_and(|n| n > 0))
}

/// Validates the request and returns the model id.
fn resolve(req: &GenRequest) -> Result<&'static str, ProviderError> {
    let info = MODELS
        .iter()
        .find(|m| m.model == req.model)
        .ok_or_else(|| bad(format!("unknown evolink model {}", req.model)))?;
    if req.kind != Kind::Image {
        return Err(bad("model does not match request kind"));
    }
    if req.prompt.trim().is_empty() {
        return Err(bad("prompt is required"));
    }
    if info.model == GPT {
        quality(req)?;
        gpt_resolution(req)?;
        if urls(req, "image_urls")?.len() > 16 {
            return Err(bad("gpt-image-2 accepts at most 16 image_urls"));
        }
        if let Some(a) = aspect(req) {
            if !valid_ratio(ratio(a)) && !a.contains('x') {
                return Err(bad(format!("unsupported gpt-image-2 size {a}")));
            }
        }
        if req
            .params
            .get("n")
            .and_then(Value::as_u64)
            .is_some_and(|n| n != 1)
        {
            return Err(bad(
                "evolink image calls produce one billed image; send n = 1",
            ));
        }
    } else {
        speed(req)?;
        if req.prompt.chars().count() > 8192 {
            return Err(bad("mj-v7 prompt is limited to 8192 characters"));
        }
        if let Some(a) = aspect(req) {
            if !valid_ratio(ratio(a)) {
                return Err(bad(format!("mj-v7 aspect must look like 16:9, got {a}")));
            }
        }
        urls(req, "image_urls")?;
    }
    Ok(info.model)
}

fn build_body(model: &str, req: &GenRequest) -> Value {
    if model == GPT {
        let mut b = json!({
            "model": GPT,
            "prompt": req.prompt,
            "size": ratio(aspect(req).unwrap_or("1:1")),
            "quality": quality(req).unwrap_or("medium"),
            "resolution": gpt_resolution(req).map(|r| r.0).unwrap_or("1K"),
            "n": 1,
        });
        let refs = urls(req, "image_urls").unwrap_or_default();
        if !refs.is_empty() {
            b["image_urls"] = json!(refs);
        }
        return b;
    }
    // Midjourney takes reference URLs at the start of the prompt and the
    // aspect as native `--ar` syntax (appended unless the prompt sets one).
    let mut prompt = String::new();
    for u in urls(req, "image_urls").unwrap_or_default() {
        prompt.push_str(&u);
        prompt.push(' ');
    }
    prompt.push_str(req.prompt.trim());
    if let Some(a) = aspect(req) {
        if !req.prompt.contains("--ar") && !req.prompt.contains("--aspect") {
            prompt.push_str(&format!(" --ar {}", ratio(a)));
        }
    }
    json!({
        "model": MJ,
        "prompt": prompt,
        "model_params": { "speed": speed(req).map(|s| s.0).unwrap_or("fast") },
    })
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
            redact(&format!("evolink read error: {e}"), key),
            http::retryable(&e),
        ),
        _ => ProviderError::new(
            "transport",
            redact(&format!("evolink transport error: {e}"), key),
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
            v.get("error")
                .or_else(|| v.get("message"))
                .or_else(|| v.get("detail"))
        })
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
    let msg: String = msg.chars().take(300).collect();
    redact(&msg, key)
}

fn map_status(status: u16, text: String) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let balance = ["insufficient", "quota", "balance", "credits"]
        .iter()
        .any(|n| lower.contains(n));
    match status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("evolink rejected the API key ({status}): {text}"),
            false,
        ),
        402 => ProviderError::new("billing", format!("evolink billing problem: {text}"), false),
        s if (400..500).contains(&s) && s != 408 && s != 429 && balance => {
            ProviderError::new("billing", format!("evolink billing problem: {text}"), false)
        }
        408 | 429 => ProviderError::new("rate_limited", format!("evolink {status}: {text}"), true),
        500..=599 => ProviderError::new("unavailable", format!("evolink {status}: {text}"), true),
        s => ProviderError::new("invalid_request", format!("evolink {s}: {text}"), false),
    }
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Provider for Evolink {
    fn id(&self) -> &'static str {
        "evolink"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    /// `params.image_urls`, https URLs only (`urls` refuses anything else; no local upload).
    /// gpt-image-2 caps the list at 16 (checked in `resolve`); mj-v7 checks no count.
    fn references(&self, model: &str) -> Option<ReferenceInput> {
        match model {
            GPT => Some(ReferenceInput {
                param: "image_urls",
                max: Some(16),
                local_upload: "none",
            }),
            MJ => Some(ReferenceInput {
                param: "image_urls",
                max: None,
                local_upload: "none",
            }),
            _ => None,
        }
    }

    /// `aspect`: `resolve` accepts any `W:H` ratio (`valid_ratio`), so the list is open and holds the ratios the
    /// app offers. `resolution` is checked only for gpt-image-2 (`gpt_resolution`, default 1K). No seed is sent.
    fn capabilities(&self, model: &str) -> Capabilities {
        let c = Capabilities::default().mode("prompt").setting(
            "aspect",
            capabilities::aspect("aspect", &["1:1", "16:9", "9:16", "4:3"], true),
        );
        if model == GPT {
            c.setting(
                "resolution",
                capabilities::resolution("resolution", &["1K", "2K", "4K"], false, Some("1K")),
            )
        } else {
            c
        }
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        if resolve(req)? == GPT {
            let base = match quality(req)? {
                "low" => 0.015,
                "high" => 0.14,
                _ => 0.022,
            };
            Ok(base * gpt_resolution(req)?.1)
        } else {
            Ok(speed(req)?.1)
        }
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        let model = resolve(req)?;
        let body = build_body(model, req);
        let req = Request::post(format!("{}/v1/images/generations", self.base))
            .secret_header("Authorization", format!("Bearer {api_key}"))
            .max_response_bytes(BODY_LIMIT)
            .json(&body);
        let sent = req.and_then(|req| self.agent.send(req));
        let reached = request_may_have_reached(&sent);
        let r = read(sent, api_key).map_err(|e| e.ambiguous_if(reached))?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key))
                .ambiguous_if(status_may_have_run(r.status)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "evolink submit reply was not JSON", true)
                .ambiguous()
        })?;
        let id = v
            .get("id")
            .or_else(|| v.get("data").and_then(|d| d.get("id")))
            .and_then(Value::as_str)
            .filter(|i| safe_id(i))
            .ok_or_else(|| {
                ProviderError::new(
                    "bad_response",
                    "evolink submit reply had no usable id",
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
            return Err(bad("invalid evolink task id"));
        }
        let r = self.get(
            &format!("{}/v1/tasks/{}", self.base, provider_job_id),
            Some(api_key),
        )?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new("bad_response", "evolink task reply was not JSON", true)
        })?;
        match v.get("status").and_then(Value::as_str).unwrap_or("") {
            "pending" | "processing" | "queued" | "running" => Ok(PollOutcome::Running),
            "completed" => {
                let outputs: Vec<Output> = v
                    .get("results")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|u| Output::Url {
                                url: u.to_string(),
                                mime: super::fal::mime_from_url(u)
                                    .or_else(|| Some("image/png".into())),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if outputs.is_empty() {
                    Ok(unknown_outcome(
                        "completed, no usable images",
                        &r.body,
                        api_key,
                        true,
                    ))
                } else {
                    Ok(PollOutcome::Completed { outputs })
                }
            }
            // Confirmed-failure vocabulary (closed list): "failed", "cancelled", "canceled".
            // Every other status string is `Unknown` and held.
            "failed" | "cancelled" | "canceled" => {
                let m = v
                    .get("error")
                    .map(|e| {
                        e.get("message")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| e.to_string())
                    })
                    .unwrap_or_else(|| "unknown error".into());
                Ok(PollOutcome::Failed {
                    message: redact(&format!("evolink task failed: {m}"), api_key),
                })
            }
            "" => Err(ProviderError::new(
                "bad_response",
                "evolink task reply had no status",
                true,
            )),
            other => Ok(unknown_outcome(other, &r.body, api_key, false)),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /v1/credits is free and requires a valid key.
        let r = self.get(&format!("{}/v1/credits", self.base), Some(api_key))?;
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
                // No credentials: result URLs are public links.
                let r = self.get(url, None)?;
                if !(200..300).contains(&r.status) {
                    return Err(map_status(r.status, error_text(&r.body, "")));
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

    fn req(model: &str, params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::Image,
            provider: "evolink".into(),
            model: model.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    fn json_resp(v: Value) -> (u16, &'static str, Vec<u8>) {
        (200, "application/json", v.to_string().into_bytes())
    }

    fn body_of(seen: &Arc<Mutex<Vec<Seen>>>, i: usize) -> Value {
        serde_json::from_str(&seen.lock().unwrap()[i].body).unwrap()
    }

    const KEY: &str = concat!("ev", "o-secret-KEY-123");

    #[test]
    fn registered_with_models_and_env_var() {
        let all = super::super::registry();
        let p = all.iter().find(|p| p.id() == "evolink").unwrap();
        let ids: Vec<_> = p.models().iter().map(|m| m.model).collect();
        assert_eq!(ids, ["gpt-image-2", "mj-v7"]);
        assert!(crate::credentials::ENV_VARS
            .iter()
            .any(|(id, v)| *id == "evolink" && v.contains(&"EVOLINK_API_KEY")));
    }

    #[test]
    fn gpt_image_2_body_price_and_flow() {
        let polls = Arc::new(Mutex::new(0));
        let p2 = polls.clone();
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/v1/images/generations") => {
                json_resp(json!({"id": "task-1", "status": "pending"}))
            }
            ("GET", "/v1/tasks/task-1") => {
                let mut n = p2.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    json_resp(json!({"status": "processing"}))
                } else {
                    json_resp(
                        json!({"status": "completed", "results": [format!("{base}/cdn/a.png")]}),
                    )
                }
            }
            ("GET", "/cdn/a.png") => (200, "image/png", vec![1, 2, 3]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ev = Evolink::new(&base);
        let r = req(
            "gpt-image-2",
            json!({"aspect": "2:3", "quality": "high", "resolution": "2K", "image_urls": ["https://x/r.png"]}),
        );
        assert!((ev.estimate_usd(&r).unwrap() - 0.14 * 1.6).abs() < 1e-9);
        assert!((ev.estimate_usd(&req("gpt-image-2", json!({}))).unwrap() - 0.022).abs() < 1e-9);
        let SubmitOutcome::Accepted { provider_job_id } = ev.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            ev.poll(&r, &provider_job_id, KEY).unwrap(),
            PollOutcome::Running
        );
        let PollOutcome::Completed { outputs } = ev.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            ev.fetch(&outputs[0]).unwrap(),
            (vec![1, 2, 3], "image/png".to_string())
        );
        let b = body_of(&seen, 0);
        assert_eq!(
            (
                b["model"].as_str(),
                b["size"].as_str(),
                b["quality"].as_str(),
                b["resolution"].as_str(),
                b["n"].as_u64(),
                b["image_urls"].clone()
            ),
            (
                Some("gpt-image-2"),
                Some("2:3"),
                Some("high"),
                Some("2K"),
                Some(1),
                json!(["https://x/r.png"])
            )
        );
        let s = seen.lock().unwrap();
        assert!(s[0]
            .headers
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
        assert!(!s
            .iter()
            .find(|x| x.path == "/cdn/a.png")
            .unwrap()
            .headers
            .contains(KEY));
    }

    #[test]
    fn mj_v7_body_speed_prices_and_four_outputs() {
        let (base, seen) = mock(|base, m, path, _| match (m, path) {
            ("POST", "/v1/images/generations") => {
                json_resp(json!({"id": "mj-1", "status": "pending"}))
            }
            ("GET", "/v1/tasks/mj-1") => json_resp(
                json!({"status": "completed", "results": (0..4).map(|i| format!("{base}/cdn/{i}.png")).collect::<Vec<_>>()}),
            ),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ev = Evolink::new(&base);
        for (sp, usd) in [("draft", 0.04), ("fast", 0.08), ("turbo", 0.159)] {
            let r = req("mj-v7", json!({"speed": sp}));
            assert!((ev.estimate_usd(&r).unwrap() - usd).abs() < 1e-9);
        }
        assert!((ev.estimate_usd(&req("mj-v7", json!({}))).unwrap() - 0.08).abs() < 1e-9);
        let r = req(
            "mj-v7",
            json!({"speed": "turbo", "aspect": "16:9", "image_urls": ["https://x/ref.png"]}),
        );
        let SubmitOutcome::Accepted { provider_job_id } = ev.submit(&r, KEY).unwrap() else {
            panic!()
        };
        let b = body_of(&seen, 0);
        assert_eq!(b["model"], "mj-v7");
        assert_eq!(b["prompt"], "https://x/ref.png a red fox --ar 16:9");
        assert_eq!(b["model_params"]["speed"], "turbo");
        let PollOutcome::Completed { outputs } = ev.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(outputs.len(), 4);
        // A prompt that already carries --ar is left alone.
        let mut r2 = req("mj-v7", json!({"aspect": "1:1"}));
        r2.prompt = "fox --ar 3:2".into();
        ev.submit(&r2, KEY).unwrap();
        assert_eq!(body_of(&seen, 2)["prompt"], "fox --ar 3:2");
    }

    #[test]
    fn invalid_requests_never_reach_network() {
        let (base, seen) = mock(|_, _, _, _| json_resp(json!({"id": "x"})));
        let ev = Evolink::new(&base);
        for (m, p) in [
            ("gpt-image-2", json!({"quality": "ultra"})),
            ("gpt-image-2", json!({"resolution": "8K"})),
            ("gpt-image-2", json!({"n": 2})),
            (
                "gpt-image-2",
                json!({"image_urls": (0..17).map(|_| "https://x/a.png").collect::<Vec<_>>()}),
            ),
            ("mj-v7", json!({"speed": "warp"})),
            ("mj-v7", json!({"aspect": "wide"})),
            ("mj-v7", json!({"image_urls": ["http://x/a.png"]})),
            ("flux", json!({})),
        ] {
            assert!(ev.submit(&req(m, p.clone()), KEY).is_err(), "{m} {p}");
        }
        let mut v = req("mj-v7", json!({}));
        v.kind = Kind::Video;
        assert!(ev.submit(&v, KEY).is_err());
        assert!(seen.lock().unwrap().is_empty());
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
        for status in [400, 401, 402, 403, 422] {
            let (base, _) = mock(move |_, _, _, _| (status, "application/json", b"{}".to_vec()));
            let e = Evolink::new(&base)
                .submit(&req("mj-v7", json!({})), KEY)
                .unwrap_err();
            assert!(!e.ambiguous && !e.retryable, "status {status}: {e:?}");
        }
        for status in [408, 429, 500, 503] {
            let (base, _) = mock(move |_, _, _, _| (status, "application/json", b"{}".to_vec()));
            let e = Evolink::new(&base)
                .submit(&req("mj-v7", json!({})), KEY)
                .unwrap_err();
            assert!(e.ambiguous && e.retryable, "status {status}: {e:?}");
        }
        let (base, _) = mock(|_, _, _, _| (200, "text/plain", b"not json".to_vec()));
        let e = Evolink::new(&base)
            .submit(&req("mj-v7", json!({})), KEY)
            .unwrap_err();
        assert!(e.ambiguous && e.code == "bad_response", "{e:?}");
        let (base, _) = mock(|_, _, _, _| json_resp(json!({"id": "bad id!"})));
        let e = Evolink::new(&base)
            .submit(&req("mj-v7", json!({})), KEY)
            .unwrap_err();
        assert!(e.ambiguous && e.code == "bad_response", "{e:?}");
        let base = hang_up_after_request();
        let e = Evolink::new(&base)
            .submit(&req("mj-v7", json!({})), KEY)
            .unwrap_err();
        assert!(e.ambiguous && e.retryable && e.code == "transport", "{e:?}");
        let e = Evolink::new("http://127.0.0.1:1")
            .submit(&req("mj-v7", json!({})), KEY)
            .unwrap_err();
        assert!(
            !e.ambiguous && e.retryable && e.code == "transport",
            "{e:?}"
        );
        let e = Evolink::new("http://127.0.0.1:1")
            .submit(&req("flux", json!({})), KEY)
            .unwrap_err();
        assert!(!e.ambiguous && !e.retryable);
    }
    #[test]
    fn errors_map_and_redact_the_key() {
        let (base, _) = mock(|_, m, path, _| match (m, path) {
            ("POST", _) => (
                401,
                "application/json",
                format!("{{\"error\":{{\"message\":\"bad key {KEY}\"}}}}").into_bytes(),
            ),
            ("GET", "/v1/tasks/t-fail") => {
                json_resp(json!({"status": "failed", "error": {"message": "content blocked"}}))
            }
            ("GET", "/v1/tasks/t-402") => (
                402,
                "application/json",
                b"{\"error\":\"insufficient credits\"}".to_vec(),
            ),
            ("GET", "/v1/tasks/t-503") => (503, "text/plain", b"down".to_vec()),
            ("GET", "/v1/credits") => json_resp(json!({"success": true})),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let ev = Evolink::new(&base);
        let r = req("mj-v7", json!({}));
        let e = ev.submit(&r, KEY).unwrap_err();
        assert_eq!((e.code.as_str(), e.retryable), ("auth", false));
        assert!(!e.message.contains(KEY));
        assert_eq!(
            ev.poll(&r, "t-fail", KEY).unwrap(),
            PollOutcome::Failed {
                message: "evolink task failed: content blocked".into()
            }
        );
        assert_eq!(ev.poll(&r, "t-402", KEY).unwrap_err().code, "billing");
        let e = ev.poll(&r, "t-503", KEY).unwrap_err();
        assert_eq!((e.code.as_str(), e.retryable), ("unavailable", true));
        assert!(ev.poll(&r, "../etc", KEY).is_err());
        assert!(ev.test_key(KEY).is_ok());
    }
}
