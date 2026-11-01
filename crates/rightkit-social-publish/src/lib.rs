//! `rightkit-social-publish`: platform publish adapters and their one-request HTTP layer.
//!
//! - [`platforms`]: request builders, payload checks and response readers for X, Bluesky,
//!   Mastodon, Threads, Instagram, Facebook, LinkedIn, Pinterest, TikTok and YouTube.
//! - [`http`]: a no-retry, no-redirect client, `Retry-After` and rate-limit-reset parsing, and the
//!   loopback-only base URL rule ([`http::EndpointSettings`] carries the host app's endpoints).
//! - [`media`] and [`platform`]: shared media checks and per-platform format specs.
//! - [`api_versions`]: pinned platform API versions and their sunset status, read from the sourced
//!   platform-rule catalog.
//! - [`redact`]: secret scrubbing for platform error text.
//! - [`wire`]: the post payload the adapters read.
//!
//! - [`protocol`]: the publish job, command inputs/outputs and the channel [`protocol::Scope`].
//! - [`quota`]: per-account pacing and the `publish.next` selector over a host-implemented
//!   [`quota::PublishQuotaStore`].
//! - [`calendar_time`]: UTC instants and local-time conversion for the supported zones.
//! - [`util`]: error constructors, input checks and the channel lookup the store-bound modules share.
//!
//! The durable job store and its schema belong to the host app; it implements the store traits.
//! Errors are [`Fail`]: a stable machine code plus a human message.

pub mod api_versions;
pub mod calendar_time;
pub mod http;
pub mod media;
pub mod platform;
pub mod platforms;
pub mod protocol;
pub mod quota;
pub mod redact;
pub mod util;
pub mod wire;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);

/// Standard FNV-1a 64 (rightkit-cache `fnv1a_64`): the id hash stored job ids and the Bluesky
/// record key are derived with, so ids stay stable across hosts.
pub fn fnv1a(text: &str) -> u64 {
    rightkit_cache::prefix::fnv1a_64(text)
}
