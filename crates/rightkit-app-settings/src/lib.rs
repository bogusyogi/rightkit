//! Versioned per-app settings persistence. See README.md.

mod atomic;
mod backend;
#[cfg(feature = "credentials-probe")]
mod credentials_probe;
mod document;
mod handle;
mod hub_commands;
mod manifest;
mod options;
mod published;
mod request;
mod secret;
#[cfg(feature = "sql")]
mod sql;
mod store;

#[cfg(feature = "sql")]
pub use sql::{SqlError, SqlSettings};

pub use atomic::{atomic_write, atomic_write_with};
pub use backend::{Edit, FileBackend, KeyLayout, SettingsBackend, Stored};
pub use document::{Document, DocumentState};
pub use handle::{
    error_tag, EventSink, HandleConfig, StoreHandle, EVENT_LOCK_ERROR, EVENT_UNREADABLE_AT_LOAD,
    MAX_OPEN_ATTEMPTS, MAX_OPEN_RETRY_DELAY, REVISION_CONFLICT_PREFIX,
};
pub use hub_commands::{
    close_matches, edit_distance, has_duplicate_keys, parse_hub_command, parse_value_like,
    same_value, show_value, wait_for_setting, wake_owner, CommandArg, CommandFolder, CommandLimits,
    HubCommand, SendError,
};
pub use manifest::{Kind, Manifest, ManifestError, Mismatch, Setting, Target};
pub use options::{
    migrated_default, DefaultsProvider, FileFormat, FlatMigration, KeyOrder, KeyValidator,
    NullMeans, OnCorrupt, OnRefused, QuarantineName, ReadOptions, ReplaceMode, SharingRetry,
    StoreOptions, Validator, WriteSecurity,
};
pub use published::{
    document_snapshot, load_frontend_document, open_frontend_document, open_published_state,
    patch_frontend_document, read_published_state, read_published_state_of,
    read_published_state_strict, PUBLISHED_MAX_BYTES,
};
pub use request::{ChangeRequest, ChangeRequestQueue, RequestOutcome, RequestPolicy};
pub use rightkit_fs::PublicationDurability;
pub use secret::{Presence, SecretProbe, SecretRef};
pub use store::{
    read_with_defaults, read_with_options, Change, Error, Migration, PatchPreview, PreserveStep,
    Refusal, Schema, Store, Subscription,
};
