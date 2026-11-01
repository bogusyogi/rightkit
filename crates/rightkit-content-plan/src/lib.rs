//! `rightkit-content-plan`: store-free content planning rules.
//!
//! - [`drafts`]: manual draft approval gate, first-sentence disclosure rule and the per-platform
//!   host table for posted links.
//!
//! - [`draft_store`]: the manual draft commands over a host-implemented [`draft_store::DraftStore`].
//! - [`protocol`]: the draft payload shapes.
//!
//! Briefs, calendar entries, variants and hook tests are stored by the host app.

pub mod draft_store;
pub mod drafts;
pub mod protocol;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);
