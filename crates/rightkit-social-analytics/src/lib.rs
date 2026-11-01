//! `rightkit-social-analytics`: store-free pieces of social campaign analytics.
//!
//! - [`links`]: tracked short links: public https destination checks, UTM tagging with defaults,
//!   deterministic link ids, and the redirect route's id, target and referrer rules.
//!
//! With the `engine-commands` feature, `cmd` adds the retention feedback handlers of a video engine.
//!
//! Storing campaigns, links and visits and pulling platform metrics belong to the host app.

#[cfg(feature = "engine-commands")]
pub mod cmd;
pub mod links;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);
