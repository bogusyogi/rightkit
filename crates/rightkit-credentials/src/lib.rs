//! Per-app OS credential storage for Right Suite apps.
//!
//! * macOS Keychain / Windows Credential Manager / Linux Secret Service behind one [`Store`], per-app [`Namespace`].
//! * Passive [`Store::exists`]: never requests secret bytes, never prompts.
//! * Explicit [`Interaction`]: runtime calls are non-interactive; only a user action opts in.
//! * Test/dev guards (`RIGHTKIT_NO_KEYCHAIN` plus per-app variables) swap in memory or deny.
//! * [`Store::get_or_create_key`] provisions DB keys; [`migrate_between`] and friends move
//!   legacy items. [`Secret`] never prints.
//! * `apple_helper` drives the synced-credentials Swift helper (RightKitAppleCredentials).

mod backend;
mod error;
#[cfg_attr(not(windows), allow(dead_code))]
mod keyring_compat;
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
mod locked;
mod migrate;
mod namespace;
mod rotation;
mod secret;
mod store;

pub mod apple_helper;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
mod windows;

pub use backend::{
    system_backend, Backend, ConditionalDelete, Interaction, MemoryBackend, UnavailableBackend,
};
pub use error::{Error, Result};
pub use migrate::{
    migrate_between, migrate_plaintext_file, EnvImport, EnvMap, EnvReader, FileMigration,
    MigrationReport, Source,
};
pub use namespace::{
    macos_credential_identity, windows_credential_identity, Namespace, WindowsTarget,
};
pub use rotation::ROTATION_PENDING_SUFFIX;
pub use secret::Secret;
pub use store::{Config, Diagnostics, GuardMode, Store, WriteIntent, UNIVERSAL_GUARD_ENV};
