//! Request, profile and error shapes shared by the gate, handlers and the
//! future transport/CLI. Success is the bare result object; errors are
//! `{"error":{"code","message"}}`.

use super::access;
use serde::Deserialize;
use serde_json::{json, Value};

/// One agent request: the pairing key, a method name and its parameters.
#[derive(Clone, Deserialize)]
pub struct Request {
    pub key: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// The pairing key must never reach a log: `Debug` prints a placeholder and
/// omits params (they can carry search text).
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("key", &"<redacted>")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// Permission profile. Order is the privilege order. Read only from the
/// registry row, never from a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile {
    ReadOnly,
    Draft,
    SendWithApproval,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::ReadOnly => "read_only",
            Profile::Draft => "draft",
            Profile::SendWithApproval => "send_with_approval",
        }
    }
    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "read_only" => Some(Profile::ReadOnly),
            "draft" => Some(Profile::Draft),
            "send_with_approval" => Some(Profile::SendWithApproval),
            _ => None,
        }
    }
}

/// Why a request produced no result. `code()` strings are a stable contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    Unauthorised,
    AppLocked,
    ForbiddenProfile,
    AccountNotAllowed,
    BadRequest(String),
    /// An authorised effect (a draft) could not be completed.
    DraftFailed,
    /// Too many send requests, or too many waiting for the user.
    RateLimited,
    /// The user denied the approval, or it timed out (`user` | `timeout`).
    ApprovalDenied(&'static str),
    /// The draft changed after the user saw it; nothing was sent.
    DraftChanged,
    /// The approved send did not complete. The reason names why; for
    /// `unknown_outcome` the mail may have left.
    SendFailed(&'static str),
    /// The approved move to Trash did not complete. The reason names why; for
    /// `unknown_outcome` Gmail may have acted.
    TrashFailed(&'static str),
    /// A hard cap was reached (hourly thread changes, export size). The text
    /// is a fixed sentence; the code is `rate_limited`.
    Quota(&'static str),
    /// A label, archive or move did not complete. The reason names why.
    MutationFailed(&'static str),
    /// An attachment could not be fetched or exported. The reason names why.
    AttachmentFailed(&'static str),
    Internal,
}

impl AgentError {
    pub fn code(&self) -> &'static str {
        match self {
            AgentError::Unauthorised => "unauthorised",
            AgentError::AppLocked => "app_locked",
            AgentError::ForbiddenProfile => "forbidden_profile",
            AgentError::AccountNotAllowed => "account_not_allowed",
            AgentError::BadRequest(_) => "bad_request",
            AgentError::DraftFailed => "draft_failed",
            AgentError::RateLimited | AgentError::Quota(_) => "rate_limited",
            AgentError::MutationFailed(_) => "mutation_failed",
            AgentError::AttachmentFailed(_) => "attachment_failed",
            AgentError::ApprovalDenied(_) => "approval_denied",
            AgentError::DraftChanged => "draft_changed",
            AgentError::SendFailed(_) => "send_failed",
            AgentError::TrashFailed(_) => "trash_failed",
            AgentError::Internal => "internal",
        }
    }
    /// Fixed human text. Never carries mail content, keys or database detail.
    pub fn message(&self) -> String {
        match self {
            AgentError::Unauthorised => "Unknown or revoked pairing key.".into(),
            AgentError::AppLocked => format!("{} is locked.", crate::host::product()),
            AgentError::ForbiddenProfile => {
                "This agent's profile does not allow that method.".into()
            }
            AgentError::AccountNotAllowed => "This agent may not use that account.".into(),
            AgentError::BadRequest(m) => m.clone(),
            AgentError::DraftFailed => {
                "The draft could not be created. Check Gmail Drafts before trying again.".into()
            }
            AgentError::RateLimited => "Too many requests. Try again later.".into(),
            AgentError::Quota(m) => (*m).into(),
            AgentError::MutationFailed("no_credentials" | "busy") => {
                format!("The change was not made. Nothing was changed in Gmail or {}; it is safe to try again.", crate::host::product())
            }
            AgentError::MutationFailed(_) => {
                format!("The change did not finish and may be partly applied in Gmail. {} did not record it; the next sync shows the real state. Check before trying again.", crate::host::product())
            }
            AgentError::AttachmentFailed(_) => "The attachment could not be exported.".into(),
            AgentError::ApprovalDenied(_) => "The user did not approve this request.".into(),
            AgentError::DraftChanged => {
                "The draft changed after the user saw it. Nothing was sent.".into()
            }
            AgentError::SendFailed(_) => {
                "The send did not complete. Check the Sent folder in Gmail before trying again."
                    .into()
            }
            AgentError::TrashFailed("draft_changed") => {
                "The conversation changed after the user saw it. Nothing was moved to Trash.".into()
            }
            AgentError::TrashFailed(_) => {
                "The move to Trash did not complete. Check Trash in Gmail before trying again."
                    .into()
            }
            AgentError::Internal => "Internal error.".into(),
        }
    }
    /// A fixed reason code some refusals carry next to the message.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            AgentError::ApprovalDenied(r)
            | AgentError::SendFailed(r)
            | AgentError::TrashFailed(r)
            | AgentError::MutationFailed(r)
            | AgentError::AttachmentFailed(r) => Some(r),
            _ => None,
        }
    }
}

/// Map a data-layer error string to an [`AgentError`]: the locked text becomes
/// `app_locked`; anything else is `internal` (detail is dropped on purpose).
pub fn from_db_error(e: &str) -> AgentError {
    if e == access::LOCKED_MESSAGE {
        AgentError::AppLocked
    } else {
        AgentError::Internal
    }
}

/// The one serialiser the transport and CLI reuse.
pub fn to_json(outcome: &Result<Value, AgentError>) -> Value {
    match outcome {
        Ok(v) => v.clone(),
        Err(e) => {
            let mut err = json!({"code": e.code(), "message": e.message()});
            if let Some(r) = e.reason() {
                err["reason"] = json!(r);
            }
            json!({ "error": err })
        }
    }
}
