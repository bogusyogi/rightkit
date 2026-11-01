// Workspace module — cancellable search: the shared cancel registry, the
// bounded streaming walker, and the search route handler.
//
// Extracted from `api_workspace.rs` (ra-019 decomposition). Pure structural
// move: cancellation semantics, the scan budget, and the hit cap are
// unchanged —
//   - cancellation: a search carries a request id; a registry stores
//     the in-flight cancel flag per id and a second request with the
//     same id flips the original cancel (cooperative, shared registry)

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use rightkit_job_queue::InlineCancelRegistry;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

use super::policy::{derive_trust_root, is_secret_leaf, looks_binary};
use super::types::{
    WorkspaceSearchHit, WorkspaceSearchRequest, WorkspaceSearchResponse, MAX_TEXT_BYTES,
    SEARCH_BUDGET_BYTES, SEARCH_MAX_HITS,
};

// ── Cancellation registry (shared, by request_id) ─────────────────────────────

/// Process-wide RightKit `InlineCancelRegistry` (rightkit-job-queue 0.4.0). A search
/// carrying a `request_id` that is still in flight is cancelled through `cancel`; the
/// superseding request is then refused (409) until the prior walker drops its guard.
fn search_registry() -> &'static InlineCancelRegistry {
    static REGISTRY: OnceLock<InlineCancelRegistry> = OnceLock::new();
    REGISTRY.get_or_init(InlineCancelRegistry::new)
}

#[derive(Copy, Clone)]
pub(crate) enum Step {
    Continue,
    Stop,
    Cancel,
}

/// Bounded streaming walk. The callback receives one path at a time; the
/// walk never materializes a `Vec<PathBuf>`. Returning `Step::Cancel`
/// stops the walk immediately (used when the cancellation flag is
/// observed mid-iteration); `Step::Stop` ends the walk without flipping
/// the cancellation flag.
pub(crate) fn walk_streaming(
    root: &Path,
    cancel: &Arc<AtomicBool>,
    scanned: &mut u64,
    visit: &mut dyn FnMut(&Path, &mut u64) -> Step,
) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        if *scanned >= SEARCH_BUDGET_BYTES {
            return;
        }
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut children: Vec<PathBuf> = Vec::new();
        for entry in read.flatten() {
            children.push(entry.path());
        }
        // Sort for deterministic ordering and to keep the same workspace
        // yielding the same hits across runs (helps UX, not correctness).
        children.sort();
        for path in children {
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            // Skip hidden dirs (other than "." / "..") during the walk
            // to avoid deep caches like ".git" / "node_modules".
            if name.starts_with('.') && path.is_dir() {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                *scanned = scanned.saturating_add(1);
                match visit(&path, scanned) {
                    Step::Continue => {}
                    Step::Stop => return,
                    Step::Cancel => return,
                }
            }
        }
    }
}

fn unique_request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", rand_u32())
}

fn rand_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    hasher.finish() as u32
}

// ── Search (cancellable, shared registry) ─────────────────────────────────────

pub(crate) async fn post_workspace_search_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Json(body): Json<WorkspaceSearchRequest>,
) -> Result<Json<WorkspaceSearchResponse>, StatusCode> {
    let _ = state;
    enforce_trusted_source_authority(&headers)?;
    if body.query.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.query.len() > 256 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let trust_root = derive_trust_root(&body.workspace_root).map_err(|e| e.status())?;
    let kind = body.kind.clone().unwrap_or_else(|| "content".into());
    let limit = body.limit.unwrap_or(SEARCH_MAX_HITS).min(SEARCH_MAX_HITS);
    let include_binary = body.include_binary.unwrap_or(false);

    // Cancellation: a second arrival with an in-flight request_id flips the
    // prior walker's flag via `cancel`, then this request registers; if the
    // prior walker has not yet dropped its guard, this request gets 409
    // (superseding). Without a request id we generate a unique one so two
    // simultaneous searches don't accidentally cancel each other.
    let request_id = body
        .request_id
        .clone()
        .unwrap_or_else(|| format!("anon-{}", unique_request_id()));
    let registry = search_registry();
    registry.cancel(&request_id);
    let (_inline_guard, cancel) = registry
        .register(&request_id)
        .map_err(|_| StatusCode::CONFLICT)?;

    let query_lower = body.query.to_ascii_lowercase();
    let mut hits: Vec<WorkspaceSearchHit> = Vec::new();
    let mut scanned: u64 = 0;
    let mut truncated = false;
    let mut cancelled = false;

    walk_streaming(
        &trust_root,
        &cancel,
        &mut scanned,
        &mut |path: &Path, scanned_local: &mut u64| {
            if cancel.load(Ordering::Acquire) {
                return Step::Cancel;
            }
            if *scanned_local >= SEARCH_BUDGET_BYTES {
                truncated = true;
                return Step::Stop;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if is_secret_leaf(&name) {
                return Step::Continue;
            }
            if !path.exists() {
                return Step::Continue;
            }
            let metadata = match path.metadata() {
                Ok(m) => m,
                Err(_) => return Step::Continue,
            };
            if metadata.is_dir() {
                return Step::Continue;
            }
            if metadata.len() > MAX_TEXT_BYTES {
                return Step::Continue;
            }
            let rel = match path.strip_prefix(&trust_root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => return Step::Continue,
            };
            let name_lower = name.to_ascii_lowercase();
            let rel_lower = rel.to_ascii_lowercase();
            if (kind == "file" || kind == "all" || kind.is_empty())
                && (name_lower.contains(&query_lower) || rel_lower.contains(&query_lower))
            {
                hits.push(WorkspaceSearchHit {
                    path: rel.clone(),
                    name: name.clone(),
                    line: 0,
                    column: 0,
                    snippet: rel.clone(),
                    kind: "file".into(),
                });
                if hits.len() >= limit {
                    truncated = true;
                    return Step::Stop;
                }
                return Step::Continue;
            }
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(_) => return Step::Continue,
            };
            if looks_binary(&bytes) && !include_binary {
                return Step::Continue;
            }
            *scanned_local = scanned_local.saturating_add(bytes.len() as u64);
            let content = String::from_utf8_lossy(&bytes);
            for (line_idx, line) in content.lines().enumerate() {
                if cancel.load(Ordering::Acquire) {
                    cancelled = true;
                    return Step::Cancel;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(col) = lower.find(&query_lower) {
                    let snippet_start = col.saturating_sub(20);
                    let snippet_end = (col + query_lower.len() + 20).min(line.len());
                    let snippet = line[snippet_start..snippet_end].to_string();
                    hits.push(WorkspaceSearchHit {
                        path: rel.clone(),
                        name: name.clone(),
                        line: line_idx + 1,
                        column: col + 1,
                        snippet,
                        kind: "content".into(),
                    });
                    if hits.len() >= limit {
                        truncated = true;
                        return Step::Stop;
                    }
                }
            }
            if hits.len() >= limit {
                truncated = true;
                return Step::Stop;
            }
            Step::Continue
        },
    );

    // Honest cancelled semantics: the request was cancelled only if the
    // walker did not run to completion AND we returned with zero (or
    // partial) hits as a direct consequence. We never tag a successful
    // full scan as cancelled just because the flag was set after the
    // walk finished.
    let was_cancelled_during = cancel.load(Ordering::Acquire) && (cancelled || hits.is_empty());

    Ok(Json(WorkspaceSearchResponse {
        workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
        query: body.query,
        request_id: Some(request_id),
        hits,
        cancelled: was_cancelled_during,
        scanned_bytes: scanned,
        truncated,
    }))
}
