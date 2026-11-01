//! OpenAI Images adapter (direct): `gpt-image-2`.
//!
//! Sources (fetched 2026-10-17):
//! - Guide: <https://developers.openai.com/api/docs/guides/image-generation>. `POST /v1/images/generations`
//!   takes `model`, `prompt`, `n`, `size`, `quality`, `output_format`, `background`; the reply is
//!   `{"data":[{"b64_json": "..."}]}` (base64 image). `gpt-image-2` quality is low|medium|high.
//!   Recommended sizes: 1024x1024, 1536x1024, 1024x1536.
//! - Per-image prices (guide table, same page, 2026-10-17), 1024x1024 / 1024x1536 / 1536x1024:
//!   low $0.006/$0.005/$0.005, medium $0.053/$0.041/$0.041, high $0.211/$0.165/$0.165.
//!   Token rates (<https://developers.openai.com/api/docs/pricing>, undated page): image output $30/1M
//!   tokens. The guide table is the only per-image source. UNVERIFIED against an invoice.
//! - NOT covered, follow-up: `gpt-image-2.5-sunburst` / `gpt-image-2.5-flare` (documented, but the page lists
//!   no per-image price, so no safe estimate) and the multipart `/v1/images/edits` route (reference images).
//!
//! The call is synchronous: `submit` returns the decoded bytes. The key goes only to the configured host.
use super::{
    request_may_have_reached, status_may_have_run, Durations, GenRequest, Kind, ModelInfo, Output,
    PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use crate::capabilities::{self, Capabilities};
use crate::host::http::{self, Http, HttpError, Request, Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://api.openai.com";
const GPT: &str = "gpt-image-2";
const BODY_LIMIT: usize = 1 << 28;

static MODELS: [ModelInfo; 1] = [ModelInfo {
    provider: "openai",
    model: GPT,
    kind: Kind::Image,
    label: "OpenAI GPT Image 2 direct (medium ~$0.041-0.053, high ~$0.165-0.211; docs price, unverified)",
    est_usd: 0.053,
    durations: Durations::NONE,
}];

pub struct OpenAi {
    base: String,
    agent: Http,
}

impl OpenAi {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent: Http::new(Duration::from_secs(10), Duration::from_secs(300)),
        }
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

/// (size string, price index: 0 = square, 1 = non-square).
fn size(req: &GenRequest) -> Result<(&'static str, usize), ProviderError> {
    let a = str_param(req, "aspect")
        .or_else(|| str_param(req, "aspect_ratio"))
        .or_else(|| str_param(req, "size"));
    match a {
        None | Some("1:1") | Some("square") | Some("1024x1024") => Ok(("1024x1024", 0)),
        Some("3:2") | Some("landscape") | Some("1536x1024") => Ok(("1536x1024", 1)),
        Some("2:3") | Some("portrait") | Some("1024x1536") => Ok(("1024x1536", 1)),
        Some(o) => Err(bad(format!(
            "gpt-image-2 supports 1:1, 3:2 and 2:3 (1024x1024, 1536x1024, 1024x1536), got {o}"
        ))),
    }
}

fn price(q: &str, idx: usize) -> f64 {
    match (q, idx) {
        ("low", 0) => 0.006,
        ("low", _) => 0.005,
        ("high", 0) => 0.211,
        ("high", _) => 0.165,
        (_, 0) => 0.053,
        _ => 0.041,
    }
}

fn resolve(req: &GenRequest) -> Result<(), ProviderError> {
    if req.model != GPT {
        return Err(bad(format!("unknown openai model {}", req.model)));
    }
    if req.kind != Kind::Image {
        return Err(bad("model does not match request kind"));
    }
    if req.prompt.trim().is_empty() {
        return Err(bad("prompt is required"));
    }
    quality(req)?;
    size(req)?;
    if req
        .params
        .get("n")
        .and_then(Value::as_u64)
        .is_some_and(|n| n != 1)
    {
        return Err(bad("send n = 1: one image is priced per call"));
    }
    Ok(())
}

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

fn error_text(body: &[u8], key: &str) -> String {
    let v: Option<Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.get("error"))
        .map(|e| {
            let m = e
                .get("message")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| e.to_string());
            match e.get("code").and_then(Value::as_str) {
                Some(c) => format!("{c}: {m}"),
                None => m,
            }
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    redact(&msg.chars().take(300).collect::<String>(), key)
}

fn map_status(status: u16, text: String) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    // 429 also carries insufficient_quota, which is a billing stop, not a retry.
    let balance = ["insufficient_quota", "billing", "quota"]
        .iter()
        .any(|n| lower.contains(n));
    match status {
        401 | 403 => ProviderError::new(
            "auth",
            format!("openai rejected the API key ({status}): {text}"),
            false,
        ),
        402 => ProviderError::new("billing", format!("openai billing problem: {text}"), false),
        s if balance && !(500..=599).contains(&s) => {
            ProviderError::new("billing", format!("openai billing problem: {text}"), false)
        }
        408 | 429 => ProviderError::new("rate_limited", format!("openai {status}: {text}"), true),
        500..=599 => ProviderError::new("unavailable", format!("openai {status}: {text}"), true),
        s => ProviderError::new("invalid_request", format!("openai {s}: {text}"), false),
    }
}

fn read(r: Result<Response, HttpError>, key: &str) -> Result<Response, ProviderError> {
    r.map_err(|e| {
        ProviderError::new(
            "transport",
            redact(&format!("openai transport error: {e}"), key),
            http::retryable(&e),
        )
    })
}

/// Standard base64; whitespace and padding optional. None on any other byte.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut t: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '=')
        .collect();
    while t.len() % 4 != 0 {
        t.push('=');
    }
    rightkit_crypto::base64_decode(&t).ok()
}

impl Provider for OpenAi {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities::default().mode("prompt").setting(
            "aspect",
            capabilities::aspect("aspect", &["1:1", "3:2", "2:3"], false),
        )
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        resolve(req)?;
        Ok(price(quality(req)?, size(req)?.1))
    }

    fn submit(&self, req: &GenRequest, api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        resolve(req)?;
        let body = json!({
            "model": GPT, "prompt": req.prompt, "n": 1,
            "size": size(req)?.0, "quality": quality(req)?, "output_format": "png",
        });
        let sent = Request::post(format!("{}/v1/images/generations", self.base))
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
            ProviderError::new("bad_response", "openai reply was not JSON", true).ambiguous()
        })?;
        let bytes = v
            .pointer("/data/0/b64_json")
            .and_then(Value::as_str)
            .and_then(b64_decode)
            .filter(|b| !b.is_empty())
            .ok_or_else(|| {
                ProviderError::new("bad_response", "openai reply had no image", true).ambiguous()
            })?;
        Ok(SubmitOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes,
                mime: "image/png".into(),
            }],
        })
    }

    fn poll(
        &self,
        _req: &GenRequest,
        _id: &str,
        _api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        Err(ProviderError::new(
            "unsupported",
            "openai image calls are synchronous",
            false,
        ))
    }

    fn test_key(&self, api_key: &str) -> Result<(), ProviderError> {
        // GET /v1/models is free and requires a valid key.
        let r = read(
            self.agent.send(
                Request::get(format!("{}/v1/models", self.base))
                    .secret_header("Authorization", format!("Bearer {api_key}")),
            ),
            api_key,
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
            _ => Err(ProviderError::new(
                "unsupported",
                "openai returns bytes only",
                false,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const KEY: &str = "sk-openai-secret-123";

    fn mock(code: u16, out: String) -> (String, Arc<Mutex<Vec<(String, String)>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
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
                s2.lock()
                    .unwrap()
                    .push((head, String::from_utf8_lossy(&buf[hend..]).to_string()));
                let h = format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = st.write_all(h.as_bytes());
                let _ = st.write_all(out.as_bytes());
            }
        });
        (base, seen)
    }

    fn req(params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::Image,
            provider: "openai".into(),
            model: GPT.into(),
            prompt: "a red fox".into(),
            params,
        }
    }

    #[test]
    fn base64_decoding() {
        assert_eq!(b64_decode("AQID").unwrap(), vec![1, 2, 3]);
        assert_eq!(b64_decode("AQI=").unwrap(), vec![1, 2]);
        assert!(b64_decode("A*ID").is_none());
    }

    #[test]
    fn prices_follow_the_documented_table() {
        let o = OpenAi::new("http://127.0.0.1:1");
        for (p, usd) in [
            (json!({}), 0.053),
            (json!({"quality": "high"}), 0.211),
            (json!({"quality": "high", "aspect": "3:2"}), 0.165),
            (json!({"quality": "low", "aspect": "portrait"}), 0.005),
            (json!({"quality": "medium", "aspect": "2:3"}), 0.041),
        ] {
            assert!(
                (o.estimate_usd(&req(p.clone())).unwrap() - usd).abs() < 1e-9,
                "{p}"
            );
        }
    }

    #[test]
    fn submit_sends_exact_body_and_decodes_bytes() {
        let (base, seen) = mock(200, json!({"data": [{"b64_json": "AQID"}]}).to_string());
        let o = OpenAi::new(&base);
        let SubmitOutcome::Completed { outputs } = o
            .submit(&req(json!({"quality": "high", "aspect": "3:2"})), KEY)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            o.fetch(&outputs[0]).unwrap(),
            (vec![1, 2, 3], "image/png".to_string())
        );
        let s = seen.lock().unwrap();
        assert!(s[0].0.starts_with("POST /v1/images/generations"));
        assert!(s[0]
            .0
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
        let b: Value = serde_json::from_str(&s[0].1).unwrap();
        assert_eq!(
            (
                b["model"].as_str(),
                b["size"].as_str(),
                b["quality"].as_str(),
                b["n"].as_u64()
            ),
            (Some(GPT), Some("1536x1024"), Some("high"), Some(1))
        );
    }

    #[test]
    fn invalid_requests_never_reach_network() {
        let (base, seen) = mock(200, "{}".into());
        let o = OpenAi::new(&base);
        for p in [
            json!({"quality": "max"}),
            json!({"aspect": "16:9"}),
            json!({"n": 2}),
        ] {
            assert!(o.submit(&req(p), KEY).is_err());
        }
        let mut r = req(json!({}));
        r.model = "dall-e-3".into();
        assert!(o.submit(&r, KEY).is_err());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn errors_map_redact_and_flag_ambiguity() {
        for (code, body, c, retry, amb) in [
            (
                401u16,
                format!("{{\"error\":{{\"message\":\"bad {KEY}\"}}}}"),
                "auth",
                false,
                false,
            ),
            (
                429,
                "{\"error\":{\"code\":\"insufficient_quota\",\"message\":\"x\"}}".to_string(),
                "billing",
                false,
                true,
            ),
            (
                429,
                "{\"error\":{\"message\":\"slow down\"}}".to_string(),
                "rate_limited",
                true,
                true,
            ),
            (500, "{}".to_string(), "unavailable", true, true),
            (
                400,
                "{\"error\":{\"message\":\"moderation blocked\"}}".to_string(),
                "invalid_request",
                false,
                false,
            ),
        ] {
            let (base, _) = mock(code, body);
            let e = OpenAi::new(&base).submit(&req(json!({})), KEY).unwrap_err();
            assert_eq!(
                (e.code.as_str(), e.retryable, e.ambiguous),
                (c, retry, amb),
                "{code} {e:?}"
            );
            assert!(!e.message.contains(KEY));
        }
        let (base, _) = mock(200, "{\"data\":[]}".to_string());
        let e = OpenAi::new(&base).submit(&req(json!({})), KEY).unwrap_err();
        assert!(e.ambiguous && e.code == "bad_response");
    }
}
