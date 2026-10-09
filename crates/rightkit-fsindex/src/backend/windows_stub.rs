//! Stub backend for platforms without an implementation (Windows today).
//!
//! The planned Windows backend enumerates the NTFS MFT for the initial
//! crawl, follows the USN journal for changes, and resumes from a stored
//! (journal id, USN). See the README. Every call here reports Unsupported.

use super::*;
use crate::error::Error;
use std::path::PathBuf;
use std::time::Duration;

pub struct UnsupportedBackend;

fn unsupported() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no file-index backend on this platform",
    )
}

impl Backend for UnsupportedBackend {
    fn name(&self) -> &'static str {
        "unsupported"
    }
    fn check_supported(&self) -> Result<()> {
        Err(Error::Unsupported("no file-index backend on this platform"))
    }
    fn stat(&self, _: &Path) -> std::io::Result<EntryData> {
        Err(unsupported())
    }
    fn crawl(&self, _: &Path, _: &Excludes, _: usize) -> Vec<Listing> {
        vec![Listing {
            error: Some(std::io::ErrorKind::Unsupported),
            ..Listing::default()
        }]
    }
    fn list_dir(&self, _: &Path, _: &Excludes) -> std::io::Result<Listing> {
        Err(unsupported())
    }
    fn watch(
        &self,
        _: &[PathBuf],
        _: Option<u64>,
        _: Duration,
        _: Sender<WatchMsg>,
    ) -> Result<Box<dyn Watcher>> {
        Err(Error::Unsupported("no change feed on this platform"))
    }
    fn current_event_id(&self) -> u64 {
        0
    }
    fn has_full_disk_access(&self) -> bool {
        false
    }
}
