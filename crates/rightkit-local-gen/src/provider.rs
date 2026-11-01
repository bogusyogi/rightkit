// Local image generation: see docs/plans/local-image-generation.md
// Loopback HTTP adapter for the pinned sd.cpp server contract (/sdcpp/v1/*).

use std::fmt;
use std::time::Duration;

use serde_json::{json, Value};

use crate::http::{Http, HttpError, Request};

pub const SESSION_HEADER: &str = "X-SDCPP-Session";
pub const SESSION_IDENTITY_HEADER: &str = "X-SDCPP-Session-Identity";

#[derive(Debug)]
pub struct ProviderError {
    pub code: &'static str,
    pub message: String,
}

impl ProviderError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProviderError {}

#[derive(Debug)]
pub enum CancelOutcome {
    Cancelled,
    NotFound,
    Conflict,
    Gone,
}

#[derive(Debug)]
pub struct ProviderJob {
    pub id: String,
    pub status: ProviderStatus,
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub struct ValidatedCapabilities {
    pub capabilities: Value,
    pub session_id: String,
}

#[derive(Debug)]
pub enum ProviderStatus {
    Queued,
    Generating,
    Completed {
        images: Vec<String>,
        output_format: String,
    },
    Failed {
        code: String,
        message: String,
    },
    Cancelled,
}

#[derive(Debug)]
pub struct ImgGenRequest {
    pub prompt: String,
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
    pub seed: i64,
    pub ref_images: Vec<String>,
    pub sample_steps: u32,
    pub sample_method: String,
    pub scheduler: Option<String>,
    pub txt_cfg: f64,
}

pub struct SdCppClient {
    agent: Http,
    status_agent: Http,
    base: String,
    token: String,
}

impl fmt::Debug for SdCppClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SdCppClient")
            .field("base", &self.base)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl SdCppClient {
    pub fn new(port: u16, token: &str) -> Self {
        // Submit agent: old config had a 20 s global timeout and no connect timeout.
        // Connect is set to 10 s (the rightkit default); total is the old 20 s.
        let agent = Http::new(Duration::from_secs(10), Duration::from_secs(20));
        // Probe agent: 500 ms connect, 500 ms response, 3 s body, 5 s for the whole call.
        let status_agent = Http::with_phases(
            Duration::from_millis(500),
            Duration::from_secs(5),
            Duration::from_millis(500),
            Duration::from_secs(3),
        );
        Self {
            agent,
            status_agent,
            base: format!("http://127.0.0.1:{port}"),
            token: token.to_string(),
        }
    }

    pub fn capabilities(&self) -> Result<Value, ProviderError> {
        let response = self
            .status_agent
            .send(
                Request::get(format!("{}/sdcpp/v1/capabilities", self.base))
                    .header(SESSION_HEADER, &self.token),
            )
            .map_err(transport_err)?;
        let status = response.status;
        if status == 200 {
            response
                .json::<Value>()
                .map_err(|error| ProviderError::new("decode", error.to_string()))
        } else {
            Err(map_http_err(status, body_snippet(&response.body)))
        }
    }

    /// Prove that endpoint belongs to this authenticated server instance.
    /// Each probe uses a short-lived client so readiness cannot inherit the
    /// longer timeout needed by generation submission.
    pub fn verify_session<F>(
        &self,
        expected_session_id: &str,
        mut cancelled: F,
    ) -> Result<ValidatedCapabilities, ProviderError>
    where
        F: FnMut() -> bool,
    {
        // Old agent: 250 ms global timeout and no per-phase timeouts. Http::new(250 ms, 250 ms)
        // bounds connect, response and the whole call by the same 250 ms.
        let timeout = Duration::from_millis(250);
        let agent = Http::new(timeout, timeout);
        let url = format!("{}/sdcpp/v1/capabilities", self.base);

        if cancelled() {
            return Err(ProviderError::new(
                "cancelled",
                "session verification cancelled",
            ));
        }
        let missing = agent.send(Request::get(&url)).map_err(transport_err)?;
        if missing.status != 401 {
            return Err(ProviderError::new(
                "auth_probe",
                format!("missing-token probe returned HTTP {}", missing.status),
            ));
        }

        if cancelled() {
            return Err(ProviderError::new(
                "cancelled",
                "session verification cancelled",
            ));
        }
        let wrong = agent
            .send(Request::get(&url).header(SESSION_HEADER, "rightkit-invalid-session-probe"))
            .map_err(transport_err)?;
        if wrong.status != 401 {
            return Err(ProviderError::new(
                "auth_probe",
                format!("wrong-token probe returned HTTP {}", wrong.status),
            ));
        }

        if cancelled() {
            return Err(ProviderError::new(
                "cancelled",
                "session verification cancelled",
            ));
        }
        let origin = agent
            .send(
                Request::get(&url)
                    .header("Origin", "https://rightkit.invalid")
                    .header(SESSION_HEADER, &self.token),
            )
            .map_err(transport_err)?;
        if origin.status != 403 {
            return Err(ProviderError::new(
                "auth_probe",
                format!("nonempty-Origin probe returned HTTP {}", origin.status),
            ));
        }

        if cancelled() {
            return Err(ProviderError::new(
                "cancelled",
                "session verification cancelled",
            ));
        }
        let authenticated = agent
            .send(Request::get(&url).header(SESSION_HEADER, &self.token))
            .map_err(transport_err)?;
        if authenticated.status != 200 {
            return Err(ProviderError::new(
                "auth_probe",
                format!(
                    "authenticated capabilities returned HTTP {}",
                    authenticated.status
                ),
            ));
        }
        let identity = authenticated
            .header(SESSION_IDENTITY_HEADER)
            .unwrap_or_default();
        if identity != expected_session_id {
            return Err(ProviderError::new(
                "session_identity",
                "authenticated capabilities returned a different session identity",
            ));
        }
        let capabilities = authenticated
            .json::<Value>()
            .map_err(|error| ProviderError::new("decode", error.to_string()))?;
        Ok(ValidatedCapabilities {
            capabilities,
            session_id: expected_session_id.to_string(),
        })
    }

    pub fn submit_img_gen(&self, req: &ImgGenRequest) -> Result<String, ProviderError> {
        let body = build_img_gen_body(req);
        let request = Request::post(format!("{}/sdcpp/v1/img_gen", self.base))
            .header(SESSION_HEADER, &self.token)
            .json(&body)
            .map_err(transport_err)?;
        let response = self.agent.send(request).map_err(transport_err)?;
        let status = response.status;
        if status == 202 {
            let value: Value = response
                .json()
                .map_err(|error| ProviderError::new("decode", error.to_string()))?;
            value["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| ProviderError::new("decode", "img_gen response missing id"))
        } else {
            let snippet = body_snippet(&response.body);
            if status == 400 || status == 429 {
                Err(ProviderError::new(
                    "submit_rejected",
                    format!("http {status}: {snippet}"),
                ))
            } else {
                Err(map_http_err(status, snippet))
            }
        }
    }

    pub fn job(&self, id: &str) -> Result<ProviderJob, ProviderError> {
        let response = self
            .status_agent
            .send(
                Request::get(format!("{}/sdcpp/v1/jobs/{id}", self.base))
                    .header(SESSION_HEADER, &self.token),
            )
            .map_err(transport_err)?;
        let status = response.status;
        if status == 200 {
            let value: Value = response
                .json()
                .map_err(|error| ProviderError::new("decode", error.to_string()))?;
            parse_provider_job(&value)
        } else {
            Err(map_http_err(status, body_snippet(&response.body)))
        }
    }

    pub fn cancel_job(&self, id: &str) -> Result<CancelOutcome, ProviderError> {
        let request = Request::post(format!("{}/sdcpp/v1/jobs/{id}/cancel", self.base))
            .header(SESSION_HEADER, &self.token)
            .json(&json!({}))
            .map_err(transport_err)?;
        let response = self.status_agent.send(request).map_err(transport_err)?;
        let status = response.status;
        match status {
            200 => Ok(CancelOutcome::Cancelled),
            404 => Ok(CancelOutcome::NotFound),
            409 => Ok(CancelOutcome::Conflict),
            410 => Ok(CancelOutcome::Gone),
            _ => Err(map_http_err(status, body_snippet(&response.body))),
        }
    }
}

pub fn build_img_gen_body(req: &ImgGenRequest) -> Value {
    let mut sample_params = json!({
        "sample_steps": req.sample_steps,
        "sample_method": req.sample_method,
        "guidance": { "txt_cfg": req.txt_cfg },
    });
    if let Some(scheduler) = &req.scheduler {
        sample_params["scheduler"] = json!(scheduler);
    }
    json!({
        "prompt": req.prompt,
        "negative_prompt": req.negative_prompt,
        "width": req.width,
        "height": req.height,
        "seed": req.seed,
        "batch_count": 1,
        "ref_images": req.ref_images,
        "sample_params": sample_params,
        "output_format": "png",
        "output_compression": 100,
        "embed_image_metadata": true,
    })
}

pub fn parse_provider_job(value: &Value) -> Result<ProviderJob, ProviderError> {
    let decode = |message: &str| ProviderError::new("decode", message.to_string());
    let id = value["id"].as_str().unwrap_or_default().to_string();
    let status = value["status"]
        .as_str()
        .ok_or_else(|| decode("job missing status"))?;
    let status = match status {
        "queued" => ProviderStatus::Queued,
        "generating" => ProviderStatus::Generating,
        "completed" => {
            let images = value["result"]["images"]
                .as_array()
                .ok_or_else(|| decode("completed job missing result.images"))?
                .iter()
                .map(|image| image["b64_json"].as_str().unwrap_or_default().to_string())
                .collect();
            let output_format = value["result"]["output_format"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            ProviderStatus::Completed {
                images,
                output_format,
            }
        }
        "failed" => ProviderStatus::Failed {
            code: value["error"]["code"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            message: value["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        },
        "cancelled" => ProviderStatus::Cancelled,
        other => return Err(decode(&format!("unknown job status: {other}"))),
    };
    Ok(ProviderJob {
        id,
        status,
        raw: value.clone(),
    })
}

pub fn encode_ref_image(bytes: &[u8], mime: &str) -> String {
    format!(
        "data:{mime};base64,{}",
        rightkit_crypto::base64_encode(bytes)
    )
}

// Every HttpError maps to code "transport"; runtime.rs and lib.rs branch on that code.
// A failed body read on a 2xx submit is also "transport", so lib.rs records it as submit_ambiguous.
fn transport_err(error: HttpError) -> ProviderError {
    ProviderError::new("transport", error.to_string())
}

// The body is read whole inside `send`, so a failed read is a transport error, not an empty snippet.
// Invalid UTF-8 gives an empty snippet.
fn body_snippet(body: &[u8]) -> String {
    std::str::from_utf8(body)
        .map(|text| text.chars().take(200).collect())
        .unwrap_or_default()
}

fn map_http_err(status: u16, body: String) -> ProviderError {
    let code = match status {
        401 | 403 => "auth",
        404 => "not_found",
        409 => "conflict",
        410 => "gone",
        _ => "http",
    };
    ProviderError::new(code, format!("http {status}: {body}"))
}
