//! `rightkit-social-inbox`: the transport- and store-free core of a signed social webhook inbox.
//!
//! - [`webhook`]: HMAC-SHA256 signature verification over the raw body (constant time, one refusal
//!   for every failure), strict base64 body decoding under a size limit, app-secret handling, and
//!   parsing a verified Meta delivery into bounded events; event-time reading.
//! - [`keywords`]: whole-word keyword matching for comment-to-DM rules.
//!
//! - [`inbox`]: the store-bound commands (`inbox.receive`, `list`, `get`, `set_state`, `stats`) over a
//!   host-implemented [`inbox::InboxStore`].
//! - [`protocol`]: the inbox and DM-automation payload shapes.
//!
//! The tables and sending replies belong to the host app.

pub mod inbox;
pub mod keywords;
pub mod protocol;
pub mod webhook;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);
