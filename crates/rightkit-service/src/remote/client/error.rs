//! Typed client errors. Every refusal the daemon can return maps to an
//! [`ErrorCode`]; transport and local failures have their own variants.

use std::fmt;

use serde_json::Value;

/// The `error` code of a daemon refusal. Mirrors the codes the server returns
/// (and the daemon-api v1 error enum); anything else is [`ErrorCode::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// 401: missing, malformed, expired, replayed or badly signed credentials, an unknown or
    /// revoked device, or a bad or expired enrolment code.
    Unauthenticated,
    /// 403: the device lacks the scope the route needs (the role gate).
    Forbidden,
    /// 400: malformed body or credentials for this route.
    BadRequest,
    /// 404: unknown path, or an upload or job the device does not own.
    NotFound,
    MethodNotAllowed,
    BodyTooLarge,
    Internal,
    StorageError,
    /// 409 on enrol: this key is already enrolled.
    AlreadyEnrolled,
    /// 403 on `upload.init`: object larger than the daemon allows.
    TooLarge,
    /// 403 on `upload.init`: too many open sessions for this device.
    TooManyUploads,
    BadChunkSize,
    BadChunkIndex,
    BadChunkLength,
    ChunkTooLarge,
    ChunkDigestMismatch,
    /// 409: a different chunk body was already stored at this index.
    ChunkConflict,
    AlreadyComplete,
    Finalizing,
    /// 400 on `upload.complete`: the assembled object does not match the claimed digest. The
    /// server discards the session.
    HashMismatch,
    /// 400 on `upload.complete`: chunks are missing (see [`Refusal::body`] `missing`).
    Incomplete,
    /// 409 on `job.update`: `If-Match` did not equal the current revision.
    RevisionConflict,
    /// 428 on `job.update`: `If-Match` or `Idempotency-Key` missing.
    PreconditionRequired,
    /// 422 on `job.update`: the idempotency key was used for a different request.
    IdempotencyKeyReuse,
    DuplicateDelivery,
    RateLimited,
    OauthUnavailable,
    /// A code this client does not know (a newer daemon), or an error body without one.
    Other(String),
}

impl ErrorCode {
    pub fn from_code(code: &str) -> Self {
        match code {
            "unauthenticated" => Self::Unauthenticated,
            "forbidden" => Self::Forbidden,
            "bad_request" => Self::BadRequest,
            "not_found" => Self::NotFound,
            "method_not_allowed" => Self::MethodNotAllowed,
            "body_too_large" => Self::BodyTooLarge,
            "internal" => Self::Internal,
            "storage_error" => Self::StorageError,
            "already_enrolled" => Self::AlreadyEnrolled,
            "too_large" => Self::TooLarge,
            "too_many_uploads" => Self::TooManyUploads,
            "bad_chunk_size" => Self::BadChunkSize,
            "bad_chunk_index" => Self::BadChunkIndex,
            "bad_chunk_length" => Self::BadChunkLength,
            "chunk_too_large" => Self::ChunkTooLarge,
            "chunk_digest_mismatch" => Self::ChunkDigestMismatch,
            "chunk_conflict" => Self::ChunkConflict,
            "already_complete" => Self::AlreadyComplete,
            "finalizing" => Self::Finalizing,
            "hash_mismatch" => Self::HashMismatch,
            "incomplete" => Self::Incomplete,
            "revision_conflict" => Self::RevisionConflict,
            "precondition_required" => Self::PreconditionRequired,
            "idempotency_key_reuse" => Self::IdempotencyKeyReuse,
            "duplicate_delivery" => Self::DuplicateDelivery,
            "rate_limited" => Self::RateLimited,
            "oauth_unavailable" => Self::OauthUnavailable,
            other => Self::Other(other.to_string()),
        }
    }
}

/// A refusal from the daemon: any HTTP status of 300 or more.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub status: u16,
    pub code: ErrorCode,
    /// The parsed JSON body (`Null` when the body was empty or not JSON), so fields such as
    /// `currentRevision` and `missing` stay reachable.
    pub body: Value,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The daemon answered with a refusal.
    Refused(Refusal),
    /// TLS failed: the certificate does not chain to a configured root, name mismatch, protocol.
    Tls(String),
    /// Connect, read, write or timeout failure.
    Io(std::io::Error),
    /// The reply was not the HTTP/1.1 the daemon speaks, or exceeded a client bound.
    Protocol(String),
    /// The client was configured with no root certificate: it never falls back to system roots.
    NoRoots,
    /// A configured certificate or server name could not be used.
    Config(String),
    /// A key could not be generated or parsed.
    Key(String),
    /// The upload source does not match the session (size) or could not be read.
    Source(String),
    /// The daemon's reply was a success status with a body this client cannot read.
    BadReply(String),
}

impl Error {
    /// The refusal code, when this is a daemon refusal.
    pub fn code(&self) -> Option<&ErrorCode> {
        match self {
            Error::Refused(r) => Some(&r.code),
            _ => None,
        }
    }

    /// The HTTP status, when this is a daemon refusal.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Refused(r) => Some(r.status),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(r) => write!(f, "daemon refused with {} ({:?})", r.status, r.code),
            Error::Tls(e) => write!(f, "tls: {e}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Protocol(e) => write!(f, "protocol: {e}"),
            Error::NoRoots => f.write_str("no root certificate configured"),
            Error::Config(e) => write!(f, "config: {e}"),
            Error::Key(e) => write!(f, "key: {e}"),
            Error::Source(e) => write!(f, "upload source: {e}"),
            Error::BadReply(e) => write!(f, "unreadable reply: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
