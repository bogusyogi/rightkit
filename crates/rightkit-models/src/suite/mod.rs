//! The suite store: the on-disk contract every Orthic Labs app shares, ported
//! from ScrapeRight's `scraperight-model-store` (HeardRight consumes that crate
//! by git dependency; CutRight carries a copy) so those apps switch to
//! `rightkit-models` without migrating a byte of data.
//!
//! ```text
//! <suite-root>/models/<model-id>/<files>
//! <suite-root>/models/<model-id>/manifest.json   (id, version, files+sha256+size)
//! <suite-root>/models/refs/<app>.json            (model ids each app uses)
//! <suite-root>/models/<model-id>.lock            (cross-process op lock)
//! <suite-root>/models/.incoming/                 (resumable download scratch)
//! <suite-root>/models/.migration/<model-id>.json (write-ahead migration journal)
//! <suite-root>/runtime/model-use/<family>.lock   (runtime-family in-use lease)
//! <suite-root>/tools/<tool>/<version>/...        (same rules; tools store)
//! ```
//!
//! Suite root: Orthic Labs `shared` directory; `RIGHT_SUITE_DIR` (dev/test) or
//! `ORTHIC_SHARED_DIR` override it. Readiness requires the *trusted catalog*
//! manifest (compiled into the app, per-file SHA-256 + size pins) - a manifest
//! read from disk never defines its own identity.
//!
//! This is the flat, unversioned layout. The revisioned layout in
//! [`crate::store`] (`<root>/<id>/state.json` + `revisions/`) is independent;
//! a model id belongs to exactly one of the two. GC ([`store::prune_unref_entries`]
//! and [`crate::Store::gc`]) honours the same `refs/` registry and runtime leases
//! for both.

#![allow(clippy::too_many_arguments)]

pub mod archive;
pub mod catalog;
pub mod discover;
pub mod grant;
pub mod handle;
pub mod install;
pub mod lease;
pub mod manifest;
pub mod migration;
pub mod paths;
pub mod refs;
pub mod store;
pub mod variant;
pub mod whisper;

pub use crate::lock;
pub use catalog::{
    add_ons, all_models, base_catalog, catalog, ModelSpec, ModelTier, RemoteFile, SpecFile,
    SpecSource,
};
#[allow(deprecated)]
pub use grant::{acquire_grant, acquire_model_grant};
pub use grant::{
    acquire_grant_pinned, acquire_grant_with_client, acquire_model_grant_pinned, grant_client,
    grant_policy, parse_retry_after_header, try_acquire_grant_pinned,
    try_acquire_grant_with_client, GrantError, ModelGrant, ModelGrantRequest,
};
pub use handle::{ModelStore, ResolvedModel};
pub use install::{
    device_id_get_or_create, ensure_with_grant, ensure_with_grant_via, GrantConfig, GrantSession,
};
pub use manifest::{FileState, Manifest, ManifestFile, VerifyMode, VerifyReport};
pub use migration::{verify_source_checked, verify_trusted_source, JournalPhase, MigrationJournal};
pub use paths::{legacy_compatibility, legacy_suite_root, LegacyCompatibility};
pub use refs::{
    acquire_registries, migrate_root_refs, migrate_root_refs_with_map, unregister_across_roots,
};
#[allow(deprecated)]
pub use store::http_agent;
pub use store::{
    base_size_hint_bytes, base_status_at, ensure_ready, ensure_ready_cancellable, migrate_legacy,
    migrate_legacy_checked, migrate_source_checked, prune_unref_entries, prune_unref_entries_with,
    register_ref, resolve_ready_dir, set_status_at, size_hint_bytes, status_at, status_in,
    EnsureOptions, EntryState, MigrateOutcome, ModelStatus, Progress, SetStatus,
};
pub use variant::{parakeet_variant_descriptor, VariantDescriptor};
pub use whisper::{
    ensure_whisper_model, ensure_whisper_model_at, resolve_whisper_model, resolve_whisper_model_at,
    WhisperKind,
};
