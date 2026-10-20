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

/// Classified native clock failure. Only `Unavailable` permits authenticated continuity.
#[derive(Debug)]
pub enum ClientClockError {
    File(FileClockStoreError),
    Unavailable,
    Refused,
    Busy,
    Corrupt,
    Unresolved,
}
impl std::fmt::Display for ClientClockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::File(_) => "clock file unavailable",
            Self::Unavailable => "clock temporarily unavailable",
            Self::Refused => "clock access refused",
            Self::Busy => "clock busy",
            Self::Corrupt => "clock corrupt",
            Self::Unresolved => "clock unresolved",
        })
    }
}
impl std::error::Error for ClientClockError {}
impl ClientClockError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Clone)]
enum ClockBackend {
    File(PolledClockStore),
    Credential {
        store: rightkit_credentials::Store,
        account: String,
        lock: super::lock::OpLock,
        wait: Duration,
    },
}

/// Opaque clock handle shared by native client & native engine.
#[derive(Clone)]
pub struct ClientClockStore {
    backend: ClockBackend,
}
impl ClientClockStore {
    pub(crate) fn from_config(cfg: &super::types::LicenseClientConfig) -> Self {
        let backend = match &cfg.credential_clock {
            None => ClockBackend::File(PolledClockStore::new(cfg.clock_path.clone())),
            Some((store, account)) => ClockBackend::Credential {
                store: store.clone(),
                account: account.clone(),
                lock: super::lock::OpLock::new(cfg.license_dir.join(".secure-clock.lock")),
                wait: cfg.persist_wait,
            },
        };
        Self { backend }
    }

    fn credential_op<R: Send + 'static>(
        &self,
        job: impl FnOnce(&rightkit_credentials::Store, &str) -> Result<R, ClientClockError>
            + Send
            + 'static,
    ) -> Result<R, ClientClockError> {
        use super::lock::{run_fenced_at, Fenced};
        let ClockBackend::Credential {
            store,
            account,
            lock,
            wait,
        } = &self.backend
        else {
            unreachable!()
        };
        let _level = order::enter(Level::SecureClock);
        let guard = lock
            .acquire(CLOCK_WAIT)
            .map_err(|_| ClientClockError::Busy)?;
        let (store, account) = (store.pinned().map_err(credential_error)?, account.clone());
        match run_fenced_at(guard, *wait, Level::SecureClock, move || {
            let result = job(&store, &account);
            // A timeout/probe failure never releases the serializing guard early.
            super::persist::settle(&store, &account);
            result
        }) {
            Fenced::Done(result, _guard) => result,
            Fenced::TimedOut | Fenced::Failed => Err(ClientClockError::Unresolved),
        }
    }
}

fn credential_error(error: rightkit_credentials::Error) -> ClientClockError {
    match super::reader::native_store_error(&error) {
        crate::NativeSourceError::Unavailable => ClientClockError::Unavailable,
        crate::NativeSourceError::Refused => ClientClockError::Refused,
    }
}

fn credential_read(
    store: &rightkit_credentials::Store,
    account: &str,
) -> Result<Option<ClockState>, ClientClockError> {
    let _rotation = order::enter(Level::Rotation);
    let status = store
        .rotation_status(account)
        .map_err(|_| ClientClockError::Unresolved)?;
    if status.held {
        return Err(ClientClockError::Unresolved);
    }
    if status.pending {
        // Clock rotation is one raw ledger value, never a licence record or a tombstone.
        let pending = store
            .pending_rotation(account)
            .map_err(credential_error)?
            .ok_or(ClientClockError::Unresolved)?;
        let text = pending
            .expose_str()
            .map_err(|_| ClientClockError::Corrupt)?;
        ClockState::decode(text).map_err(|_| ClientClockError::Corrupt)?;
        store
            .commit_rotation_of(account, &pending)
            .map_err(|_| ClientClockError::Unresolved)?;
        super::persist::settle(store, account);
        if !store
            .rotation_status(account)
            .map_err(|_| ClientClockError::Unresolved)?
            .is_idle()
        {
            return Err(ClientClockError::Unresolved);
        }
    }
    match store.get(account).map_err(credential_error)? {
        None => Ok(None),
        Some(secret) => {
            let text = secret.expose_str().map_err(|_| ClientClockError::Corrupt)?;
            ClockState::decode(text)
                .map(Some)
                .map_err(|_| ClientClockError::Corrupt)
        }
    }
}

fn credential_write(
    store: &rightkit_credentials::Store,
    account: &str,
    state: &ClockState,
) -> Result<(), ClientClockError> {
    let _rotation = order::enter(Level::Rotation);
    let secret = rightkit_credentials::Secret::from_bytes(state.encode().into_bytes());
    if !store
        .begin_rotation(account, &secret)
        .map_err(|_| ClientClockError::Unresolved)?
    {
        return Err(ClientClockError::Unresolved);
    }
    store
        .commit_rotation_of(account, &secret)
        .map_err(|_| ClientClockError::Unresolved)?;
    super::persist::settle(store, account);
    if !store
        .rotation_status(account)
        .map_err(|_| ClientClockError::Unresolved)?
        .is_idle()
    {
        return Err(ClientClockError::Unresolved);
    }
    Ok(())
}

impl ClockStore for ClientClockStore {
    type Error = ClientClockError;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        match &self.backend {
            ClockBackend::File(store) => store.load().map_err(ClientClockError::File),
            ClockBackend::Credential { .. } => self.credential_op(credential_read),
        }
    }
    fn save(&self, state: &ClockState) -> Result<(), Self::Error> {
        match &self.backend {
            ClockBackend::File(store) => store.save(state).map_err(ClientClockError::File),
            ClockBackend::Credential { .. } => {
                let state = state.clone();
                self.credential_op(move |store, account| {
                    credential_read(store, account)?;
                    credential_write(store, account, &state)
                })
            }
        }
    }
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        match &self.backend {
            ClockBackend::File(store) => store
                .compare_and_save(expected, new)
                .map_err(ClientClockError::File),
            ClockBackend::Credential { .. } => {
                let (expected, new) = (expected.cloned(), new.clone());
                self.credential_op(move |store, account| {
                    if credential_read(store, account)? != expected {
                        return Ok(false);
                    }
                    credential_write(store, account, &new)?;
                    Ok(true)
                })
            }
        }
    }
}
