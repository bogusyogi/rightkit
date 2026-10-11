//! File-producer reservations: a temp path a producer writes, then commit or abort.

use crate::disk::{remove_tmp, DiskCache, PutOutcome};
use std::io;
use std::path::{Path, PathBuf};

/// A reserved temp file for one cache slot.
///
/// Write the file at [`CacheReservation::path`], then call [`CacheReservation::commit`] to move
/// it into the cache atomically with size accounting. [`CacheReservation::abort`], or dropping
/// the reservation without committing, deletes the temp file and leaves the cache unchanged.
#[must_use = "a reservation that is neither committed nor aborted is removed on drop"]
pub struct CacheReservation {
    cache: DiskCache,
    name: String,
    ext: String,
    tmp: PathBuf,
    armed: bool,
}

impl CacheReservation {
    pub(crate) fn new(cache: DiskCache, name: String, ext: String, tmp: PathBuf) -> Self {
        Self {
            cache,
            name,
            ext,
            tmp,
            armed: true,
        }
    }

    /// Temp path for the producer. It ends in the reserved extension.
    pub fn path(&self) -> &Path {
        &self.tmp
    }

    /// Move the producer's file into the cache.
    ///
    /// Returns [`PutOutcome::Stored`] when the entry is in place, or [`PutOutcome::TooLarge`]
    /// when the file exceeds the cache's per-entry or total budget (the temp file is removed and
    /// existing entries are untouched). An empty or missing producer output is an error, so a
    /// failed encoder never leaves an empty cached file behind. After commit, a successful
    /// [`DiskCache::get_path`] returns the stored file.
    pub fn commit(mut self) -> io::Result<PutOutcome> {
        self.armed = false;
        let result = self.cache.install_file(&self.name, &self.ext, &self.tmp);
        if !matches!(result, Ok(PutOutcome::Stored)) {
            remove_tmp(&self.tmp);
        }
        result
    }

    /// Discard the producer's output and the reservation.
    pub fn abort(mut self) {
        self.armed = false;
        remove_tmp(&self.tmp);
    }
}

impl Drop for CacheReservation {
    fn drop(&mut self) {
        if self.armed {
            remove_tmp(&self.tmp);
        }
    }
}
