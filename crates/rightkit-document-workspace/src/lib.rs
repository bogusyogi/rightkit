//! `rightkit-document-workspace` (Rust side): the persistent store behind a document viewer's
//! workspace: recent files, pins (files and explorer folders), per-document view state and the
//! open-tab session, with corruption quarantine and a purge of every stored path.
//!
//! The host app chooses the database path and passes it to [`Db::open_path`].

pub mod store;

pub use rightkit_db::Degraded;
pub use store::{Db, FileEntry, PinEntry, ViewState};
