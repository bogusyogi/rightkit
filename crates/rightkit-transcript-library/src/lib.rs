//! Transcript library for dictation apps.
//!
//! - [`delivery`]: the transcript record ([`delivery::DeliveryRecord`]): outcome, target
//!   snapshot, timings, source, plus pure record shaping and paste-attribution predicates. No
//!   clipboard, window or keystroke calls.
//! - [`history`]: the [`history::HistoryStore`] contract (filters, paging, retention, drafts,
//!   scoped updates and deletes by origin) and an in-memory store. Disk-backed stores implement
//!   the trait in the host.
//! - [`net`]: host of an absolute URL.
//! - [`export`]: JSON, Markdown, plain text and CSV exports with host-provided wording.
//! - `store` (feature `store`): keyed-connection history store. The host owns key custody,
//!   rotation and recovery.

pub mod delivery;
pub mod history;
pub mod net;

pub mod export;

/// Host-backed JSON fallback for transcript history.
pub mod file_store;

#[cfg(feature = "store")]
pub mod store;
