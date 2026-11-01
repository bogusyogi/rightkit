//! Disk-backed history stores: the JSON file store and, with the `sqlcipher` feature, the
//! encrypted SQLite store with its key rotation, legacy JSON migration and lost-key recovery.
//!
//! Everything the stores need from the application (data directory, logging, the credential
//! store holding the database key, sync consent) comes from [`crate::host::HistoryHost`].

mod atomic;
mod file_store;
mod retention;

pub use file_store::FileHistoryStore;

#[cfg(feature = "sqlcipher")]
mod key_rotation;
#[cfg(feature = "sqlcipher")]
mod legacy_json_migration;
#[cfg(feature = "sqlcipher")]
mod migrations;
#[cfg(feature = "sqlcipher")]
mod queries;
#[cfg(feature = "sqlcipher")]
mod recovery;
#[cfg(feature = "sqlcipher")]
mod sqlcipher_conn;
#[cfg(feature = "sqlcipher")]
mod sqlcipher_store;

#[cfg(feature = "sqlcipher")]
pub use key_rotation::rotate_with_for_test;
#[cfg(feature = "sqlcipher")]
pub use recovery::{
    finish_recovery_after_replay, history_recovery_diagnostics, history_recovery_status,
    recovery_pending_for_scope, reset_zone_change_token, test_consent_generation,
    zone_token_path_for_scope, HistoryRecoveryBackup, HistoryRecoveryStatus, ReplayReceipt,
};
#[cfg(feature = "sqlcipher")]
pub use sqlcipher_store::{
    open_connection_keyed_for_test, outcome_kind_str_for_test, validate_sqlcipher_key_for_pragma,
    SQLCipherHistoryStore,
};
