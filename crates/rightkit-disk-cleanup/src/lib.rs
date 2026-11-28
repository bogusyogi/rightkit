//! Disk usage scanning and safe cleanup: a bounded, symlink-safe filesystem scan with volume and
//! snapshot identity, cleanup rules with evidence, reclaimable-space findings, duplicate detection,
//! folder growth history, a crash-safe snapshot store and an applied-cleanup activity log.
//!
//! The platform layer (`platform`) reads native file and volume identity (statfs/getattrlistbulk on
//! macOS, `GetFileInformationByHandleEx` on Windows) without following links or reading file data.

pub mod activity;
pub mod cleanup;
pub mod cleanup_runs;
pub mod cleanup_scan;
pub mod compression;
pub mod duplicates;
#[cfg(windows)]
pub mod eject_windows;
pub mod folder_growth;
pub mod folder_index;
pub mod history;
pub mod item_actions;
pub mod managed_sources;
pub mod model;
pub mod platform;
pub mod refresh_schedule;
pub mod rules;
pub mod scan;
pub mod snapshot;
pub mod storage_browser;
pub mod store;
pub mod volumes;

pub use model::*;
pub use scan::{FilesystemProvider, StdFilesystemProvider, scan, scan_paths, scan_with_provider};
