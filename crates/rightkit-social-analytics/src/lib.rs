//! `rightkit-social-analytics`: store-free pieces of social campaign analytics.
//!
//! - [`links`]: tracked short links: public https destination checks, UTM tagging with defaults,
//!   deterministic link ids, the redirect route's id, target and referrer rules, and the campaign and
//!   link commands over a host-implemented [`links::LinksStore`].
//!
//! - [`metric_import`]: manual CSV metric import (preview, commit, list) over a host-implemented
//!   [`metric_import::MetricImportStore`].
//! - [`optimise`]: calendar change proposals (the `remake` rule) over a host-implemented
//!   [`optimise::OptimiseStore`].
//! - [`protocol`]: the analytics, metric-import and optimise payload shapes.
//!
//! The tables and pulling platform metrics belong to the host app.

pub mod links;
pub mod metric_import;
pub mod optimise;
pub mod protocol;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);
