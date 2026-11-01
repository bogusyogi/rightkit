//! `rightkit-agent-engine-history` — file-edit history + unified diffs for the edit tools.

mod diff;
pub mod session_package;
mod store;

pub use diff::unified_diff;
pub use store::HistoryStore;
