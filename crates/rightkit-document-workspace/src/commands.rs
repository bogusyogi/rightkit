//! Asynchronous command bodies for a Tauri document viewer.
//!
//! Every function here is the body of one IPC command: it runs the synchronous workspace
//! function of the same name on Tauri's blocking pool and answers [`WorkspaceError`]. The app
//! keeps a one-line `#[tauri::command]` shim per command that resolves its state, builds a
//! [`WorkspaceHost`] handle and calls in. Nothing here names an app: the app supplies its
//! database, path grants, document backend, watcher, settings store and display name through the
//! [`WorkspaceHost`] trait.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::document_io::{self, WritePayload};
use crate::documents::{DocumentBackend, FileFingerprint, RawDocument, WriteDocumentResult};
use crate::error::{broker_error, WorkspaceError};
use crate::explorer::{self, ExplorerListing, ExplorerLocation, HomeLocation};
use crate::grants::PathGrants;
use crate::library::{self, RecentEntry};
use crate::markdown_index::{self, IndexedMarkdownFolderResult};
use crate::markdown_resources::{
    self, CommitMarkdownAssetRequest, MarkdownAssetTransactions, MarkdownResourceRequest,
    MarkdownResourceResult, RollbackMarkdownAssetRequest, SaveMarkdownAssetRequest,
    SavedMarkdownAsset,
};
use crate::persistence_status::{status_of, PersistenceStatus};
use crate::settings::SettingsStore;
use crate::store::{Db, PinEntry};
use crate::watch::WorkspaceWatcher;

/// What the commands need from the app. Implement it on the app's managed state.
pub trait WorkspaceHost: Send + Sync + 'static {
    /// The workspace database.
    fn db(&self) -> &Db;
    /// The path-grant broker.
    fn grants(&self) -> &dyn PathGrants;
    /// Document format classification and IO.
    fn backend(&self) -> &dyn DocumentBackend;
    /// The OS watcher registry.
    fn watcher(&self) -> &dyn WorkspaceWatcher;
    /// The durable settings store.
    fn settings(&self) -> &SettingsStore;
    /// Pending pasted-asset transactions.
    fn transactions(&self) -> &MarkdownAssetTransactions;
    /// The app's display name, used in user-facing error text.
    fn app_name(&self) -> &str;
    /// Home folder and whether it is an isolated stand-in, for the sidebar locations.
    fn home(&self) -> HomeLocation;
    /// How the database open recovered, `None` for a normal open.
    fn degraded(&self) -> Option<&rightkit_db::Degraded>;
}

async fn blocking<T, F>(task: F) -> Result<T, WorkspaceError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, WorkspaceError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|e| WorkspaceError::Other(e.to_string()))?
}

/// `list_recent`.
pub async fn list_recent<H: WorkspaceHost>(
    host: Arc<H>,
    limit: Option<i64>,
) -> Result<Vec<RecentEntry>, WorkspaceError> {
    blocking(move || library::list_recent(host.db(), host.grants(), limit)).await
}

/// `clear_recent`.
pub async fn clear_recent<H: WorkspaceHost>(host: Arc<H>) -> Result<(), WorkspaceError> {
    blocking(move || library::clear_recent(host.db())).await
}

/// `purge_user_paths`.
pub async fn purge_user_paths<H: WorkspaceHost>(host: Arc<H>) -> Result<(), WorkspaceError> {
    blocking(move || library::purge_user_paths(host.db())).await
}

/// `is_pinned_path`.
pub async fn is_pinned_path<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
) -> Result<bool, WorkspaceError> {
    blocking(move || library::is_pinned_path(host.db(), host.grants(), &path)).await
}

/// `toggle_pin`.
pub async fn toggle_pin<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
    display_name: String,
) -> Result<bool, WorkspaceError> {
    blocking(move || {
        library::toggle_pin(
            host.db(),
            host.grants(),
            host.backend(),
            &path,
            &display_name,
        )
    })
    .await
}

/// `list_pins`.
pub async fn list_pins<H: WorkspaceHost>(host: Arc<H>) -> Result<Vec<PinEntry>, WorkspaceError> {
    blocking(move || library::list_pins(host.db(), host.grants())).await
}

/// `toggle_pin_dir`.
pub async fn toggle_pin_dir<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
    display_name: String,
) -> Result<bool, WorkspaceError> {
    blocking(move || {
        library::toggle_pin_dir(
            host.db(),
            host.grants(),
            host.app_name(),
            &path,
            &display_name,
        )
    })
    .await
}

/// `record_recent`.
pub async fn record_recent<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
    display_name: String,
) -> Result<(), WorkspaceError> {
    blocking(move || {
        library::record_recent(
            host.db(),
            host.grants(),
            host.backend(),
            &path,
            &display_name,
        )
    })
    .await
}

/// `save_view_state`.
pub async fn save_view_state<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
    last_view: String,
    view_meta: Option<String>,
) -> Result<(), WorkspaceError> {
    blocking(move || {
        library::save_view_state(
            host.db(),
            host.grants(),
            host.backend(),
            &path,
            &last_view,
            view_meta.as_deref(),
        )
    })
    .await
}

/// `save_session`.
pub async fn save_session<H: WorkspaceHost>(
    host: Arc<H>,
    tabs: Vec<(String, bool)>,
) -> Result<(), WorkspaceError> {
    blocking(move || library::save_session(host.db(), host.grants(), tabs)).await
}

/// `load_session`.
pub async fn load_session<H: WorkspaceHost>(
    host: Arc<H>,
) -> Result<Vec<(String, bool)>, WorkspaceError> {
    blocking(move || library::load_session(host.db(), host.grants())).await
}

/// `clear_session`.
pub async fn clear_session<H: WorkspaceHost>(host: Arc<H>) -> Result<(), WorkspaceError> {
    blocking(move || library::clear_session(host.db())).await
}

/// `read_document`.
pub async fn read_document<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
) -> Result<RawDocument, WorkspaceError> {
    blocking(move || document_io::read_document(host.db(), host.grants(), host.backend(), &path))
        .await
}

/// `write_document`.
pub async fn write_document<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
    content: String,
    encoding: Option<String>,
    expected_source_sha256: Option<String>,
) -> Result<WriteDocumentResult, WorkspaceError> {
    blocking(move || {
        document_io::write_document(
            host.grants(),
            host.backend(),
            &path,
            &content,
            encoding,
            expected_source_sha256.as_deref(),
        )
    })
    .await
}

/// `write_document_base64` / raw-body save: the app decodes the request into a [`WritePayload`].
pub async fn write_document_payload<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
    payload: WritePayload,
) -> Result<(), WorkspaceError> {
    blocking(move || {
        document_io::write_document_payload(host.grants(), host.backend(), &path, payload)
    })
    .await
}

/// `copy_document`.
pub async fn copy_document<H: WorkspaceHost>(
    host: Arc<H>,
    source_path: String,
    target_path: String,
) -> Result<(), WorkspaceError> {
    blocking(move || {
        document_io::copy_document(host.grants(), host.backend(), &source_path, &target_path)
    })
    .await
}

/// `revoke_path`.
pub fn revoke_path<H: WorkspaceHost>(host: &H, path: &str) -> Result<(), WorkspaceError> {
    document_io::revoke_path(host.grants(), path)
}

/// `list_sibling_images`.
pub async fn list_sibling_images<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
) -> Result<Vec<String>, WorkspaceError> {
    blocking(move || document_io::list_sibling_images(host.grants(), host.backend(), &path)).await
}

/// `file_fingerprint`.
pub fn file_fingerprint<H: WorkspaceHost>(
    host: &H,
    path: &str,
) -> Result<FileFingerprint, WorkspaceError> {
    document_io::file_fingerprint(host.grants(), host.backend(), path)
}

/// `watch_file`.
pub fn watch_file<H: WorkspaceHost>(host: &H, path: &str) -> Result<(), WorkspaceError> {
    document_io::watch_file(host.grants(), host.watcher(), path)
}

/// `watch_files`.
pub fn watch_files<H: WorkspaceHost>(host: &H, paths: &[String]) -> Result<(), WorkspaceError> {
    document_io::watch_files(host.grants(), host.watcher(), paths)
}

/// `explorer_locations`.
pub async fn explorer_locations<H: WorkspaceHost>(
    host: Arc<H>,
) -> Result<Vec<ExplorerLocation>, WorkspaceError> {
    blocking(move || Ok(explorer::explorer_locations(&host.home()))).await
}

/// `explorer_list_dir`. `on_timing` receives timing marks for the app's diagnostics.
pub async fn explorer_list_dir<H: WorkspaceHost>(
    host: Arc<H>,
    dir: String,
    keep: Option<String>,
    on_timing: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<ExplorerListing, WorkspaceError> {
    blocking(move || {
        explorer::explorer_list_dir(
            host.app_name(),
            host.grants(),
            host.backend(),
            &dir,
            keep.as_deref(),
            &*on_timing,
        )
    })
    .await
}

/// `explorer_watch_dir`.
pub async fn explorer_watch_dir<H: WorkspaceHost>(
    host: Arc<H>,
    dir: String,
) -> Result<(), WorkspaceError> {
    blocking(move || explorer::explorer_watch_dir(host.app_name(), host.watcher(), &dir)).await
}

/// `explorer_unwatch_dir`.
pub async fn explorer_unwatch_dir<H: WorkspaceHost>(
    host: Arc<H>,
    dir: String,
) -> Result<(), WorkspaceError> {
    blocking(move || {
        explorer::explorer_unwatch_dir(host.watcher(), &dir);
        Ok(())
    })
    .await
}

/// `explorer_reveal`.
pub async fn explorer_reveal<H: WorkspaceHost>(
    host: Arc<H>,
    path: String,
) -> Result<(), WorkspaceError> {
    blocking(move || explorer::explorer_reveal(host.app_name(), &path)).await
}

/// Request body of `index_markdown_folder`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexMarkdownFolderRequest {
    /// The open document whose folder is indexed.
    pub document_path: String,
    /// Folder enumeration is opt-in: opening a document must never turn into a directory walk.
    pub explicit_consent: bool,
    /// Descend into subfolders.
    #[serde(default = "default_recursive")]
    pub recursive: bool,
}

fn default_recursive() -> bool {
    true
}

/// `index_markdown_folder`.
pub async fn index_markdown_folder<H: WorkspaceHost>(
    host: Arc<H>,
    req: IndexMarkdownFolderRequest,
) -> Result<IndexedMarkdownFolderResult, WorkspaceError> {
    if !req.explicit_consent {
        return Err(WorkspaceError::BrokerDenied(
            "folder index requires explicit user consent".to_string(),
        ));
    }
    blocking(move || {
        let authorized = host
            .grants()
            .require_existing_file(&req.document_path)
            .map_err(broker_error)?;
        markdown_index::index_markdown_folder(&PathBuf::from(authorized), req.recursive)
    })
    .await
}

/// `inspect_markdown_resources`.
pub async fn inspect_markdown_resources<H: WorkspaceHost>(
    host: Arc<H>,
    document_path: String,
    requests: Vec<MarkdownResourceRequest>,
) -> Result<Vec<MarkdownResourceResult>, WorkspaceError> {
    blocking(move || {
        markdown_resources::inspect_markdown_resources(host.grants(), &document_path, requests)
    })
    .await
}

/// `save_markdown_asset`.
pub async fn save_markdown_asset<H: WorkspaceHost>(
    host: Arc<H>,
    req: SaveMarkdownAssetRequest,
) -> Result<SavedMarkdownAsset, WorkspaceError> {
    blocking(move || {
        markdown_resources::save_markdown_asset(req, host.grants(), host.transactions())
    })
    .await
}

/// `rollback_markdown_asset`.
pub async fn rollback_markdown_asset<H: WorkspaceHost>(
    host: Arc<H>,
    req: RollbackMarkdownAssetRequest,
) -> Result<bool, WorkspaceError> {
    blocking(move || {
        markdown_resources::rollback_markdown_asset(host.transactions(), &req.rollback_token)
    })
    .await
}

/// `commit_markdown_asset`.
pub async fn commit_markdown_asset<H: WorkspaceHost>(
    host: Arc<H>,
    req: CommitMarkdownAssetRequest,
) -> Result<bool, WorkspaceError> {
    blocking(move || {
        markdown_resources::commit_markdown_asset(host.transactions(), &req.rollback_token)
    })
    .await
}

/// `settings_get`.
pub async fn settings_get<H: WorkspaceHost>(
    host: Arc<H>,
    key: String,
    with_revision: Option<bool>,
) -> Result<Value, WorkspaceError> {
    blocking(move || host.settings().get(&key, with_revision)).await
}

/// `settings_set`. The answer is the renderer's reply; `on_diagnostics` receives the committed
/// diagnostics choice when the settings document was written.
pub async fn settings_set<H: WorkspaceHost>(
    host: Arc<H>,
    key: String,
    value: String,
    expected_revision: Option<u64>,
    merge: Option<bool>,
    on_diagnostics: impl FnOnce(bool) + Send,
) -> Result<Value, WorkspaceError> {
    let write =
        blocking(move || host.settings().set(&key, &value, expected_revision, merge)).await?;
    if let Some(enabled) = write.diagnostics_enabled() {
        on_diagnostics(enabled);
    }
    Ok(write.answer)
}

/// `persistence_status`.
pub fn persistence_status<H: WorkspaceHost>(host: &H) -> PersistenceStatus {
    status_of(host.degraded())
}
