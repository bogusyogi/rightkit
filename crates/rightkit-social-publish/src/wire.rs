//! Wire shapes the platform adapters read: what one post carries. Serde-compatible with the
//! job payloads an app stores, so a stored job deserializes straight into these.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Platforms the adapters in [`crate::platforms`] drive.
pub const PUBLISH_PLATFORMS: [&str; 10] = [
    "instagram",
    "facebook",
    "threads",
    "x",
    "linkedin",
    "youtube",
    "bluesky",
    "mastodon",
    "tiktok",
    "pinterest",
];

/// One media item in a job payload. `url` is where the platform fetches the file; the object-storage
/// upload that produces it is not built, so it comes from the enqueue input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishMedia {
    pub hash: String,
    pub kind: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

/// What the job posts, frozen at enqueue from the approved calendar entry revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishPayload {
    pub caption: String,
    pub format: String,
    #[serde(default)]
    pub media: Vec<PublishMedia>,
    #[serde(default)]
    pub settings: Value,
    /// The follow-up comment posted after the post is live (#1497; see `comment`). Absent: none.
    #[serde(default)]
    pub first_comment: Option<String>,
}
