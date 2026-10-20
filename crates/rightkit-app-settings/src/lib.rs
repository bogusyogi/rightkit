//! Versioned per-app settings persistence. See README.md.

mod atomic;
mod backend;
mod document;
mod options;
mod secret;
#[cfg(feature = "sql")]
mod sql;
mod store;

#[cfg(feature = "sql")]
pub use sql::{SqlError, SqlSettings};

pub use atomic::{atomic_write, atomic_write_with};
pub use backend::{Edit, FileBackend, KeyLayout, SettingsBackend, Stored};
pub use document::{Document, DocumentState};
pub use options::{
    DefaultsProvider, FileFormat, FlatMigration, KeyOrder, KeyValidator, NullMeans, OnCorrupt,
    OnRefused, QuarantineName, ReadOptions, ReplaceMode, SharingRetry, StoreOptions, Validator,
    WriteSecurity,
};
pub use rightkit_fs::PublicationDurability;
pub use secret::{Presence, SecretProbe, SecretRef};
pub use store::{
    read_with_defaults, read_with_options, Change, Error, Migration, PatchPreview, PreserveStep,
    Refusal, Schema, Store, Subscription,
};
