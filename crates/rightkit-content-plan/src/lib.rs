//! `rightkit-content-plan`: store-free content planning rules.
//!
//! - [`drafts`]: manual draft approval gate, first-sentence disclosure rule and the per-platform
//!   host table for posted links.
//!
//! Briefs, calendar entries, variants and hook tests are stored by the host app.

pub mod drafts;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);
