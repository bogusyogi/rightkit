//! `rightkit-document-workspace` (Rust side): the persistent store behind a document viewer's
//! workspace: recent files, pins (files and explorer folders), per-document view state and the
//! open-tab session, with corruption quarantine and a purge of every stored path.
//!
//! The host app chooses the database path and passes it to [`Db::open_path`].
//! [`markdown_index`] indexes the Markdown files beside a document.

pub mod store;

pub use rightkit_db::Degraded;
pub use store::{Db, FileEntry, PinEntry, ViewState};

pub mod markdown_index;

/// A workspace operation failure, typed so the host maps it onto its own error wire shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceError {
    /// The operation would exceed a size or count cap.
    DocumentTooLarge(String),
    /// A filesystem error.
    Io(String),
    /// Anything else.
    Other(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DocumentTooLarge(m) | Self::Io(m) | Self::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for WorkspaceError {}
