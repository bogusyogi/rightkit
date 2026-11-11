//! The typed request and result API: a [`GenerateRequest`] becomes a job, the job a [`JobRecord`],
//! and a finished job a [`JobResult`] of [`Asset`]s.
//!
//! It is the same path as the JSON command surface in [`crate::api`] (validation, routing record,
//! spend reservation, durable job, library descriptor), so a host that embeds the crate in Rust
//! and an engine that answers JSON commands get identical behaviour. Unknown fields the engine
//! adds later are kept in each type's `extra` map instead of being dropped.
//!
//! ```no_run
//! use rightkit_media_gen::providers::Kind;
//! use rightkit_media_gen::typed::{GenerateRequest, MediaGen};
//!
//! let engine = MediaGen::new(rightkit_media_gen::Context::new("/tmp/media-gen".into()));
//! let request = GenerateRequest::new(Kind::Image, "fal", "fal-ai/flux/dev", "a lighthouse at dawn")
//!     .with_request_id("lighthouse-1");
//! let quote = engine.estimate(&request)?;
//! let job = engine.submit(&request)?;
//! let done = engine.wait(&job.job_id, Some(600.0))?;
//! if done.job.status == "complete" {
//!     let result = engine.result(&job.job_id)?;
//!     for asset in result.assets {
//!         println!("{} {}", asset.mime, asset.path);
//!     }
//! }
//! # let _ = quote;
//! # Ok::<(), rightkit_media_gen::GenError>(())
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::providers::Kind;
use crate::{api, Context, GenError, GenResult};

/// One generation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub kind: Kind,
    pub provider: String,
    pub model: String,
    /// May be empty only when `params.audio_url` names the audio (avatar families).
    pub prompt: String,
    /// Provider and model options (aspect, duration, voice_id, image_urls, ...).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    /// Idempotency key: the same id always maps to the same job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Per-run consent to upload a local reference to the user's own fal storage (never stored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_fal_upload: Option<bool>,
    /// Scope ids (all three together), stored on the job and its assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
}

/// The venture, channel and account a request is made for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub venture: String,
    pub channel: String,
    pub account: String,
}

impl GenerateRequest {
    pub fn new(
        kind: Kind,
        provider: impl Into<String>,
        model: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            provider: provider.into(),
            model: model.into(),
            prompt: prompt.into(),
            params: Map::new(),
            request_id: None,
            confirm_fal_upload: None,
            scope: None,
        }
    }

    pub fn with_param(mut self, key: impl Into<String>, value: Value) -> Self {
        self.params.insert(key.into(), value);
        self
    }

    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }

    pub fn confirm_fal_upload(mut self, confirmed: bool) -> Self {
        self.confirm_fal_upload = Some(confirmed);
        self
    }

    /// The `generate` command input this request is.
    pub fn to_input(&self) -> Value {
        let mut v = json!({
            "kind": self.kind,
            "provider": self.provider,
            "model": self.model,
            "prompt": self.prompt,
            "params": Value::Object(self.params.clone()),
        });
        if let Some(id) = &self.request_id {
            v["request_id"] = json!(id);
        }
        if let Some(c) = self.confirm_fal_upload {
            v[crate::fal_storage::CONFIRM_FIELD] = json!(c);
        }
        if let Some(s) = &self.scope {
            v["venture"] = json!(s.venture);
            v["channel"] = json!(s.channel);
            v["account"] = json!(s.account);
        }
        v
    }
}

/// What `submit` returns: the job a request created or found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Submitted {
    pub job_id: String,
    pub status: String,
    #[serde(default)]
    pub est_usd: Option<f64>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// True when the request id already named a job and that job is returned.
    #[serde(default)]
    pub deduplicated: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A job as the store records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// `queued`, `submitted`, `running`, `complete`, `failed`, `cancelled` or `outcome_unknown`.
    pub status: String,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    pub est_usd: Option<f64>,
    #[serde(default)]
    pub actual_usd: Option<f64>,
    /// `{code, message, details?}` of a failed job.
    #[serde(default)]
    pub error: Option<GenError>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What `wait` returns: the job when the wait ended, and its result when it completed.
#[derive(Debug, Clone, PartialEq)]
pub struct Waited {
    pub job: JobRecord,
    pub timed_out: bool,
    pub result: Option<JobResult>,
}

/// The result descriptor of a completed job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobResult {
    pub job_id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub kind: String,
    pub assets: Vec<Asset>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One produced file in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub asset_id: String,
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub mime: String,
    /// Probed `{kind, width, height, duration_s}` when the format is known.
    #[serde(default)]
    pub media: Option<Value>,
    /// Origin, evidence and rights status.
    #[serde(default)]
    pub facts: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn decode<T: serde::de::DeserializeOwned>(what: &str, v: Value) -> GenResult<T> {
    serde_json::from_value(v).map_err(|e| {
        GenError::new(
            "bad_response",
            format!("{what} has an unexpected shape: {e}"),
        )
    })
}

/// The engine over one [`Context`].
#[derive(Debug, Clone)]
pub struct MediaGen {
    ctx: Context,
}

impl MediaGen {
    pub fn new(ctx: Context) -> Self {
        Self { ctx }
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Validate and price a request without creating a job (`generate` with `dry_run`).
    pub fn estimate(&self, request: &GenerateRequest) -> GenResult<Value> {
        api::generate(&self.ctx, &request.to_input(), true)
    }

    /// Create the job (or find the one its request id names) and reserve its estimate. The job
    /// runs in a detached runner the host engine starts, or in this process with [`Self::run`].
    pub fn submit(&self, request: &GenerateRequest) -> GenResult<Submitted> {
        decode(
            "generate",
            api::generate(&self.ctx, &request.to_input(), false)?,
        )
    }

    /// Drive the job to a terminal state in this process, holding its lock (status
    /// `locked_elsewhere` when another process holds it).
    pub fn run(&self, job_id: &str) -> GenResult<JobRecord> {
        decode(
            "job.run",
            api::job_run(&self.ctx, &json!({ "job_id": job_id }))?,
        )
    }

    pub fn status(&self, job_id: &str) -> GenResult<JobRecord> {
        decode(
            "job.status",
            api::job_status(&self.ctx, &json!({ "job_id": job_id }))?,
        )
    }

    /// Follow the job until it is terminal or `timeout_s` passes (`None` waits without bound).
    /// Never takes the job lock and never stops the job.
    pub fn wait(&self, job_id: &str, timeout_s: Option<f64>) -> GenResult<Waited> {
        let mut input = json!({ "job_id": job_id });
        if let Some(t) = timeout_s {
            input["timeout_s"] = json!(t);
        }
        let mut v = api::job_wait(&self.ctx, &input)?;
        let timed_out = v.get("timed_out").and_then(Value::as_bool).unwrap_or(false);
        let result = match v.as_object_mut().and_then(|o| o.remove("result")) {
            Some(r) if !r.is_null() => Some(decode("job.result", r)?),
            _ => None,
        };
        Ok(Waited {
            job: decode("job.wait", v)?,
            timed_out,
            result,
        })
    }

    /// The result descriptor; `not_ready` until the job is complete.
    pub fn result(&self, job_id: &str) -> GenResult<JobResult> {
        decode(
            "job.result",
            api::job_result(&self.ctx, &json!({ "job_id": job_id }))?,
        )
    }

    /// Cancel the job (see [`api::job_cancel`] for when a paid job is held instead).
    pub fn cancel(&self, job_id: &str) -> GenResult<Value> {
        api::job_cancel(&self.ctx, &json!({ "job_id": job_id }))
    }

    /// Record that `consumer` durably imported the result, so it may be cleaned up.
    pub fn ack(&self, job_id: &str, consumer: &str) -> GenResult<Value> {
        api::job_ack(
            &self.ctx,
            &json!({ "job_id": job_id, "consumer": consumer }),
        )
    }
}
