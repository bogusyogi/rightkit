//! Transcript library for dictation apps.
//!
//! - [`delivery`]: the transcript record ([`delivery::DeliveryRecord`]): outcome, target
//!   snapshot, timings, source, plus pure record shaping and paste-attribution predicates. No
//!   clipboard, window or keystroke calls.
//! - [`history`]: the [`history::HistoryStore`] contract (filters, paging, retention, drafts,
//!   scoped updates and deletes by origin) and an in-memory store. Disk-backed stores implement
//!   the trait in the host.
//! - [`store`]: the JSON file history store and, with the `sqlcipher` feature, the encrypted store;
//!   [`host`] is the application seam they use.
//! - [`net`]: host of an absolute URL.

pub mod delivery;
pub mod history;
pub mod host;
pub mod net;
pub mod store;
