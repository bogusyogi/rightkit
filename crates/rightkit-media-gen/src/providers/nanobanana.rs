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

    fn req(params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::Image,
            provider: "nanobanana".into(),
            model: MODEL.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    const KEY: &str = concat!("nb", "-secret-KEY-123");

    fn json_resp(v: Value) -> (u16, &'static str, Vec<u8>) {
        (200, "application/json", v.to_string().into_bytes())
    }

    #[test]
    fn submit_poll_fetch_and_request_shape() {
        let polls = Arc::new(Mutex::new(0));
        let p2 = polls.clone();
        let (base, seen) = mock(move |base, m, path, _| match (m, path) {
            ("POST", "/api/v1/nanobanana/generate-2") => {
                json_resp(json!({"code":200,"msg":"success","data":{"taskId":"t 1"}}))
            }
            ("GET", "/api/v1/nanobanana/record-info?taskId=t%201") => {
                let mut n = p2.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    json_resp(json!({"code":200,"data":{"successFlag":0}}))
                } else {
                    json_resp(
                        json!({"code":200,"data":{"successFlag":1,"response":{"resultImageUrl": format!("{base}/img/a.png")}}}),
                    )
                }
            }
            ("GET", "/img/a.png") => (200, "image/png", vec![7, 8, 9]),
            _ => (404, "text/plain", b"nope".to_vec()),
        });
        let nb = NanoBanana::new(&base);
        let r =
            req(json!({"aspect": "16:9", "resolution": "4K", "image_urls": ["https://x/ref.png"]}));
        let SubmitOutcome::Accepted { provider_job_id } = nb.submit(&r, KEY).unwrap() else {
            panic!()
        };
        assert_eq!(provider_job_id, "t 1");
        assert_eq!(
            nb.poll(&r, &provider_job_id, KEY).unwrap(),
            PollOutcome::Running
        );
        let PollOutcome::Completed { outputs } = nb.poll(&r, &provider_job_id, KEY).unwrap() else {
            panic!()
        };
        let (bytes, mime) = nb.fetch(&outputs[0]).unwrap();
        assert_eq!((bytes, mime.as_str()), (vec![7, 8, 9], "image/png"));

        let seen = seen.lock().unwrap();
        assert!(seen[0]
            .headers
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
        let b: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(b["aspectRatio"], "16:9");
        assert_eq!(b["resolution"], "4K");
        assert_eq!(b["imageUrls"][0], "https://x/ref.png");
        assert_eq!(b["outputFormat"], "png");
        assert_eq!(b["prompt"], "a red fox");
        let get = seen.iter().find(|s| s.path == "/img/a.png").unwrap();
        assert!(!get.headers.contains(KEY));
    }

    #[test]
    fn https_reference_passes_through_unchanged() {
        let (base, seen) = mock(|_, _, _, _| json_resp(json!({"code":200,"data":{"taskId":"t1"}})));
        let nb = NanoBanana::new(&base);
        let r = req(json!({"image_urls": ["https://x/ref.png", "http://127.0.0.1:9/b.png"]}));
        nb.estimate_usd(&r).unwrap();
        nb.submit(&r, KEY).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "only the generate call is sent");
        assert_eq!(seen[0].path, GENERATE_PATH);
        let b: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(
            b["imageUrls"],
            json!(["https://x/ref.png", "http://127.0.0.1:9/b.png"])
        );
    }

    #[test]
    fn local_reference_is_refused_before_any_request() {
        let (base, seen) = mock(|_, _, _, _| json_resp(json!({"code":200,"data":{"taskId":"t1"}})));
        let nb = NanoBanana::new(&base);
        for local in [
            "/Users/someone/Pictures/page 1.png",
            "file:///Users/someone/page.png",
            "C:\\Users\\someone\\page.png",
        ] {
            let r = req(json!({"image_urls": ["https://x/ok.png", local]}));
            for e in [
                nb.estimate_usd(&r).unwrap_err(),
                nb.submit(&r, KEY).unwrap_err(),
            ] {
                assert_eq!(
                    (e.code.as_str(), e.message.as_str()),
                    ("invalid_request", LOCAL_REFERENCE_REFUSED),
                    "{local}"
                );
                assert!(!e.retryable);
            }
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "no request reached the host"
        );
    }

    #[test]
    fn failures_map_to_failed() {
        let (base, _) = mock(|_, _, path, _| {
            if path.contains("bad1") {
                json_resp(
                    json!({"code":200,"data":{"successFlag":3,"errorMessage":"content policy"}}),
                )
            } else if path.contains("bad2") {
                json_resp(json!({"code":200,"data":{"successFlag":1,"response":{}}}))
            } else {
                (404, "text/plain", b"".to_vec())
            }
        });
        let nb = NanoBanana::new(&base);
        match nb.poll(&req(json!({})), "bad1", KEY).unwrap() {
            PollOutcome::Failed { message } => {
                assert!(message.contains("content policy"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        // old: flag 1 with no URL was `Failed`; new: `Unknown` with completed=true (held, settled).
        match nb.poll(&req(json!({})), "bad2", KEY).unwrap() {
            PollOutcome::Unknown { completed, .. } => assert!(completed),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn error_mapping_and_no_key_leak() {
        for (status, code, retry) in [
            (401, "auth", false),
            (403, "auth", false),
            (402, "billing", false),
            (429, "rate_limited", true),
            (503, "unavailable", true),
            (422, "invalid_request", false),
        ] {
            let (base, _) = mock(move |_, _, _, _| {
                (
                    status,
                    "application/json",
                    format!(r#"{{"msg":"bad key {KEY}"}}"#).into_bytes(),
                )
            });
            let e = NanoBanana::new(&base)
                .submit(&req(json!({})), KEY)
                .unwrap_err();
            assert_eq!(
                (e.code.as_str(), e.retryable),
                (code, retry),
                "status {status}"
            );
            assert!(!e.message.contains(KEY), "{}", e.message);
        }
        // HTTP 200 carrying a body-level error code
        for (c, code) in [
            (401u64, "auth"),
            (402, "billing"),
            (429, "rate_limited"),
            (500, "unavailable"),
        ] {
            let (base, _) =
                mock(move |_, _, _, _| json_resp(json!({"code": c, "msg": format!("nope {KEY}")})));
            let e = NanoBanana::new(&base)
                .submit(&req(json!({})), KEY)
                .unwrap_err();
            assert_eq!(e.code, code);
            assert!(!e.message.contains(KEY));
        }
        let e = NanoBanana::new("http://127.0.0.1:1")
            .submit(&req(json!({})), KEY)
            .unwrap_err();
        assert!(e.retryable && e.code == "transport" && !e.message.contains(KEY));
    }

    #[test]
    fn key_check_and_estimates() {
        let (base, seen) =
            mock(|_, _, _, _| json_resp(json!({"code":200,"msg":"success","data":120})));
        NanoBanana::new(&base).test_key(KEY).unwrap();
        assert_eq!(seen.lock().unwrap()[0].path, "/api/v1/common/credit");
        let (base, _) = mock(|_, _, _, _| json_resp(json!({"code":401,"msg":"unauthorized"})));
        assert_eq!(
            NanoBanana::new(&base).test_key(KEY).unwrap_err().code,
            "auth"
        );
        let (base, _) = mock(|_, _, _, _| (401, "application/json", b"{}".to_vec()));
        assert_eq!(
            NanoBanana::new(&base).test_key(KEY).unwrap_err().code,
            "auth"
        );

        let nb = NanoBanana::new("http://x");
        for (p, usd) in [
            (json!({}), 0.06),
            (json!({"resolution": "1K"}), 0.04),
            (json!({"resolution": "4K"}), 0.09),
        ] {
            assert!((nb.estimate_usd(&req(p)).unwrap() - usd).abs() < 1e-9);
        }
        assert_eq!(
            nb.estimate_usd(&req(json!({"resolution": "8K"})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            nb.estimate_usd(&req(json!({"image_urls": ["file:///etc/passwd"]})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            nb.estimate_usd(&req(json!({"image_urls": ["ftp://x/a.png"]})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            nb.estimate_usd(&req(json!({"image_urls": ["/definitely/missing.png"]})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        let mut bad = req(json!({}));
        bad.model = "nope".into();
        assert_eq!(nb.estimate_usd(&bad).unwrap_err().code, "invalid_request");
        assert_eq!(nb.models().len(), 1);
    }
}
