//! Payload shapes for deletion requests. A request deletes what the host app holds for one
//! account on one platform: the stored rows, not the posts on the platform. A request is recorded with the
//! count of rows removed from each table, and a confirmation code a person or the Meta callback can look up.
//! Nothing here sends an email or calls a platform.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Scope;

/// Where a request came from. `deletion.request` takes `email` or `endpoint`; `meta_callback` is set only by
/// `deletion.meta_callback`.
pub const DELETION_SOURCES: [&str; 3] = ["email", "endpoint", "meta_callback"];

/// Request states. `completed`: at least one row was removed. `no_match`: the request matched no stored data.
pub const DELETION_STATES: [&str; 2] = ["completed", "no_match"];

/// The platforms whose accounts a Meta deletion callback matches (the Meta app covers all three).
pub const META_PLATFORMS: [&str; 3] = ["facebook", "instagram", "threads"];

/// `deletion.request` input. `request_id` makes the call idempotent: the same id with the same scope, platform and
/// source returns the stored record; a different one is `request_mismatch`. `requester_ref` is an optional short
/// reference the requester gave (for example a ticket number); do not put an email address or a secret in it.
#[derive(Debug, Clone, Deserialize)]
pub struct DeletionRequestInput {
    pub request_id: String,
    pub scope: Scope,
    pub platform: String,
    /// `email` or `endpoint`.
    pub source: String,
    #[serde(default)]
    pub requester_ref: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `deletion.meta_callback` input: the platform user id the daemon read from Meta's verified signed request. The
/// daemon verifies the signature; the engine matches the id to stored accounts by handle.
#[derive(Debug, Clone, Deserialize)]
pub struct DeletionMetaCallbackInput {
    pub user_id: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `deletion.status` input: either the request id or the confirmation code.
#[derive(Debug, Clone, Deserialize)]
pub struct DeletionStatusInput {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub confirmation_code: Option<String>,
}

/// One account the request covered, with the number of rows removed from each table (zero counts are listed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionTarget {
    pub scope: Scope,
    pub platform: String,
    pub deleted: BTreeMap<String, u64>,
}

/// One stored deletion request, as every deletion command returns it. `replayed` is true when the call found the
/// request already stored and changed nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionRecord {
    pub request_id: String,
    pub source: String,
    pub requester_ref: Option<String>,
    pub state: String,
    pub confirmation_code: String,
    pub requested_at: String,
    pub completed_at: String,
    pub targets: Vec<DeletionTarget>,
    pub replayed: bool,
}
