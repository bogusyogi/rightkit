//! Versioned per-app settings persistence. See README.md.

mod atomic;
mod backend;
mod options;
mod secret;
mod store;

pub use atomic::{atomic_write, atomic_write_with};
pub use backend::{Edit, FileBackend, KeyLayout, SettingsBackend, Stored};
pub use options::{
    DefaultsProvider, FileFormat, FlatMigration, KeyOrder, KeyValidator, NullMeans, OnCorrupt,
    QuarantineName, ReadOptions, ReplaceMode, SharingRetry, StoreOptions, Validator,
};
pub use rightkit_fs::PublicationDurability;
pub use secret::{Presence, SecretProbe, SecretRef};
pub use store::{
    read_with_defaults, read_with_options, Change, Error, Migration, PatchPreview, PreserveStep,
    Schema, Store, Subscription,
};
