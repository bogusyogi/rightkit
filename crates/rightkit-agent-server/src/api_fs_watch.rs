//! `fs/watch` and `fs/unwatch` daemon endpoints.
//!
//! DIRECT_PORT_RUST (atom VG-003, lane A17-vg-rust-ports): the subscription
//! lifecycle (register/unwatch/debounce/notify) is adapted from codex
//! `codex-rs/app-server/src/fs_watch.rs`'s `FsWatchManager`. Donor revision
//! and NOTICE entry are recorded in
//! `docs/canon/daemon-master-canon-atoms.md`.
//!
//! Seam adaptation (required — RightKit has no persistent JSON-RPC
//! connection/session concept like codex's `app-server`):
//!   - Donor keys watches by `(ConnectionId, watch_id)` over a long-lived
//!     RPC connection and pushes `FsChanged` notifications back down that
//!     same connection via `OutgoingMessageSender`.
//!   - This port keys watches by `(client_id: String, watch_id: String)`
//!     and, since this daemon is a stateless-per-request Axum HTTP server
//!     with no push channel wired to callers yet, buffers debounced
//!     `FsChangedNotification` batches per watch for the caller to drain via
//!     `GET /fs/watch/{client_id}/{watch_id}/poll` — the same buffered-poll
//!     buffered-poll shape. A push transport (SSE/WS) can replace the poll drain later without
//!     touching the ported `FileWatcher`/`FsWatchManager` core.
//!   - `connection_closed` becomes `client_disconnected(client_id)`.
//!
//! The underlying watcher engine (`rightkit_file_watch`) is an
//! unmodified direct port; only this connection-shape seam is original.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use rightkit_file_watch::{
    DebouncedWatchReceiver, FileWatcher, FileWatcherSubscriber, WatchPath, WatchRegistration,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

use crate::api::DaemonState;

const FS_CHANGED_DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWatchRequest {
    pub watch_id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWatchResponse {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsUnwatchResponse {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsChangedNotification {
    pub watch_id: String,
    pub changed_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FsWatchPollResponse {
    pub events: Vec<FsChangedNotification>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWatchError {
    pub message: String,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct WatchKey {
    client_id: String,
    watch_id: String,
}

struct WatchEntry {
    terminate_tx: oneshot::Sender<oneshot::Sender<()>>,
    pending: Arc<AsyncMutex<Vec<FsChangedNotification>>>,
    _subscriber: FileWatcherSubscriber,
    _registration: WatchRegistration,
}

#[derive(Default)]
struct FsWatchStateInner {
    entries: HashMap<WatchKey, WatchEntry>,
}

/// Shared daemon-wide `fs/watch` manager. One [`FileWatcher`] core backs
/// every client's watches, matching the donor's one-watcher-per-process shape.
#[derive(Clone)]
pub struct FsWatchManager {
    file_watcher: Arc<FileWatcher>,
    state: Arc<AsyncMutex<FsWatchStateInner>>,
}

impl Default for FsWatchManager {
    fn default() -> Self {
        let file_watcher = match FileWatcher::new() {
            Ok(watcher) => Arc::new(watcher),
            Err(err) => {
                warn!("fs watch manager falling back to noop core watcher: {err}");
                Arc::new(FileWatcher::noop())
            }
        };
        Self {
            file_watcher,
            state: Arc::new(AsyncMutex::new(FsWatchStateInner::default())),
        }
    }
}

impl FsWatchManager {
    #[cfg(test)]
    fn new_with_file_watcher(file_watcher: Arc<FileWatcher>) -> Self {
        Self {
            file_watcher,
            state: Arc::new(AsyncMutex::new(FsWatchStateInner::default())),
        }
    }

    pub async fn watch(
        &self,
        client_id: String,
        req: FsWatchRequest,
    ) -> Result<FsWatchResponse, FsWatchError> {
        let watch_key = WatchKey {
            client_id: client_id.clone(),
            watch_id: req.watch_id.clone(),
        };
        let (subscriber, rx) = self.file_watcher.add_subscriber();
        let watch_root = req.path.clone();
        let registration = subscriber.register_paths(vec![WatchPath {
            path: req.path.clone(),
            recursive: req.recursive,
        }]);
        let (terminate_tx, terminate_rx) = oneshot::channel();
        let pending = Arc::new(AsyncMutex::new(Vec::new()));

        match self.state.lock().await.entries.entry(watch_key) {
            Entry::Occupied(_) => {
                return Err(FsWatchError {
                    message: format!("watchId already exists: {}", req.watch_id),
                });
            }
            Entry::Vacant(entry) => {
                entry.insert(WatchEntry {
                    terminate_tx,
                    pending: pending.clone(),
                    _subscriber: subscriber,
                    _registration: registration,
                });
            }
        }

        let task_watch_id = req.watch_id.clone();
        tokio::spawn(async move {
            let mut rx = DebouncedWatchReceiver::new(rx, FS_CHANGED_DEBOUNCE);
            tokio::pin!(terminate_rx);
            loop {
                let event = tokio::select! {
                    biased;
                    _ = &mut terminate_rx => break,
                    event = rx.recv() => match event {
                        Some(event) => event,
                        None => break,
                    },
                };
                let mut changed_paths = event
                    .paths
                    .into_iter()
                    .map(|path| {
                        if path.is_absolute() {
                            path
                        } else {
                            watch_root.join(path)
                        }
                    })
                    .collect::<Vec<_>>();
                changed_paths.sort_by(|left, right| left.as_path().cmp(right.as_path()));
                if !changed_paths.is_empty() {
                    pending.lock().await.push(FsChangedNotification {
                        watch_id: task_watch_id.clone(),
                        changed_paths,
                    });
                }
            }
        });

        Ok(FsWatchResponse { path: req.path })
    }

    pub async fn unwatch(
        &self,
        client_id: String,
        watch_id: String,
    ) -> Result<FsUnwatchResponse, FsWatchError> {
        let watch_key = WatchKey {
            client_id,
            watch_id,
        };
        let entry = self.state.lock().await.entries.remove(&watch_key);
        if let Some(entry) = entry {
            // Wait for the oneshot to be destroyed by the task to ensure that
            // no further notifications land after the unwatch response.
            let (done_tx, done_rx) = oneshot::channel();
            let _ = entry.terminate_tx.send(done_tx);
            let _ = done_rx.await;
        }
        Ok(FsUnwatchResponse {})
    }

    pub async fn poll(&self, client_id: String, watch_id: String) -> FsWatchPollResponse {
        let watch_key = WatchKey {
            client_id,
            watch_id,
        };
        let state = self.state.lock().await;
        let Some(entry) = state.entries.get(&watch_key) else {
            return FsWatchPollResponse::default();
        };
        let mut pending = entry.pending.lock().await;
        FsWatchPollResponse {
            events: std::mem::take(&mut *pending),
        }
    }

    /// Adapted from the donor's `connection_closed` — releases every watch
    /// owned by one client, e.g. on daemon session teardown.
    pub async fn client_disconnected(&self, client_id: &str) {
        let mut state = self.state.lock().await;
        state.entries.retain(|key, _| key.client_id != client_id);
    }
}

pub fn router() -> Router<DaemonState> {
    Router::new()
        .route("/fs/watch/{client_id}", post(watch_handler))
        .route("/fs/watch/{client_id}/{watch_id}", delete(unwatch_handler))
        .route("/fs/watch/{client_id}/{watch_id}/poll", get(poll_handler))
}

async fn watch_handler(
    State(state): State<DaemonState>,
    AxumPath(client_id): AxumPath<String>,
    Json(req): Json<FsWatchRequest>,
) -> Result<Json<FsWatchResponse>, (StatusCode, Json<FsWatchError>)> {
    state
        .fs_watch
        .watch(client_id, req)
        .await
        .map(Json)
        .map_err(|err| (StatusCode::CONFLICT, Json(err)))
}

async fn unwatch_handler(
    State(state): State<DaemonState>,
    AxumPath((client_id, watch_id)): AxumPath<(String, String)>,
) -> Json<FsUnwatchResponse> {
    let result = state
        .fs_watch
        .unwatch(client_id, watch_id)
        .await
        .unwrap_or(FsUnwatchResponse {});
    Json(result)
}

async fn poll_handler(
    State(state): State<DaemonState>,
    AxumPath((client_id, watch_id)): AxumPath<(String, String)>,
) -> Json<FsWatchPollResponse> {
    Json(state.fs_watch.poll(client_id, watch_id).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn manager_with_noop_watcher() -> FsWatchManager {
        FsWatchManager::new_with_file_watcher(Arc::new(FileWatcher::noop()))
    }

    #[tokio::test]
    async fn watch_uses_client_id_and_tracks_the_owner_scoped_entry() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let head_path = temp_dir.path().join("HEAD");
        std::fs::write(&head_path, "ref: refs/heads/main\n").expect("write HEAD");

        let manager = manager_with_noop_watcher();
        let watch_id = "watch-head".to_string();
        let response = manager
            .watch(
                "client-1".to_string(),
                FsWatchRequest {
                    watch_id: watch_id.clone(),
                    path: head_path.clone(),
                    recursive: false,
                },
            )
            .await
            .expect("watch should succeed");

        assert_eq!(response.path, head_path);

        let state = manager.state.lock().await;
        let keys: HashSet<(String, String)> = state
            .entries
            .keys()
            .map(|k| (k.client_id.clone(), k.watch_id.clone()))
            .collect();
        assert_eq!(keys, HashSet::from([("client-1".to_string(), watch_id)]));
    }

    #[tokio::test]
    async fn unwatch_is_scoped_to_the_client_that_created_the_watch() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let head_path = temp_dir.path().join("HEAD");
        std::fs::write(&head_path, "ref: refs/heads/main\n").expect("write HEAD");

        let manager = manager_with_noop_watcher();
        manager
            .watch(
                "client-1".to_string(),
                FsWatchRequest {
                    watch_id: "watch-head".to_string(),
                    path: head_path.clone(),
                    recursive: false,
                },
            )
            .await
            .expect("watch should succeed");

        manager
            .unwatch("client-2".to_string(), "watch-head".to_string())
            .await
            .expect("foreign unwatch should be a no-op");
        assert_eq!(manager.state.lock().await.entries.len(), 1);

        manager
            .unwatch("client-1".to_string(), "watch-head".to_string())
            .await
            .expect("owner unwatch should succeed");
        assert_eq!(manager.state.lock().await.entries.len(), 0);
    }

    #[tokio::test]
    async fn watch_rejects_duplicate_id_for_the_same_client() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let head_path = temp_dir.path().join("HEAD");
        let fetch_head_path = temp_dir.path().join("FETCH_HEAD");
        std::fs::write(&head_path, "ref: refs/heads/main\n").expect("write HEAD");
        std::fs::write(&fetch_head_path, "old-fetch\n").expect("write FETCH_HEAD");

        let manager = manager_with_noop_watcher();
        manager
            .watch(
                "client-1".to_string(),
                FsWatchRequest {
                    watch_id: "watch-head".to_string(),
                    path: head_path,
                    recursive: false,
                },
            )
            .await
            .expect("first watch should succeed");

        let error = manager
            .watch(
                "client-1".to_string(),
                FsWatchRequest {
                    watch_id: "watch-head".to_string(),
                    path: fetch_head_path,
                    recursive: false,
                },
            )
            .await
            .expect_err("duplicate watch should fail");

        assert_eq!(error.message, "watchId already exists: watch-head");
        assert_eq!(manager.state.lock().await.entries.len(), 1);
    }

    #[tokio::test]
    async fn client_disconnected_removes_only_that_clients_watches() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let head_path = temp_dir.path().join("HEAD");
        let fetch_head_path = temp_dir.path().join("FETCH_HEAD");
        let packed_refs_path = temp_dir.path().join("packed-refs");
        std::fs::write(&head_path, "ref: refs/heads/main\n").expect("write HEAD");
        std::fs::write(&fetch_head_path, "old-fetch\n").expect("write FETCH_HEAD");
        std::fs::write(&packed_refs_path, "refs\n").expect("write packed-refs");

        let manager = manager_with_noop_watcher();
        manager
            .watch(
                "client-1".to_string(),
                FsWatchRequest {
                    watch_id: "watch-head".to_string(),
                    path: head_path,
                    recursive: false,
                },
            )
            .await
            .expect("first watch should succeed");
        manager
            .watch(
                "client-1".to_string(),
                FsWatchRequest {
                    watch_id: "watch-fetch-head".to_string(),
                    path: fetch_head_path,
                    recursive: false,
                },
            )
            .await
            .expect("second watch should succeed");
        manager
            .watch(
                "client-2".to_string(),
                FsWatchRequest {
                    watch_id: "watch-packed-refs".to_string(),
                    path: packed_refs_path,
                    recursive: false,
                },
            )
            .await
            .expect("third watch should succeed");

        manager.client_disconnected("client-1").await;

        let state = manager.state.lock().await;
        let keys: HashSet<(String, String)> = state
            .entries
            .keys()
            .map(|k| (k.client_id.clone(), k.watch_id.clone()))
            .collect();
        assert_eq!(
            keys,
            HashSet::from([("client-2".to_string(), "watch-packed-refs".to_string())])
        );
    }
}
