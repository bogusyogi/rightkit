//! The encrypted-store lifecycle (feature `encrypted-lifecycle`, which also needs `sqlcipher` or
//! `sqlcipher-system`).
//!
//! One owner for what apps used to hand-roll around SQLCipher: choosing the key that opens the file
//! (primary, pending, mapped legacy staging), rotating it, recovering from a rotation or restore
//! that stopped halfway, restoring a backup, and relocating the database to a new root, all behind
//! one cross-process guard.
//!
//! - [`ProtectedKeyStore`] is the contract rightkit-db defines for protected key storage.
//!   rightkit-credentials implements it in an adapter behind its own feature; rightkit-db never
//!   depends on rightkit-credentials (Cargo would reject the cycle), and never on rightkit-daemon.
//! - [`StoreTransition`] is the guard: bounded acquisition, `Busy` before any mutation, held until
//!   the effects settle, journaling both the old and the new root. Lock order: guard, then the key
//!   store's rotation lock, then the database operation.
//! - [`EncryptedStore::open`] selects the key and reconciles unfinished transitions. It never
//!   creates, quarantines or replaces a database; every failure is classified ([`LifecycleError`]).
//! - [`EncryptedStore::rotate_key`] returns a [`RotationReport`] classified from readback, never
//!   from an error value alone, and keeps every viable key durably until the database and key
//!   storage are proven consistent. A cleanup failure never rolls back a committed rotation.
//! - [`EncryptedStore::recover_rotation`] / [`recover_rotation`] decide from the real state, not
//!   from the journal.
//! - [`EncryptedStore::backup_to`], [`EncryptedStore::restore`] and [`EncryptedStore::relocate`]
//!   delegate to the crate's existing online backup, journaled restore and verified copy, with
//!   quiescence through the same guard.
//!
//! Keys are SQLCipher passphrase keys (the `DbOptions::key` charset); `PRAGMA rekey` has no raw-key
//! form here. A backup restored through [`EncryptedStore::restore`] must open with the store's
//! current key: a replacement under another key is refused as `WrongDestinationKey` before
//! anything changes.

mod error;
mod keystore;
mod relocate;
mod rotate;
mod store;
mod transition;

pub use error::LifecycleError;
pub use keystore::{
    Key, KeySource, KeyStoreError, KeyStoreErrorKind, PendingKey, PendingRevision,
    ProtectedKeySession, ProtectedKeyStore,
};
pub use relocate::{
    recover_aborted_relocation, recover_completed_relocation, recover_relocation,
    recover_relocation_session, RelocationPlan, RelocationRecovery, RelocationReport,
    RelocationSession, RelocationSettlement, RelocationSettlementReport,
};
pub use rotate::{
    recover_rotation, BlockedReason, Phase, RecoveryOutcome, RecoveryReport, RecoveryStep,
    RotationReport, RotationRequest,
};
#[doc(hidden)]
pub use rotate::{Faults, Killed, RekeyFault, RekeyStep};
pub use store::{EncryptedStore, EncryptedStoreConfig, HostQuiesce, ManagedPool, StoreValidate};
pub use transition::{RootIdentity, StoreTransition, TransitionKind, TransitionOptions};
