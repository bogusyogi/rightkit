//! Resident endpoint wire shapes. The resident serves one owner-only endpoint
//! (`rightkit-daemon`); each frame carries one request or one answer below.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RESIDENT_ENDPOINT_VERSION: u32 = 1;

/// `rightkit-daemon` service name (`<state>/runtime/<service>/service.json`)
/// and the app label every caller declares in its hello.
pub const RESIDENT_SERVICE: &str = "membrane";
pub const RESIDENT_APP: &str = "membrane";

/// Largest request or answer body, in bytes, either side will encode.
pub const RESIDENT_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The operations the resident answers. Each is also the
/// `rightkit-daemon` method name that the per-app allowlist admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentOp {
    Pull,
    Push,
    Blueprint,
    Status,
    Hook,
    /// Tier-scoped lexical memory search (`membrane memory search`).
    MemorySearch,
    /// Delete one memory by id (`membrane memory delete`).
    MemoryDelete,
    /// An Adapt command body (`membrane adapt recall|apply|taste-review`), answered as the
    /// operator route answers it: `{status, body}`.
    AdaptOperator,
    /// An observation request body (`membrane adapt observe`, `membrane transcript append`),
    /// answered as the observation route answers it: `{status, body}`.
    AdaptObservation,
    /// The committed end cursor of one session's transcript stream (`membrane transcript append`).
    TranscriptTip,
}

impl ResidentOp {
    pub const ALL: [ResidentOp; 10] = [
        ResidentOp::Pull,
        ResidentOp::Push,
        ResidentOp::Blueprint,
        ResidentOp::Status,
        ResidentOp::Hook,
        ResidentOp::MemorySearch,
        ResidentOp::MemoryDelete,
        ResidentOp::AdaptOperator,
        ResidentOp::AdaptObservation,
        ResidentOp::TranscriptTip,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ResidentOp::Pull => "pull",
            ResidentOp::Push => "push",
            ResidentOp::Blueprint => "blueprint",
            ResidentOp::Status => "status",
            ResidentOp::Hook => "hook",
            ResidentOp::MemorySearch => "memory_search",
            ResidentOp::MemoryDelete => "memory_delete",
            ResidentOp::AdaptOperator => "adapt_operator",
            ResidentOp::AdaptObservation => "adapt_observation",
            ResidentOp::TranscriptTip => "transcript_tip",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == name)
    }
}

/// `{v:1, op, args}`. `args` is the operation's argument object exactly as the
/// in-process path takes it: `pull` and `blueprint` carry the public caller
/// binding, `push` the memory write body, `hook` the raw host hook payload,
/// and `status` an empty object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidentRequestV1 {
    pub v: u32,
    pub op: ResidentOp,
    #[serde(default)]
    pub args: Value,
}

impl ResidentRequestV1 {
    pub fn new(op: ResidentOp, args: Value) -> Self {
        Self {
            v: RESIDENT_ENDPOINT_VERSION,
            op,
            args,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidentErrorV1 {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

/// `{v:1, ok, data}` on success, `{v:1, ok:false, error}` on failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidentAnswerV1 {
    pub v: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResidentErrorV1>,
}

impl ResidentAnswerV1 {
    pub fn success(data: Value) -> Self {
        Self {
            v: RESIDENT_ENDPOINT_VERSION,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn failure(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            v: RESIDENT_ENDPOINT_VERSION,
            ok: false,
            data: None,
            error: Some(ResidentErrorV1 {
                code: code.to_owned(),
                message: message.into(),
                retryable,
            }),
        }
    }
}

/// `resident.json` under the state root, written by `membrane serve` and
/// refreshed every 10 s. Timestamps are milliseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidentServiceFileV1 {
    pub pid: u32,
    /// Socket path (Mac) or pipe name (Windows) the endpoint is bound to.
    pub endpoint: String,
    pub generation: u64,
    pub started: u64,
    pub ts: u64,
}
