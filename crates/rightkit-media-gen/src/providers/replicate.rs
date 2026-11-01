//! Replicate adapter (generic predictions): any hosted model, image or video.
//!
//! Sources (fetched 2026-10-17, <https://replicate.com/docs/reference/http>):
//! - `POST /v1/models/{owner}/{name}/predictions` runs an official model; `POST /v1/predictions` with
//!   `version` (64-char id) runs a pinned version. Body `{"input": {...}}`, `Authorization: Bearer <token>`.
//!   Without `Prefer: wait` the reply returns immediately with status `starting`.
//! - Prediction fields: `id`, `status` (starting | processing | succeeded | failed | canceled), `output`
//!   (a URL string or an array of URL strings for file outputs), `error`, `urls.get|cancel`, `metrics`.
//!   `GET /v1/predictions/{id}` polls; `GET /v1/account` is a free token check.
//! - Pricing varies per model (per second of compute or per output) and the HTTP reference lists none, so this
//!   adapter cannot know it: UNVERIFIED by design. The caller MUST pass `params.estimate_usd` (the price taken
//!   from the model's Replicate page) and the adapter refuses a request without it, so a spend can never be
//!   understated. Whether a failed or canceled prediction is billed is not stated in the fetched page; the
//!   adapter treats `failed` and `canceled` as confirmed failures like the other adapters (UNVERIFIED).
//!
//! Request contract: `model` is `generic-image` or `generic-video` (sets the output kind);
//! `params.replicate_model` = `owner/name`; optional `params.version`; `params.input` = the model's input object;
//! `params.estimate_usd` = required positive price. Result files live on Replicate's delivery host and are
//! fetched without credentials.
use super::{
    request_may_have_reached, status_may_have_run, unknown_outcome, Durations, GenRequest, Kind,
    ModelInfo, Output, PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.replicate.com";
const IMAGE: &str = "generic-image";
const VIDEO: &str = "generic-video";
const BODY_LIMIT: usize = 1 << 30;
/// A single request may not reserve more than this; a typo in `estimate_usd` should not drain a balance.
const MAX_ESTIMATE_USD: f64 = 50.0;

static MODELS: [ModelInfo; 2] = [
    ModelInfo {
        provider: "replicate",
        model: IMAGE,
        kind: Kind::Image,
        label: "Replicate: any image model (set params.replicate_model and params.estimate_usd; price unverified)",
        est_usd: 0.05,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: "replicate",
        model: VIDEO,
        kind: Kind::Video,
        label: "Replicate: any video model (set params.replicate_model and params.estimate_usd; price unverified)",
        est_usd: 0.5,
        durations: Durations::NONE,
    },
];

pub struct Replicate {
    base: String,
    agent: Http,
}

impl Replicate {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent: Http::new(Duration::from_secs(10), Duration::from_secs(300)),
        }
    }

    fn get(&self, url: &str, key: Option<&str>) -> Result<Response, ProviderError> {
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

fn slug_ok(s: &str) -> bool {
    let mut it = s.split('/');
    let part = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    };
    part(it.next()) && part(it.next()) && it.next().is_none()
}

struct Plan {
    slug: String,
    version: Option<String>,
    input: Value,
    usd: f64,
}

fn resolve(req: &GenRequest) -> Result<Plan, ProviderError> {
    let info = MODELS
        .iter()
        .find(|m| m.model == req.model)
        .ok_or_else(|| bad(format!("unknown replicate model {}", req.model)))?;
    if req.kind != info.kind {
        return Err(bad("model does not match request kind"));
    }
    let slug = req
        .params
        .get("replicate_model")
        .and_then(Value::as_str)
        .filter(|s| slug_ok(s))
        .ok_or_else(|| bad("params.replicate_model must look like owner/name"))?;
    let version = match req.params.get("version").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(
            v.as_str()
                .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .ok_or_else(|| bad("params.version must be a 64-character version id"))?
                .to_string(),
        ),
    };
    let usd = req
        .params
        .get("estimate_usd")
        .and_then(Value::as_f64)
        .filter(|u| u.is_finite() && *u > 0.0)
        .ok_or_else(|| {
            ProviderError::new(
                "price_unknown",
                "replicate prices vary per model: pass params.estimate_usd from the model page",
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
        slug: slug.to_string(),
        version,
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
            redact(&format!("replicate transport error: {e}"), key),
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
                .or_else(|| v.get("title"))
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
    let billing = ["insufficient", "credit", "billing", "payment"]
        .iter()
        .any(|n| lower.contains(n));
    match status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("replicate rejected the API token ({status}): {text}"),
            false,
        ),
        402 => ProviderError::new(
            "billing",
            format!("replicate billing problem: {text}"),
            false,
        ),
        s if billing && (400..500).contains(&s) && s != 408 && s != 429 => ProviderError::new(
            "billing",
            format!("replicate billing problem: {text}"),
            false,
        ),
        408 | 429 => {
            ProviderError::new("rate_limited", format!("replicate {status}: {text}"), true)
        }
        500..=599 => ProviderError::new("unavailable", format!("replicate {status}: {text}"), true),
        s => ProviderError::new("invalid_request", format!("replicate {s}: {text}"), false),
    }
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Provider for Replicate {
    fn id(&self) -> &'static str {
        "replicate"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        Ok(resolve(req)?.usd)
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        let plan = resolve(req)?;
        let (url, body) = match &plan.version {
            Some(v) => (
                format!("{}/v1/predictions", self.base),
                json!({"version": v, "input": plan.input}),
            ),
            None => (
                format!("{}/v1/models/{}/predictions", self.base, plan.slug),
                json!({"input": plan.input}),
            ),
        };
        let sent = Request::post(url)
            .secret_header("Authorization", format!("Bearer {api_key}"))
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
            ProviderError::new("bad_response", "replicate submit reply was not JSON", true)
                .ambiguous()
        })?;
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| safe_id(i))
            .ok_or_else(|| {
                ProviderError::new(
                    "bad_response",
                    "replicate submit reply had no usable id",
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
            return Err(bad("invalid replicate prediction id"));
        }
        let r = self.get(
            &format!("{}/v1/predictions/{}", self.base, provider_job_id),
            Some(api_key),
        )?;
        if !(200..300).contains(&r.status) {
            return Err(map_status(r.status, error_text(&r.body, api_key)));
        }
        let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
            ProviderError::new(
                "bad_response",
                "replicate prediction reply was not JSON",
                true,
            )
        })?;
        match v.get("status").and_then(Value::as_str).unwrap_or("") {
            "starting" | "processing" => Ok(PollOutcome::Running),
            "succeeded" => {
                let urls: Vec<&str> = match v.get("output") {
                    Some(Value::String(s)) => vec![s.as_str()],
                    Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
                    _ => vec![],
                };
                let outputs: Vec<Output> = urls
                    .into_iter()
                    .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                    .map(|u| Output::Url {
                        url: u.to_string(),
                        mime: super::fal::mime_from_url(u),
                    })
                    .collect();
                if outputs.is_empty() {
                    Ok(unknown_outcome(
                        "succeeded, no file output",
                        &r.body,
                        api_key,
                        true,
                    ))
                } else {
                    Ok(PollOutcome::Completed { outputs })
                }
            }
            // Confirmed-failure vocabulary (closed list): failed, canceled. Anything else is held.
            "failed" | "canceled" => {
                let m = v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                Ok(PollOutcome::Failed {
                    message: redact(&format!("replicate prediction failed: {m}"), api_key),
                })
            }
            "" => Err(ProviderError::new(
                "bad_response",
                "replicate prediction reply had no status",
                true,
            )),
            other => Ok(unknown_outcome(other, &r.body, api_key, false)),
        }
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /v1/account is free and requires a valid token.
        let r = self.get(&format!("{}/v1/account", self.base), Some(api_key))?;
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
                // No credentials: result files are public delivery links.
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const KEY: &str = concat!("r8", "_replicate_secret_1");

    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    fn mock<F>(handler: F) -> (String, Seen)
    where
        F: Fn(&str, &str, &str) -> (u16, &'static str, Vec<u8>) + Send + 'static,
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
                s2.lock().unwrap().push((head.clone(), body));
                let (code, ct, out) = handler(&b2, &method, &path);
                let h = format!("HTTP/1.1 {code} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = st.write_all(h.as_bytes());
                let _ = st.write_all(&out);
            }
        });
        (base, seen)
    }

    fn req(model: &str, params: Value) -> GenRequest {
        let kind = if model == VIDEO {
            Kind::Video
        } else {
            Kind::Image
        };
        GenRequest {
            kind,
            provider: "replicate".into(),
            model: model.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    fn js(v: Value) -> (u16, &'static str, Vec<u8>) {
        (200, "application/json", v.to_string().into_bytes())
    }

    fn ok_params() -> Value {
        json!({"replicate_model": "black-forest-labs/flux-schnell", "estimate_usd": 0.003, "input": {"aspect_ratio": "1:1"}})
    }

    #[test]
    fn price_must_be_stated_and_is_capped() {
        let r = Replicate::new("http://127.0.0.1:1");
        assert!((r.estimate_usd(&req(IMAGE, ok_params())).unwrap() - 0.003).abs() < 1e-12);
        let e = r
            .estimate_usd(&req(IMAGE, json!({"replicate_model": "a/b"})))
            .unwrap_err();
        assert_eq!(e.code, "price_unknown");
        for p in [
            json!({"replicate_model": "a/b", "estimate_usd": 0}),
            json!({"replicate_model": "a/b", "estimate_usd": -1}),
            json!({"replicate_model": "a/b", "estimate_usd": 500}),
            json!({"replicate_model": "a", "estimate_usd": 1}),
            json!({"replicate_model": "a/b/c", "estimate_usd": 1}),
            json!({"replicate_model": "a/b", "estimate_usd": 1, "version": "short"}),
            json!({"replicate_model": "a/b", "estimate_usd": 1, "input": [1]}),
        ] {
            assert!(r.estimate_usd(&req(IMAGE, p.clone())).is_err(), "{p}");
        }
    }

    #[test]
    fn model_route_flow_and_credential_scope() {
        let polls = Arc::new(Mutex::new(0));
        let p2 = polls.clone();
        let (base, seen) = mock(move |base, m, path| match (m, path) {
            ("POST", "/v1/models/black-forest-labs/flux-schnell/predictions") => {
                js(json!({"id": "pred1", "status": "starting"}))
            }
            ("GET", "/v1/predictions/pred1") => {
                let mut n = p2.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    js(json!({"status": "processing"}))
                } else {
                    js(json!({"status": "succeeded", "output": [format!("{base}/cdn/a.webp")]}))
                }
            }
            ("GET", "/cdn/a.webp") => (200, "image/webp", vec![1, 2, 3]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let rp = Replicate::new(&base);
        let r = req(IMAGE, ok_params());
        let SubmitOutcome::Accepted { provider_job_id } = rp.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            rp.poll(&r, &provider_job_id, KEY).unwrap(),
            PollOutcome::Running
        );
        let PollOutcome::Completed { outputs } = rp.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(
            rp.fetch(&outputs[0]).unwrap(),
            (vec![1, 2, 3], "image/webp".to_string())
        );
        let s = seen.lock().unwrap();
        let b: Value = serde_json::from_str(&s[0].1).unwrap();
        assert_eq!(
            b,
            json!({"input": {"aspect_ratio": "1:1", "prompt": "a red fox"}})
        );
        assert!(s[0]
            .0
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
        assert!(!s
            .iter()
            .find(|x| x.0.contains("/cdn/a.webp"))
            .unwrap()
            .0
            .contains(KEY));
    }

    #[test]
    fn pinned_version_route_and_string_output() {
        let ver = "a".repeat(64);
        let (base, seen) = mock(|base, m, path| match (m, path) {
            ("POST", "/v1/predictions") => js(json!({"id": "pv1"})),
            ("GET", "/v1/predictions/pv1") => {
                js(json!({"status": "succeeded", "output": format!("{base}/cdn/v.mp4")}))
            }
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let rp = Replicate::new(&base);
        let r = req(
            VIDEO,
            json!({"replicate_model": "o/m", "version": ver, "estimate_usd": 0.4}),
        );
        rp.submit(&r, KEY).unwrap();
        let b: Value = serde_json::from_str(&seen.lock().unwrap()[0].1).unwrap();
        assert_eq!(b["version"], ver.as_str());
        let PollOutcome::Completed { outputs } = rp.poll(&r, "pv1", KEY).unwrap() else {
            panic!()
        };
        assert!(matches!(&outputs[0], Output::Url { mime: Some(m), .. } if m == "video/mp4"));
    }

    #[test]
    fn terminal_and_unknown_states() {
        let (base, _) = mock(|_, _, path| match path {
            "/v1/predictions/f1" => {
                js(json!({"status": "failed", "error": "NSFW content detected"}))
            }
            "/v1/predictions/c1" => js(json!({"status": "canceled"})),
            "/v1/predictions/e1" => js(json!({"status": "succeeded", "output": null})),
            "/v1/predictions/w1" => js(json!({"status": "weird"})),
            "/v1/account" => js(json!({"username": "u"})),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let rp = Replicate::new(&base);
        let r = req(IMAGE, ok_params());
        assert_eq!(
            rp.poll(&r, "f1", KEY).unwrap(),
            PollOutcome::Failed {
                message: "replicate prediction failed: NSFW content detected".into()
            }
        );
        assert!(matches!(
            rp.poll(&r, "c1", KEY).unwrap(),
            PollOutcome::Failed { .. }
        ));
        assert!(matches!(
            rp.poll(&r, "e1", KEY).unwrap(),
            PollOutcome::Unknown {
                completed: true,
                ..
            }
        ));
        assert!(matches!(
            rp.poll(&r, "w1", KEY).unwrap(),
            PollOutcome::Unknown {
                completed: false,
                ..
            }
        ));
        assert!(rp.poll(&r, "../x", KEY).is_err());
        assert!(rp.test_key(KEY).is_ok());
    }

    #[test]
    fn invalid_requests_never_reach_network_and_errors_map() {
        let (base, seen) = mock(|_, _, _| js(json!({"id": "x"})));
        let rp = Replicate::new(&base);
        assert!(rp
            .submit(&req(IMAGE, json!({"replicate_model": "a/b"})), KEY)
            .is_err());
        assert!(rp.submit(&req("flux", ok_params()), KEY).is_err());
        let mut wrong = req(IMAGE, ok_params());
        wrong.kind = Kind::Video;
        assert!(rp.submit(&wrong, KEY).is_err());
        assert!(seen.lock().unwrap().is_empty());
        for (code, c, retry, amb) in [
            (401u16, "auth", false, false),
            (402, "billing", false, false),
            (422, "invalid_request", false, false),
            (429, "rate_limited", true, true),
            (500, "unavailable", true, true),
        ] {
            let (base, _) = mock(move |_, _, _| {
                (
                    code,
                    "application/json",
                    format!("{{\"detail\":\"bad {KEY}\"}}").into_bytes(),
                )
            });
            let e = Replicate::new(&base)
                .submit(&req(IMAGE, ok_params()), KEY)
                .unwrap_err();
            assert_eq!(
                (e.code.as_str(), e.retryable, e.ambiguous),
                (c, retry, amb),
                "{code} {e:?}"
            );
            assert!(!e.message.contains(KEY));
        }
    }
}
