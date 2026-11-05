//! Local semantic search (DISPATCH.md Lane C5): meaning-based search over
//! the local mail archive, fully local and deterministic — EmbeddingGemma
//! vectors in sqlite-vec inside the SAME SQLCipher DB as everything else.
//! FTS5 stays the default/primary engine; semantic is an additive rerank/
//! recall layer (see query.rs's hybrid path), never a replacement.
//!
//! Module map:
//! - `vec_ext`   — static sqlite-vec extension registration.
//! - `embedder`  — the `Embedder` trait + the bundled-resource FastEmbedder.
//! - `resolve`   — locates the installed/bundled model dir + onnxruntime dylib.
//! - `model_install` — EmbeddingGemma bundle install via rightkit-models.
//! - `storage`   — the vec0 table + per-account index-progress bookkeeping.
//! - `index`     — the resumable background embedding job.
//! - `query`     — the hybrid FTS-candidates + vector-rerank search path.
//!
//! Settings keys (stored in the existing `settings` key/value table, same
//! pattern as every other feature flag in this codebase):
//!   - `semantic_enabled`            "1"/"0" — Pro-gated master switch.
//!   - `semantic_embed_model_id`     last model id an account was indexed
//!     under (test-run override via the `EMBED_MODEL` selector; ship default is
//!     embeddinggemma2-q4).

pub mod embedder;
use rightkit_mail::fusion;
pub mod index;
mod index_commit;
pub(crate) mod invalidate;
pub mod model_install;
pub mod query;
mod query_scope;
pub mod resolve;
pub mod status;
pub mod storage;
pub mod vec_ext;

use rightkit_db::rusqlite::Connection;
use std::sync::{Arc, OnceLock};

pub use embedder::FastEmbedder;

/// Settings key: master on/off switch, written by the `set_semantic_enabled`
/// command (commands/semantic.rs), which refuses "1" without Pro. The index
/// job and the query path both check it (and Pro) before doing any work, so a
/// free user pays zero CPU/disk for this feature.
pub const SETTING_ENABLED: &str = "semantic_enabled";

/// Which embed model id is configured for this install. Empty/unset resolves
/// to the shipped default (`DEFAULT_MODEL_ID`). Overridable in a test run
/// only (`model_selector`) — the setting itself is what
/// `needs_reindex` compares against per account.
pub const SETTING_MODEL_ID: &str = "semantic_embed_model_id";

/// Shipped default model id. The resource layout is produced by
/// `scripts/fetch-embed-model.mjs`, whose `MODEL_ID` is `embeddinggemma2-q4`
/// (and `rightkit_embed::EMBEDDINGGEMMA2_MODEL_ID`, which the embedder reports
/// for that bundle); the older `embeddinggemma-300m-q4` id is never produced
/// by the build.
pub const DEFAULT_MODEL_ID: &str = "embeddinggemma2-q4";

pub fn is_enabled(conn: &Connection) -> bool {
    crate::db::get_setting(conn, SETTING_ENABLED)
        .ok()
        .flatten()
        .map(|v| v == "1")
        .unwrap_or(false)
}

pub fn set_enabled(conn: &Connection, enabled: bool) -> Result<(), String> {
    crate::db::set_setting(conn, SETTING_ENABLED, if enabled { "1" } else { "0" })
}

/// The `EMBED_MODEL` selector, read through `MailStoreHost::test_selector`: it has a value only
/// in a test run.
fn model_selector() -> Option<String> {
    crate::host::host()
        .test_selector("EMBED_MODEL")
        .map(|v| v.trim().to_string())
}

/// Resolve the configured model id: the test-run selector wins, else the persisted setting, else
/// the shipped default.
pub fn configured_model_id(conn: &Connection) -> String {
    configured_model_id_with(conn, model_selector())
}

/// [`configured_model_id`] with the selector value injected (tests).
fn configured_model_id_with(conn: &Connection, selector: Option<String>) -> String {
    if let Some(id) = selector.filter(|v| !v.is_empty()) {
        return id;
    }
    crate::db::get_setting(conn, SETTING_MODEL_ID)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL_ID.to_string())
}

/// Process-wide embedder singleton. Loading the ONNX model is expensive
/// (hundreds of ms to seconds); every caller (index job, query path) shares
/// one instance instead of reloading per call. `OnceLock` because the model
/// choice is fixed for the process lifetime (a model change requires an app
/// restart to pick up cleanly, same as any other "settings that gate a
/// loaded native resource" pattern in this codebase — e.g. the DB key).
static EMBEDDER: OnceLock<Result<Arc<FastEmbedder>, String>> = OnceLock::new();

/// Get (or lazily construct) the shared embedder. `resource_base` is the
/// Tauri resource dir (`app.path().resource_dir()`), passed on first call
/// only — later calls reuse the cached instance/error regardless of the
/// argument, since construction only happens once per process.
///
/// FAIL-LOUD (per the C5 spec): a missing/corrupt bundled resource is a
/// clear `Err` describing exactly which file is missing, never a silent
/// fallback to a different embedding space. Callers (index job, query
/// command) surface this error to the UI as "semantic search unavailable"
/// rather than degrading search results without telling the user.
pub fn shared_embedder(
    resource_base: Option<&std::path::Path>,
    model_id: &str,
) -> Result<Arc<FastEmbedder>, String> {
    // Resolution is cheap and re-run on every call so a model installed after
    // a failed first attempt is picked up without a restart; only the (heavy)
    // load result is cached for the process.
    let dir = resolve::locate_model(&model_install::default_store(), model_id, resource_base)
        .map(|(dir, _source)| dir)?;
    resolve::verify_model_files_present(&dir, model_id)?;
    let dylib = resolve::onnxruntime_dylib_path(resource_base)?;
    EMBEDDER
        .get_or_init(|| FastEmbedder::load(&dir, &dylib).map(Arc::new))
        .clone()
}
