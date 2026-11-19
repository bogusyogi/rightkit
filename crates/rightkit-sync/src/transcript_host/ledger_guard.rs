//! The guard in front of every write of the engine ledger.
//!
//! The ledger file is bound to one account (`scope`) and holds that account's change cursor,
//! tombstones and queued deletions. This guard makes three things true for every write the engine
//! makes, and gives the host the means to prove a fourth before it hands the engine anything:
//!
//! 1. A write never replaces a ledger this process has not seen. Before the engine's bytes are
//!    written, the file on disk is read again (one no-follow handle, byte cap, streamed through a
//!    SHA-256) and compared with what this process last loaded or wrote (length and digest). A
//!    different file, a file that appeared or vanished, or an unreadable one refuses the write.
//!    The comparison, the write and the update of what was seen run under one in-process mutex and
//!    one cross-process advisory file lock (`<ledger>.lock`), so a second instance of the app that
//!    follows the same rule cannot interleave. A writer that does not take the lock can still
//!    race the window between the comparison and the rename; that is not closed here.
//! 2. A write never changes the account the ledger is bound to, unless the new account is the one
//!    the host established for the pass in flight (`set_expected`). The engine's bind, account
//!    reset and corruption recovery all arrive here as writes; an account switch the host did not
//!    establish (the live account moved after the host read it, or the host could not establish
//!    one) is refused. A bound ledger is never rewritten as an unbound one.
//! 3. After a refused or failed write the guard is marked diverged: the engine's cached copy no
//!    longer matches the disk, so the host drops the engine and rebuilds it from the file.
//! 4. [`LedgerGuard::check`] answers, under the same locks, whether the disk still equals this
//!    process's view and which account the DURABLE file is bound to. The host calls it before the
//!    first thing it writes into the ledger.
//!
//! What "generation" means here: the ledger has no counter, so the length and SHA-256 of the
//! file's bytes are its generation.
//!
//! The account is read from the first 4,096 bytes of the file (`scope` is the first field the
//! engine writes). A file that does not show its scope there reads as `ScopeState::Unknown`, which
//! equals no account, so nothing is handed to the engine until the engine's own next write has
//! rewritten the file in its own order.

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rightkit_fs::digest::Sha256Hasher;
use rightkit_fs::lock::FileLock;
use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};

use super::sync::{open_state_file, too_large, MAX_LEDGER_FILE_BYTES};

/// Bytes of the file's head searched for the `scope` field.
const HEAD_PROBE_BYTES: usize = 4096;
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// The account a ledger file is bound to, as far as its head shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ScopeState {
    /// `"scope"` is a string.
    Bound(String),
    /// No file, or `"scope"` is null or absent.
    Unbound,
    /// The head does not show the field or does not parse. Equals no account.
    Unknown,
}

struct ScopeProbe<'a>(&'a RefCell<Option<Option<String>>>);

impl<'de> Visitor<'de> for ScopeProbe<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a ledger")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "scope" {
                let value = map.next_value::<Option<String>>()?;
                *self.0.borrow_mut() = Some(value);
                // The rest of the document is not needed; stopping here is reported as an error
                // that the caller ignores.
                return Err(<A::Error as serde::de::Error>::custom("scope found"));
            }
            map.next_value::<IgnoredAny>()?;
        }
        Ok(())
    }
}

/// The scope shown by the head of a ledger file (at most `HEAD_PROBE_BYTES` are looked at).
pub(super) fn scope_of_head(head: &[u8]) -> ScopeState {
    let found: RefCell<Option<Option<String>>> = RefCell::new(None);
    let mut deserializer = serde_json::Deserializer::from_slice(head);
    let _ = (&mut deserializer).deserialize_map(ScopeProbe(&found));
    match found.into_inner() {
        Some(Some(scope)) => ScopeState::Bound(scope),
        Some(None) => ScopeState::Unbound,
        None => ScopeState::Unknown,
    }
}

/// Length, digest and account of one ledger file's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    len: u64,
    digest: [u8; 32],
    scope: ScopeState,
}

impl Seen {
    fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256Hasher::new();
        hasher.update(bytes);
        Self {
            len: bytes.len() as u64,
            digest: hasher.finalize_bytes(),
            scope: scope_of_head(&bytes[..bytes.len().min(HEAD_PROBE_BYTES)]),
        }
    }

    fn same_bytes(&self, other: &Seen) -> bool {
        self.len == other.len && self.digest == other.digest
    }
}

/// What this process last loaded or wrote.
enum Known {
    /// Nothing loaded yet: no write is allowed.
    Unloaded,
    /// The file did not exist.
    Absent,
    Present(Seen),
}

impl Known {
    fn matches(&self, disk: &Option<Seen>) -> bool {
        match (self, disk) {
            (Known::Absent, None) => true,
            (Known::Present(seen), Some(on_disk)) => seen.same_bytes(on_disk),
            _ => false,
        }
    }

    fn scope(&self) -> ScopeState {
        match self {
            Known::Present(seen) => seen.scope.clone(),
            Known::Absent => ScopeState::Unbound,
            Known::Unloaded => ScopeState::Unknown,
        }
    }
}

/// One streamed pass over the file: no-follow handle, byte cap on the handle and while reading,
/// digest of every byte, scope from the head. `Ok(None)`: no such file.
fn fingerprint_file(path: &Path) -> io::Result<Option<Seen>> {
    let file = match open_state_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_LEDGER_FILE_BYTES {
        return Err(too_large());
    }
    let mut limited = file.take(MAX_LEDGER_FILE_BYTES.saturating_add(1));
    let mut hasher = Sha256Hasher::new();
    let mut head: Vec<u8> = Vec::with_capacity(HEAD_PROBE_BYTES);
    let mut buffer = vec![0u8; READ_BUFFER_BYTES];
    let mut len: u64 = 0;
    loop {
        let read = limited.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        len = len.saturating_add(read as u64);
        if head.len() < HEAD_PROBE_BYTES {
            let room = HEAD_PROBE_BYTES - head.len();
            head.extend_from_slice(&buffer[..read.min(room)]);
        }
    }
    if len > MAX_LEDGER_FILE_BYTES {
        return Err(too_large());
    }
    Ok(Some(Seen {
        len,
        digest: hasher.finalize_bytes(),
        scope: scope_of_head(&head),
    }))
}

struct GuardState {
    known: Known,
    /// The account the host established for the pass in flight.
    expected: Option<String>,
    diverged: bool,
}

/// What [`LedgerGuard::check`] found.
pub(super) enum Disk {
    /// The disk equals this process's view; the file is bound as stated.
    Matches(ScopeState),
    /// The disk differs from this process's view, or an earlier write failed.
    Differs,
}

pub(super) struct LedgerGuard {
    path: PathBuf,
    state: Mutex<GuardState>,
}

fn poisoned() -> String {
    "cloud sync ledger guard poisoned".to_string()
}

impl LedgerGuard {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(GuardState {
                known: Known::Unloaded,
                expected: None,
                diverged: false,
            }),
        }
    }

    /// The cross-process lock. Errors carry no path.
    fn file_lock(&self) -> Result<FileLock, String> {
        crate::storage::ledger_file_lock(&self.path)
    }

    /// Record the bytes the engine just loaded (`None`: no file). Clears a divergence: the engine
    /// that asked has the disk's view.
    pub(super) fn record_loaded(&self, bytes: Option<&[u8]>) {
        if let Ok(mut state) = self.state.lock() {
            state.known = match bytes {
                None => Known::Absent,
                Some(bytes) => Known::Present(Seen::of_bytes(bytes)),
            };
            state.diverged = false;
        }
    }

    /// The account established for the pass in flight, or `None` when none was established (then
    /// no write may change the ledger's account).
    pub(super) fn set_expected(&self, scope: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.expected = scope;
        }
    }

    /// Whether the engine's view is stale: an earlier write was refused or failed.
    pub(super) fn is_diverged(&self) -> bool {
        self.state.lock().map_or(true, |state| state.diverged)
    }

    /// Compare the disk with this process's view, under the ledger locks.
    pub(super) fn check(&self) -> Result<Disk, String> {
        let state = self.state.lock().map_err(|_| poisoned())?;
        if state.diverged {
            return Ok(Disk::Differs);
        }
        let _lock = self.file_lock()?;
        let disk = fingerprint_file(&self.path)
            .map_err(|error| format!("read cloud sync ledger: {error}"))?;
        if state.known.matches(&disk) {
            Ok(Disk::Matches(
                disk.map_or(ScopeState::Unbound, |seen| seen.scope),
            ))
        } else {
            Ok(Disk::Differs)
        }
    }

    /// Replace the ledger with `bytes` through `write`, only if the disk still equals what this
    /// process last saw and the account does not change except to the one the host established.
    pub(super) fn replace(
        &self,
        bytes: &[u8],
        write: impl FnOnce(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        let _lock = self.file_lock()?;
        let disk = fingerprint_file(&self.path)
            .map_err(|error| format!("read cloud sync ledger: {error}"))?;
        if state.diverged || !state.known.matches(&disk) {
            state.diverged = true;
            return Err("cloud sync ledger changed since it was read; not replaced".to_string());
        }
        let current = state.known.scope();
        let next = Seen::of_bytes(bytes);
        if next.scope != current {
            let allowed = match (&current, &next.scope) {
                (ScopeState::Bound(_), ScopeState::Unbound | ScopeState::Unknown) => false,
                (_, ScopeState::Bound(to)) => state.expected.as_deref() == Some(to.as_str()),
                (_, ScopeState::Unbound) => true,
                (_, ScopeState::Unknown) => false,
            };
            if !allowed {
                return Err(
                    "cloud sync ledger account change refused: not the account established for this pass"
                        .to_string(),
                );
            }
        }
        match write(bytes) {
            Ok(()) => {
                state.known = Known::Present(next);
                Ok(())
            }
            Err(error) => {
                // The file may or may not have been replaced.
                state.diverged = true;
                Err(error)
            }
        }
    }
}
