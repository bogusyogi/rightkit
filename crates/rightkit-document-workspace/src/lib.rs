//! `rightkit-document-workspace` (Rust side): the persistent store behind a document viewer's
//! workspace: recent files, pins (files and explorer folders), per-document view state and the
//! open-tab session, with corruption quarantine and a purge of every stored path.
//!
//! The host app chooses the database path and passes it to [`Db::open_path`].
//!
//! It also holds the command bodies a viewer app wraps in its IPC layer: [`library`] (recent
//! files, pins, view state, session), [`document_io`] (read, write, copy, sibling images, file
//! watching), [`explorer`] (locations, directory listings, folder watches),
//! [`markdown_resources`] (reference inspection and pasted assets) and [`settings`] (the durable
//! settings document). The functions are synchronous and return [`WorkspaceError`]. The host
//! supplies three seams: [`PathGrants`] (which paths the webview may read or write),
//! [`DocumentBackend`] (format classification and document IO) and [`WorkspaceWatcher`] (OS
//! watchers). It runs the functions on its blocking pool and keeps its IPC wrappers.

#[cfg(feature = "app-paths")]
pub mod app_paths;
#[cfg(feature = "tauri")]
pub mod commands;
pub mod document_io;
pub mod documents;
pub mod error;
pub mod explorer;
pub mod grants;
pub mod library;
pub mod markdown_index;
pub mod markdown_resources;
pub mod persistence_status;
pub mod settings;
pub mod store;
pub mod watch;

pub use documents::{DocumentBackend, FileFingerprint, RawDocument, WriteDocumentResult};
pub use error::{broker_error, WorkspaceError};
pub use grants::{authorize_existing_for_read, PathGrants};
pub use rightkit_db::Degraded;
pub use store::{Db, FileEntry, PinEntry, ViewState};
pub use watch::WorkspaceWatcher;
