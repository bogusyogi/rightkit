//! The anti-rollback ledger store of the native client.
//!
//! The same file and the same sidecar lock as [`FileClockStore`], so an engine that opens the
//! ledger with a `FileClockStore` on the same path interoperates. The difference is the wait:
//! this store never blocks on the OS lock. It polls `try_acquire` against a short deadline and
//! reports a held lock as `io::ErrorKind::WouldBlock`, which the client turns into `Busy`.
//! The clock is the LAST lock in the fixed order (operation, rotation, clock).

use std::io;
use std::time::{Duration, Instant};

use super::lock::pause;
use super::order::{self, Level};
use crate::{ClockState, ClockStore, CrossProcessLock, FileClockStore, FileClockStoreError};

/// How long one clock operation waits for the ledger lock. It is held for one read-compare-write
/// of a few bytes, so a longer wait means a stuck holder.
const CLOCK_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub(crate) struct PolledClockStore {
    inner: FileClockStore,
    wait: Duration,
}

impl PolledClockStore {
    pub(crate) fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            inner: FileClockStore::new(path),
            wait: CLOCK_WAIT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    fn lock(&self, exclusive: bool) -> Result<CrossProcessLock, FileClockStoreError> {
        let path = self.inner.lock_path();
        let now = Instant::now();
        let deadline = now.checked_add(self.wait).unwrap_or(now);
        loop {
            let tried = if exclusive {
                CrossProcessLock::try_acquire(&path)
            } else {
                CrossProcessLock::try_acquire_shared(&path)
            };
            match tried {
                Ok(Some(lock)) => return Ok(lock),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return Err(FileClockStoreError::Io(io::Error::from(
                            io::ErrorKind::WouldBlock,
                        )));
                    }
                    pause(deadline);
                }
                Err(error) => return Err(FileClockStoreError::Io(error)),
            }
        }
    }
}

impl ClockStore for PolledClockStore {
    type Error = FileClockStoreError;

    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        let _level = order::enter(Level::Clock);
        let _lock = self.lock(false)?;
        self.inner.read_unlocked()
    }

    fn save(&self, state: &ClockState) -> Result<(), Self::Error> {
        let _level = order::enter(Level::Clock);
        let _lock = self.lock(true)?;
        self.inner.write_unlocked(state)
    }

    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        let _level = order::enter(Level::Clock);
        let _lock = self.lock(true)?;
        if self.inner.read_unlocked()?.as_ref() != expected {
            return Ok(false);
        }
        self.inner.write_unlocked(new)?;
        Ok(true)
    }
}

/// Whether a clock-store failure is only "the ledger lock is held".
pub(crate) fn is_lock_held(error: &FileClockStoreError) -> bool {
    matches!(error, FileClockStoreError::Io(e) if e.kind() == io::ErrorKind::WouldBlock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(mark: i64, iat: i64) -> ClockState {
        ClockState {
            highest_seen: mark,
            trusted_process: "p".into(),
            last_server_iat: iat,
        }
    }

    #[test]
    fn it_reads_and_writes_the_same_file_as_a_file_clock_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock.json");
        let polled = PolledClockStore::new(&path);
        assert_eq!(polled.load().unwrap(), None);
        assert!(polled.compare_and_save(None, &state(10, 5)).unwrap());
        assert!(!polled.compare_and_save(None, &state(11, 6)).unwrap());
        let plain = FileClockStore::new(&path);
        assert_eq!(plain.load().unwrap(), Some(state(10, 5)));
        plain.save(&state(20, 7)).unwrap();
        assert_eq!(polled.load().unwrap(), Some(state(20, 7)));
    }

    #[test]
    fn a_held_ledger_lock_is_would_block_after_the_wait_not_a_hang() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock.json");
        let polled = PolledClockStore::new(&path).with_wait(Duration::from_millis(40));
        let held = CrossProcessLock::try_acquire(FileClockStore::new(&path).lock_path())
            .unwrap()
            .unwrap();
        let started = Instant::now();
        let error = polled.load().unwrap_err();
        assert!(is_lock_held(&error), "{error}");
        assert!(polled.save(&state(1, 1)).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(held);
        assert!(polled.save(&state(1, 1)).is_ok());
    }
}
