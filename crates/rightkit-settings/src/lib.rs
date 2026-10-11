//! Versioned per-app settings persistence. See README.md.

mod atomic;
mod backend;
mod options;
mod secret;
mod store;

pub use atomic::{atomic_write, atomic_write_with};
pub use backend::{Edit, FileBackend, SettingsBackend, Stored};
pub use options::{
    DefaultsProvider, FileFormat, FlatMigration, KeyValidator, NullMeans, OnCorrupt,
    QuarantineName, ReadOptions, ReplaceMode, SharingRetry, StoreOptions, Validator,
};
pub use rightkit_fs::PublicationDurability;
pub use secret::{Presence, SecretProbe, SecretRef};
pub use store::{
    read_with_defaults, read_with_options, Change, Error, InterruptedReplace, Migration,
    PatchPreview, Schema, Store, Subscription,
};
