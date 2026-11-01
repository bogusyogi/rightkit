//! Workspace-scoped ownership of live language-server subprocesses.

use crate::coding_tools::shell::NativeCommandSandboxAdapter;
use rightkit_mcp::agent_mcp::{LspMcpBridge, LspServerConfig};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

const MUTATION_SYNC_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_millis(750);
const EXTERNAL_MUTATION_POLL_INTERVAL: tokio::time::Duration =
    tokio::time::Duration::from_millis(500);
const MAX_WATCHED_DOCUMENTS: usize = 128;
const MAX_WATCHED_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) type SharedLspBridge = Arc<Mutex<LspMcpBridge>>;

#[derive(Clone)]
struct BridgeEntry {
    config: LspServerConfig,
    bridge: SharedLspBridge,
    launch_adapter: Option<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
    tracked_documents: Arc<Mutex<HashMap<String, String>>>,
    watcher_started: Arc<AtomicBool>,
}

fn bridges() -> &'static Mutex<HashMap<String, BridgeEntry>> {
    static BRIDGES: OnceLock<Mutex<HashMap<String, BridgeEntry>>> = OnceLock::new();
    BRIDGES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn bridge_key(config: &LspServerConfig) -> String {
    format!(
        "{}\0{}\0{}\0{}",
        config.workspace_root,
        config.language_id,
        config.server_path,
        config.server_args.join("\0")
    )
}

async fn cached_entry(config: &LspServerConfig) -> BridgeEntry {
    cached_entry_with_launcher(
        config,
        Some(Arc::new(NativeCommandSandboxAdapter)),
        rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: config.workspace_root.clone(),
            sandbox_profile: None,
            sandbox_required: false,
            network_required: false,
        },
    )
    .await
}

async fn cached_entry_with_launcher(
    config: &LspServerConfig,
    launch_adapter: Option<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
) -> BridgeEntry {
    let key = bridge_key(config);
    let mut registry = bridges().lock().await;
    registry
        .entry(key)
        .or_insert_with(|| BridgeEntry {
            config: config.clone(),
            bridge: Arc::new(Mutex::new(match launch_adapter.as_ref() {
                Some(adapter) => LspMcpBridge::new(config.clone())
                    .with_process_launcher(adapter.clone(), launch_context.clone()),
                None => {
                    LspMcpBridge::new(config.clone()).with_launch_context(launch_context.clone())
                }
            })),
            launch_adapter: launch_adapter.clone(),
            launch_context: launch_context.clone(),
            tracked_documents: Arc::new(Mutex::new(HashMap::new())),
            watcher_started: Arc::new(AtomicBool::new(false)),
        })
        .clone()
}

#[cfg(test)]
async fn cached_bridge(config: &LspServerConfig) -> SharedLspBridge {
    cached_entry(config).await.bridge
}

#[allow(dead_code)]
pub(crate) async fn initialized_bridge(
    config: &LspServerConfig,
) -> Result<SharedLspBridge, String> {
    initialized_bridge_with_launcher(
        config,
        Some(Arc::new(NativeCommandSandboxAdapter)),
        rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: config.workspace_root.clone(),
            sandbox_profile: None,
            sandbox_required: false,
            network_required: false,
        },
    )
    .await
}

pub(crate) async fn initialized_bridge_with_launcher(
    config: &LspServerConfig,
    launch_adapter: Option<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
) -> Result<SharedLspBridge, String> {
    let entry = cached_entry_with_launcher(config, launch_adapter, launch_context).await;
    let mut guard = entry.bridge.lock().await;
    if !guard.is_initialized() {
        if let Err(error) = guard.initialize().await {
            drop(guard);
            invalidate_generation(config, &entry.bridge).await;
            return Err(error.to_string());
        }
        let documents = entry.tracked_documents.lock().await.clone();
        for (uri, text) in documents {
            if let Err(error) = guard.sync_document(&uri, &text).await {
                drop(guard);
                invalidate_generation(config, &entry.bridge).await;
                return Err(format!("failed to replay tracked LSP document: {error}"));
            }
        }
    }
    drop(guard);
    ensure_external_mutation_watcher(&entry);
    Ok(entry.bridge)
}

/// Keep documents already opened by the host app synchronized when another
/// process (shell, Git, editor, formatter) changes them on disk. This is
/// intentionally bounded to the live document set; it never scans a workspace.
fn ensure_external_mutation_watcher(entry: &BridgeEntry) {
    if entry.watcher_started.swap(true, Ordering::AcqRel) {
        return;
    }
    let entry = entry.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(EXTERNAL_MUTATION_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if !entry_is_current(&entry).await {
                break;
            }
            sync_external_document_changes(&entry).await;
        }
    });
}

async fn entry_is_current(entry: &BridgeEntry) -> bool {
    bridges()
        .lock()
        .await
        .get(&bridge_key(&entry.config))
        .is_some_and(|current| {
            Arc::ptr_eq(&current.tracked_documents, &entry.tracked_documents)
                && Arc::ptr_eq(&current.bridge, &entry.bridge)
        })
}

async fn sync_external_document_changes(entry: &BridgeEntry) {
    let documents = entry
        .tracked_documents
        .lock()
        .await
        .iter()
        .take(MAX_WATCHED_DOCUMENTS)
        .map(|(uri, text)| (uri.clone(), text.clone()))
        .collect::<Vec<_>>();

    for (uri, previous) in documents {
        let Some(current) = read_changed_document(&uri, &previous).await else {
            continue;
        };

        entry
            .tracked_documents
            .lock()
            .await
            .insert(uri.clone(), current.clone());
        let sync = async {
            let mut bridge = entry.bridge.lock().await;
            if bridge.is_initialized() {
                bridge.sync_document(&uri, &current).await.map(|_| ())
            } else {
                Ok(())
            }
        };
        if !matches!(
            tokio::time::timeout(MUTATION_SYNC_TIMEOUT, sync).await,
            Ok(Ok(()))
        ) {
            invalidate_generation(&entry.config, &entry.bridge).await;
        }
    }
}

async fn read_changed_document(uri: &str, previous: &str) -> Option<String> {
    let path = file_uri_path(uri)?;
    let metadata = tokio::fs::metadata(&path).await.ok()?;
    if metadata.len() > MAX_WATCHED_DOCUMENT_BYTES {
        return None;
    }
    let current = tokio::fs::read_to_string(path).await.ok()?;
    (current != previous).then_some(current)
}

fn file_uri_path(uri: &str) -> Option<std::path::PathBuf> {
    let parsed = url::Url::parse(uri).ok()?;
    (parsed.scheme() == "file")
        .then(|| parsed.to_file_path().ok())
        .flatten()
}

/// Replace only the failed generation, preserving tracked documents for replay.
/// A stale failing request cannot evict a newer healthy generation.
pub(crate) async fn invalidate_generation(config: &LspServerConfig, failed: &SharedLspBridge) {
    let replaced = {
        let mut registry = bridges().lock().await;
        let Some(entry) = registry.get_mut(&bridge_key(config)) else {
            return;
        };
        if Arc::ptr_eq(&entry.bridge, failed) {
            let replacement = match entry.launch_adapter.as_ref() {
                Some(adapter) => LspMcpBridge::new(config.clone())
                    .with_process_launcher(adapter.clone(), entry.launch_context.clone()),
                None => LspMcpBridge::new(config.clone())
                    .with_launch_context(entry.launch_context.clone()),
            };
            entry.bridge = Arc::new(Mutex::new(replacement));
            entry.watcher_started.store(false, Ordering::Release);
            true
        } else {
            false
        }
    };
    if replaced {
        failed.lock().await.shutdown().await;
    }
}

/// Live count of workspace-scoped LSP bridges currently registered.
///
/// Bounded by the process-global `BRIDGES` map; the map itself is the bound —
/// one entry per distinct `(workspace_root, language_id, server_path, args)`.
/// Used by `GET /runtime/resource-metrics` to surface `lspBridges.count` as a
/// live gauge instead of `null`.
pub async fn bridge_count() -> usize {
    bridges().lock().await.len()
}

/// Snapshot of the bridge registry length plus the distinct workspace roots
/// that currently own a bridge. Useful for provenance in baseline reports.
pub async fn bridge_snapshot() -> (usize, Vec<String>) {
    let registry = bridges().lock().await;
    let count = registry.len();
    let mut roots: Vec<String> = registry
        .values()
        .map(|entry| entry.config.workspace_root.clone())
        .collect();
    roots.sort();
    roots.dedup();
    (count, roots)
}

#[cfg(test)]
pub(crate) async fn evict(config: &LspServerConfig) {
    bridges().lock().await.remove(&bridge_key(config));
}

pub(crate) async fn sync_document(
    config: &LspServerConfig,
    bridge: &SharedLspBridge,
    uri: &str,
    text: &str,
) -> Result<i32, String> {
    let entry = cached_entry(config).await;
    entry
        .tracked_documents
        .lock()
        .await
        .insert(uri.to_string(), text.to_string());
    let result = bridge.lock().await.sync_document(uri, text).await;
    match result {
        Ok(version) => Ok(version),
        Err(error) => {
            invalidate_generation(config, bridge).await;
            Err(error.to_string())
        }
    }
}

/// Push a successful the host app file mutation into an already-live language
/// server. Edits do not start new servers; the first semantic LSP request owns
/// that lifecycle, and later writes advance its in-memory document revision.
pub async fn notify_document_mutated(workspace_root: &Path, path: &Path) {
    let Some(language_id) = super::implementation::language_id(path) else {
        return;
    };
    let workspace_uri = super::implementation::file_uri(workspace_root);
    let live = {
        let registry = bridges().lock().await;
        registry
            .values()
            .filter(|entry| {
                entry.config.workspace_root == workspace_uri
                    && entry.config.language_id == language_id
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    if live.is_empty() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let uri = super::implementation::file_uri(path);
    for entry in live {
        if tokio::time::timeout(
            MUTATION_SYNC_TIMEOUT,
            sync_document(&entry.config, &entry.bridge, &uri, &text),
        )
        .await
        .is_err()
        {
            // File writes must not wait behind a wedged language server. The
            // tracked snapshot remains in the entry and is replayed into the
            // replacement generation on the next semantic request.
            invalidate_generation(&entry.config, &entry.bridge).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn config(root: &str) -> LspServerConfig {
        LspServerConfig {
            language_id: "rust".to_string(),
            server_path: "/definitely/not/started/rust-analyzer".to_string(),
            server_args: vec!["--stdio".to_string()],
            workspace_root: root.to_string(),
        }
    }

    #[tokio::test]
    async fn cache_reuses_one_bridge_per_workspace_server() {
        let _guard = TEST_LOCK.lock().await;
        let config = config("file:///tmp/lsp-cache-reuse");
        let first = cached_bridge(&config).await;
        let second = cached_bridge(&config).await;
        assert!(Arc::ptr_eq(&first, &second));
        evict(&config).await;
    }

    #[tokio::test]
    async fn cache_separates_workspaces() {
        let _guard = TEST_LOCK.lock().await;
        let first_config = config("file:///tmp/lsp-cache-a");
        let second_config = config("file:///tmp/lsp-cache-b");
        let first = cached_bridge(&first_config).await;
        let second = cached_bridge(&second_config).await;
        assert!(!Arc::ptr_eq(&first, &second));
        evict(&first_config).await;
        evict(&second_config).await;
    }

    #[tokio::test]
    async fn failed_generation_is_replaced_without_losing_tracked_documents() {
        let _guard = TEST_LOCK.lock().await;
        let config = config("file:///tmp/lsp-replay");
        let entry = cached_entry(&config).await;
        entry.tracked_documents.lock().await.insert(
            "file:///tmp/lsp-replay/src/lib.rs".into(),
            "fn first() {}".into(),
        );

        invalidate_generation(&config, &entry.bridge).await;
        let replacement = cached_entry(&config).await;

        assert!(!Arc::ptr_eq(&entry.bridge, &replacement.bridge));
        assert_eq!(
            replacement
                .tracked_documents
                .lock()
                .await
                .get("file:///tmp/lsp-replay/src/lib.rs")
                .map(String::as_str),
            Some("fn first() {}")
        );
        evict(&config).await;
    }

    #[tokio::test]
    async fn stale_failure_cannot_replace_new_generation() {
        let _guard = TEST_LOCK.lock().await;
        let config = config("file:///tmp/lsp-generation");
        let first = cached_entry(&config).await;
        invalidate_generation(&config, &first.bridge).await;
        let second = cached_entry(&config).await;

        invalidate_generation(&config, &first.bridge).await;
        let still_second = cached_entry(&config).await;

        assert!(Arc::ptr_eq(&second.bridge, &still_second.bridge));
        evict(&config).await;
    }

    #[test]
    fn file_uri_path_decodes_paths_and_rejects_non_files() {
        // A driveless `/tmp/...` URI is not a valid Windows file path, so
        // `to_file_path()` correctly yields None there. Assert the percent-decode
        // against a path the host can actually represent.
        #[cfg(windows)]
        let (uri, decoded) = ("file:///C:/tmp/a%20b.rs", "C:\\tmp\\a b.rs");
        #[cfg(not(windows))]
        let (uri, decoded) = ("file:///tmp/a%20b.rs", "/tmp/a b.rs");

        assert_eq!(file_uri_path(uri), Some(std::path::PathBuf::from(decoded)));
        assert_eq!(file_uri_path("https://example.com/a.rs"), None);
    }

    #[tokio::test]
    async fn detects_only_changed_bounded_utf8_documents() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("watched.rs");
        tokio::fs::write(&path, "fn before() {}\n").await.unwrap();
        let uri = super::super::implementation::file_uri(&path);

        assert_eq!(read_changed_document(&uri, "fn before() {}\n").await, None);
        tokio::fs::write(&path, "fn after() {}\n").await.unwrap();
        assert_eq!(
            read_changed_document(&uri, "fn before() {}\n").await,
            Some("fn after() {}\n".to_string())
        );
    }

    #[tokio::test]
    async fn bridge_count_reflects_live_registry() {
        let _guard = TEST_LOCK.lock().await;
        let a = config("file:///tmp/lsp-count-a");
        let b = config("file:///tmp/lsp-count-b");
        let before = bridge_count().await;
        let _a_bridge = cached_bridge(&a).await;
        assert_eq!(bridge_count().await, before + 1);
        let _b_bridge = cached_bridge(&b).await;
        assert_eq!(bridge_count().await, before + 2);
        let (count, roots) = bridge_snapshot().await;
        assert_eq!(count, before + 2);
        assert!(roots.contains(&"file:///tmp/lsp-count-a".to_string()));
        assert!(roots.contains(&"file:///tmp/lsp-count-b".to_string()));
        evict(&a).await;
        evict(&b).await;
        assert_eq!(bridge_count().await, before);
    }
}
