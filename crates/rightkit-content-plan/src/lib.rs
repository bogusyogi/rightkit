//! `rightkit-content-plan`: store-free content planning rules.
//!
//! - [`drafts`]: manual draft approval gate, first-sentence disclosure rule and the per-platform
//!   host table for posted links.
//!
//! - [`calendar`]: briefs and calendar entries (put, get, list, approve, reject, pause, resume,
//!   request approval, resolve failure) over a host-implemented [`calendar::CalendarStore`].
//! - [`variants`]: typed briefs and platform-native variants derived from them under a host-supplied
//!   rule catalog, over a host-implemented [`variants::VariantStore`].
//! - [`hook_test`]: N-opening hook tests decided from stored observations, over a host-implemented
//!   [`hook_test::HookTestStore`].
//! - [`draft_store`]: the manual draft commands over a host-implemented [`draft_store::DraftStore`].
//! - [`protocol`]: the draft payload shapes.
//!
//! The tables are the host app's.

pub mod calendar;
pub mod draft_store;
pub mod drafts;
pub mod hook_test;
pub mod protocol;
pub mod variants;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = (&'static str, String);
