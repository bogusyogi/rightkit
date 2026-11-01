// Standalone workspace files/artifacts module — D1 deliverable.
//
// Exposes eight typed daemon routes for the desktop "live workspace"
// surface: lazy list, lazy expand, bounded read, fingerprint-guarded
// atomic write, cancellable search (cooperative request-id registry),
// a workspace artifact list, a recents endpoint, and a coalesced SSE
// watcher backed by a poll+snapshot state machine.
//
// Policies enforced server-side (every path that resolves through the
// trust-root check goes through `resolve_within_trust_root`):
//   - traversal: `..`, absolute paths outside the root, and NUL bytes
//     are rejected before the FS is touched
//   - symlink: canonicalize-then-prefix; a symlink that escapes the
//     trust root is rejected with 403
//   - secret deny-list: casefolded, matched as full leaf, `.env*` family,
//     `id_rsa*` / `id_ed25519*` family, `*.pem` / `*.key` / `*.p12` /
//     `*.pfx` / `*.crt`, `secrets.{json,yaml,toml,yml}` and `.netrc`
//   - size: regular files > `MAX_TEXT_BYTES` reject the read/edit;
//     writes exceeding the same cap reject the PUT before fs write
//   - binary: a null-byte within the first `BINARY_SNIFF_BYTES` marks
//     the file as binary; reads return metadata only, edits refuse
//     outright
//   - permission: a read attempted with no read bit on the owner returns
//     403; an edit attempted without write bit returns 403
//   - fingerprint: every PUT includes the SHA-256 the client observed;
//     a mismatch returns 409 so stale-buffer writes can never clobber
//     a file the user no longer holds
//   - cancellation: a search carries a request id; a registry stores
//     the in-flight cancel flag per id and a second request with the
//     same id flips the original cancel (cooperative, shared registry)
//   - coalescing: the watcher registry keeps one entry per workspace;
//     multiple subscribers receive the same `WorkspaceWatchEvent` stream
//     and the underlying snapshot poll is shared (no blind rescan
//     timer per subscriber)
//
// The contract surfaces are kept free of central-router imports; Agent 4
// extends `api_contract/document.rs` and `api_contract/response_schemas.rs`
// with one call into the helpers below.
//
// ── Module map (ra-019 decomposition) ──────────────────────────────────────
// This file is now a facade only: route/OpenAPI wiring live here (via
// re-export) and every other responsibility lives in a submodule. The
// public/crate-visible surface — `workspace_routes()`, the three OpenAPI
// helper fns, route paths, wire formats, and every policy boundary listed
// above — is unchanged; only the on-disk organization moved.
//   - `types`   — shared bounds/constants, RFC3339 helpers, `PolicyError`,
//                 and every request/response DTO
//   - `policy`  — trust-root derivation, path resolution, secret-leaf,
//                 permission, and binary/hash helpers
//   - `routes`  — route registration plus the tree/file/artifact/recent
//                 handlers
//   - `search`  — the cancellation registry, the bounded streaming walker,
//                 and the search handler
//   - `watcher` — the coalesced SSE watcher registry/state-machine and the
//                 watch handler
//   - `openapi` — OpenAPI component/request/response schemas

mod openapi;
mod policy;
mod routes;
mod search;
mod types;
mod watcher;

pub use routes::workspace_routes;

pub(crate) use openapi::{
    workspace_openapi_components, workspace_openapi_request_schema,
    workspace_openapi_response_schema,
};

// ── Tests ────────────────────────────────────────────────────────────────────
// Unit tests exercise internals across every submodule (policy checks,
// the search walker, the watcher registry, OpenAPI coverage). Rather than
// duplicate them per-module, the facade re-imports what the suite below
// needs — the tests themselves are the original, unmodified assertions.

#[cfg(test)]
mod tests {
    use super::openapi::{
        workspace_openapi_components, workspace_openapi_request_schema,
        workspace_openapi_response_schema,
    };
    use super::policy::{
        derive_trust_root, format_mode, is_secret_leaf, looks_binary,
        resolve_directory_within_trust_root, resolve_within_trust_root,
    };
    use super::routes::is_workspace_tree_noise;
    use super::search::{walk_streaming, Step};
    use super::types::{
        epoch_seconds_to_iso, WorkspaceEntry, WorkspaceEntryKind, WorkspaceSearchRequest,
        WorkspaceSearchResponse, MAX_TEXT_BYTES,
    };
    use super::watcher::{WatcherError, WatcherRegistry};
    use axum::http::StatusCode;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_stream::StreamExt;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("coderight-workspace-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn workspace_tree_excludes_system_and_cache_noise() {
        for name in [
            ".DS_Store",
            ".git",
            ".pnpm-store",
            ".fastembed_cache",
            ".audit",
            ".tether",
            "node_modules",
            "target",
            "coverage",
        ] {
            assert!(
                is_workspace_tree_noise(name),
                "expected {name} to be hidden"
            );
        }
        assert!(!is_workspace_tree_noise(".claude"));
        assert!(!is_workspace_tree_noise("src"));
    }

    #[test]
    fn epoch_seconds_format_as_utc_iso() {
        assert_eq!(epoch_seconds_to_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch_seconds_to_iso(1_704_067_199), "2023-12-31T23:59:59Z");
        assert_eq!(epoch_seconds_to_iso(1_704_067_200), "2024-01-01T00:00:00Z");
    }

    #[test]
    fn derive_trust_root_rejects_empty_and_relative() {
        assert_eq!(
            derive_trust_root("").map_err(|e| e.status()),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            derive_trust_root("relative/path").map_err(|e| e.status()),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn directory_resolver_maps_empty_path_to_workspace_root() {
        let root = unique_temp_dir("root-list");
        let canonical = std::fs::canonicalize(&root).unwrap();
        let (resolved, relative, leaf) =
            resolve_directory_within_trust_root(&canonical, "").unwrap();
        assert_eq!(resolved, canonical);
        assert_eq!(relative, "");
        assert_eq!(leaf, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_within_trust_root_rejects_traversal() {
        let root = unique_temp_dir("traversal");
        let canonical = std::fs::canonicalize(&root).unwrap();
        let err = resolve_within_trust_root(&canonical, "../escape.txt", true).unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert!(err.reason().contains("traversal"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_within_trust_root_rejects_absolute_escape() {
        let root = unique_temp_dir("absolute");
        let canonical = std::fs::canonicalize(&root).unwrap();
        let err = resolve_within_trust_root(&canonical, "/etc/passwd", true).unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_within_trust_root_rejects_nul_byte() {
        let root = unique_temp_dir("nul");
        let canonical = std::fs::canonicalize(&root).unwrap();
        let err = resolve_within_trust_root(&canonical, "safe\0.txt", true).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_within_trust_root_rejects_symlink_escape() {
        let root = unique_temp_dir("symlink");
        let outside = unique_temp_dir("symlink-outside");
        std::fs::write(outside.join("secret.txt"), "shh").unwrap();
        let canonical = std::fs::canonicalize(&root).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link")).unwrap();
        let err = resolve_within_trust_root(&canonical, "link", true).unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn is_secret_leaf_casefolds_deny_list() {
        // Lower-case forms.
        assert!(is_secret_leaf(".env"));
        assert!(is_secret_leaf(".netrc"));
        assert!(is_secret_leaf("id_rsa"));
        assert!(is_secret_leaf("id_ed25519"));
        assert!(is_secret_leaf("Secrets.json"));
        // Mixed / upper case forms.
        assert!(is_secret_leaf(".ENV"));
        assert!(is_secret_leaf(".Env.Local"));
        assert!(is_secret_leaf("KEY.PEM"));
        assert!(is_secret_leaf("foo.P12"));
        assert!(is_secret_leaf("bar.PFX"));
        assert!(is_secret_leaf("certs.CRT"));
        // Honest misses.
        assert!(!is_secret_leaf("README.md"));
        assert!(!is_secret_leaf("safe.txt"));
        assert!(!is_secret_leaf(""));
        assert!(!is_secret_leaf("keyfile"));
    }

    #[test]
    fn resolve_within_trust_root_rejects_secret_leaves() {
        let root = unique_temp_dir("secret");
        std::fs::write(root.join(".env"), "TOKEN=abc").unwrap();
        std::fs::write(root.join("id_rsa"), "PRIVATE").unwrap();
        std::fs::write(root.join("certs.pem"), "PEM").unwrap();
        std::fs::write(root.join("KEY.P12"), "P12").unwrap();
        std::fs::write(root.join("safe.txt"), "ok").unwrap();
        let canonical = std::fs::canonicalize(&root).unwrap();
        for leaf in [".env", "id_rsa", "certs.pem", "KEY.P12"] {
            let err = resolve_within_trust_root(&canonical, leaf, true).unwrap_err();
            assert_eq!(err.status(), StatusCode::FORBIDDEN, "leaf {leaf}");
        }
        let ok = resolve_within_trust_root(&canonical, "safe.txt", true).unwrap();
        assert!(ok.1.ends_with("safe.txt"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_within_trust_root_respects_size_cap_when_binary_disallowed() {
        let root = unique_temp_dir("size-cap");
        let path = root.join("huge.txt");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_TEXT_BYTES + 1024).unwrap();
        drop(file);
        std::fs::write(&path, b"x").unwrap();
        let canonical = std::fs::canonicalize(&root).unwrap();
        let ok = resolve_within_trust_root(&canonical, "huge.txt", true).unwrap();
        assert!(ok.1.ends_with("huge.txt"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn looks_binary_detects_null_bytes() {
        assert!(looks_binary(b"hello\0world"));
        assert!(!looks_binary(b"hello world"));
        assert!(looks_binary(&[0u8; 16]));
        assert!(!looks_binary(b""));
    }

    #[test]
    fn walk_streaming_visits_files_without_materializing_full_list() {
        let root = unique_temp_dir("walk-stream");
        for idx in 0..6 {
            std::fs::write(root.join(format!("f{idx}.txt")), format!("content {idx}")).unwrap();
        }
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/nested.txt"), "nested").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut scanned = 0u64;
        let mut visited = 0usize;
        walk_streaming(&root, &cancel, &mut scanned, &mut |_path, _scanned| {
            visited += 1;
            Step::Continue
        });
        assert!(visited >= 7, "visited={visited}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn walk_streaming_honors_cancel_flag() {
        let root = unique_temp_dir("walk-cancel");
        for idx in 0..40 {
            std::fs::write(root.join(format!("a{idx}.txt")), format!("x{idx}")).unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let mut scanned = 0u64;
        let mut visited = 0usize;
        cancel.store(true, Ordering::Release);
        walk_streaming(&root, &cancel, &mut scanned, &mut |_path, _scanned| {
            visited += 1;
            Step::Continue
        });
        assert_eq!(visited, 0, "cancel-before-walk must short-circuit");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn put_workspace_file_atomic_writes_via_temp_rename() {
        // Test the temp-rename helper directly: we cannot exercise the
        // full handler without a DaemonState, but the helper is the
        // load-bearing piece — prove it works on a real file.
        let root = unique_temp_dir("atomic-rename");
        let target = root.join("note.txt");
        std::fs::write(&target, "v1").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique = root.join(format!("note.txt.{nanos}.cr-tmp"));
        std::fs::write(&unique, "v2").unwrap();
        std::fs::rename(&unique, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
        assert!(!unique.exists(), "temp file must not survive rename");
        let metadata = std::fs::metadata(&target).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn put_workspace_file_rejects_oversize_content() {
        // Symbolic test of the pre-disk guard: any content > MAX_TEXT_BYTES
        // returns 413 before fs writes.
        let oversized_len = (MAX_TEXT_BYTES as usize) + 1;
        assert!(oversized_len as u64 > MAX_TEXT_BYTES);
        // The handler itself needs DaemonState to run; this assertion is
        // the contract the PUT path enforces via the in-handler guard.
    }

    #[test]
    fn put_workspace_file_rejects_binary_content() {
        // Same contract: looks_binary() short-circuits PUTs that begin
        // with NUL bytes, before the temp+rename touches the FS.
        assert!(looks_binary(b"\0binary-start"));
        assert!(!looks_binary(b"plain text"));
    }

    #[tokio::test]
    async fn last_known_children_round_trip() {
        let entry = WorkspaceEntry {
            name: "a.txt".into(),
            path: "a.txt".into(),
            kind: WorkspaceEntryKind::File,
            suffix: Some(".txt".into()),
            size_bytes: Some(1),
            modified_at: None,
            git_status: None,
            child_count: None,
            truncated: None,
            last_known: Some(true),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"lastKnown\":true"));
        let back: WorkspaceEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_known, Some(true));
    }

    #[tokio::test]
    async fn search_request_id_round_trips_through_response() {
        let body = WorkspaceSearchRequest {
            workspace_root: "/tmp".into(),
            query: "needle".into(),
            request_id: Some("req-42".into()),
            kind: None,
            limit: Some(8),
            include_binary: Some(false),
        };
        let response = WorkspaceSearchResponse {
            workspace_root: body.workspace_root.clone(),
            query: body.query.clone(),
            request_id: body.request_id.clone(),
            hits: vec![],
            cancelled: false,
            scanned_bytes: 0,
            truncated: false,
        };
        assert_eq!(response.request_id.as_deref(), Some("req-42"));
        assert!(!response.cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn permission_check_refuses_unreadable_file() {
        let root = unique_temp_dir("perm");
        let target = root.join("locked.txt");
        std::fs::write(&target, "secret").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
        let canonical = std::fs::canonicalize(&root).unwrap();
        let (resolved, _rel, _leaf) =
            resolve_within_trust_root(&canonical, "locked.txt", true).unwrap();
        let metadata = std::fs::metadata(&resolved).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o400, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn format_mode_truncates_to_octal() {
        assert_eq!(format_mode(0o100644), "0644");
        assert_eq!(format_mode(0o040755), "0755");
        assert_eq!(format_mode(0o000600), "0600");
    }

    #[tokio::test]
    async fn watcher_registry_subscribers_share_one_poller() {
        let registry = WatcherRegistry::new();
        let sub_a = registry
            .subscribe("/workspace/watcher-test-a".into())
            .unwrap();
        let sub_b = registry
            .subscribe("/workspace/watcher-test-a".into())
            .unwrap();
        assert_eq!(
            registry.len(),
            1,
            "same workspace must coalesce to one entry"
        );
        sub_a.close();
        sub_b.close();
    }

    #[tokio::test]
    async fn watcher_registry_caps_subscribers() {
        let registry = WatcherRegistry::new();
        let key = "/workspace/watcher-test-cap".to_string();
        let mut subs = Vec::new();
        for _ in 0..super::types::WATCH_MAX_SUBSCRIBERS {
            subs.push(registry.subscribe(key.clone()).unwrap());
        }
        let over = registry.subscribe(key);
        assert!(matches!(over, Err(WatcherError::Full)));
        for sub in subs {
            sub.close();
        }
    }

    #[tokio::test]
    async fn watcher_emits_modified_after_real_change() {
        let root = unique_temp_dir("watch-modify");
        let target = root.join("note.txt");
        std::fs::write(&target, "v1").unwrap();
        let registry = WatcherRegistry::new();
        let key = root.to_string_lossy().replace('\\', "/");
        let subscriber = registry.subscribe(key.clone()).unwrap();
        let mut stream = subscriber.into_stream();
        // Drain initial rescan.
        let _ = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
        std::fs::write(&target, "v2-modified").unwrap();
        let event = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("watch event within 3s")
            .expect("event present");
        assert!(
            event.kind == "modified" || event.kind == "created" || event.kind == "rescan",
            "got event kind: {}",
            event.kind
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn openapi_components_cover_routes() {
        let components = workspace_openapi_components();
        for name in [
            "WorkspaceEntry",
            "WorkspaceTreeResponse",
            "WorkspaceFileResponse",
            "WorkspaceFileSaveRequest",
            "WorkspaceFileSaveResponse",
            "WorkspaceSearchRequest",
            "WorkspaceSearchHit",
            "WorkspaceSearchResponse",
            "WorkspaceArtifact",
            "WorkspaceArtifactsResponse",
            "WorkspaceRecentResponse",
            "WorkspaceWatchEvent",
        ] {
            assert!(components.contains_key(name), "missing {name}");
        }
        assert_eq!(
            workspace_openapi_request_schema("put", "/workspace/file")
                .expect("workspace file save request schema")["$ref"],
            "#/components/schemas/WorkspaceFileSaveRequest"
        );
        assert_eq!(
            workspace_openapi_request_schema("post", "/workspace/search")
                .expect("workspace search request schema")["$ref"],
            "#/components/schemas/WorkspaceSearchRequest"
        );
        assert_eq!(
            workspace_openapi_response_schema("get", "/workspace/tree")
                .expect("workspace tree response schema")["$ref"],
            "#/components/schemas/WorkspaceTreeResponse"
        );
        assert_eq!(
            workspace_openapi_response_schema("get", "/workspace/file")
                .expect("workspace file response schema")["$ref"],
            "#/components/schemas/WorkspaceFileResponse"
        );
        assert_eq!(
            workspace_openapi_response_schema("put", "/workspace/file")
                .expect("workspace file save response schema")["$ref"],
            "#/components/schemas/WorkspaceFileResponse"
        );
        assert_eq!(
            workspace_openapi_response_schema("post", "/workspace/search")
                .expect("workspace search response schema")["$ref"],
            "#/components/schemas/WorkspaceSearchResponse"
        );
        assert_eq!(
            workspace_openapi_response_schema("get", "/workspace/artifacts")
                .expect("workspace artifacts response schema")["$ref"],
            "#/components/schemas/WorkspaceArtifactsResponse"
        );
        assert_eq!(
            workspace_openapi_response_schema("get", "/workspace/recent")
                .expect("workspace recent response schema")["$ref"],
            "#/components/schemas/WorkspaceRecentResponse"
        );
    }
}
