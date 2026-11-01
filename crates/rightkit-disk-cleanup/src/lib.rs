//! Disk usage scanning and safe cleanup: a bounded, symlink-safe filesystem scan with volume and
//! snapshot identity, cleanup rules with evidence, reclaimable-space findings, duplicate detection,
//! folder growth history, a crash-safe snapshot store and an applied-cleanup activity log.
//!
//! The platform layer (`platform`) reads native file and volume identity (statfs/getattrlistbulk on
//! macOS, `GetFileInformationByHandleEx` on Windows) without following links or reading file data.

pub mod activity;
pub mod cleanup;
pub mod cleanup_scan;
pub mod compression;
pub mod duplicates;
pub mod folder_growth;
pub mod history;
pub mod model;
pub mod platform;
pub mod rules;
pub mod scan;
pub mod snapshot;
pub mod storage_browser;
pub mod store;

pub use model::*;
pub use scan::{FilesystemProvider, StdFilesystemProvider, scan, scan_paths, scan_with_provider};
