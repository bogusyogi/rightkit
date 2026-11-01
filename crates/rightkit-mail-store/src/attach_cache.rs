// Disk cache for Gmail attachment BYTES, keyed by (account_id, message_id,
// attachment_id). Backed by RightKit's `rightkit-cache` byte cache: LRU
// eviction under a hard byte cap, atomic writes, and one group per account so
// `clear_account` drops that account's entries without an index file.
//
// Why: attachment previews/downloads/drag-outs re-fetch the full attachment
// bytes from the Gmail API on every open (see attachments::download_attachment
// call sites in engine_api/attachments.rs, which runs in the core process and is the one
// process that reads and writes this cache). A cache hit works with no
// network at all.
//
// Layout: this cache and the remote-image cache are two namespaces of one
// `rightkit_cache::CacheSet` rooted at `<data dir>`, so they share one combined
// byte budget (`SHARED_BUDGET_BYTES`), enforced after every write.
// Attachments live in `<data dir>/attachcache/<shard>/<key>-<account group>`.

use rightkit_cache::{CacheSet, Config, DiskCache, Key};
use std::sync::OnceLock;

/// Namespace (and folder name) of the attachment cache under the data dir.
pub(crate) const ATTACH_NAMESPACE: &str = "attachcache";

/// Combined cap on attachment and remote-image bytes together. Documented here
/// so it's easy to surface as a Settings control later (not built in this lane).
pub const SHARED_BUDGET_BYTES: u64 = 768 * 1024 * 1024; // 768 MB

/// `<data dir>/attachcache`.
#[cfg(test)]
pub(crate) fn cache_root() -> std::path::PathBuf {
    crate::db::data_dir().join(ATTACH_NAMESPACE)
}

/// The one cache set shared by the attachment and remote-image caches.
pub(crate) fn cache_set() -> Result<&'static CacheSet, String> {
    static SET: OnceLock<Result<CacheSet, String>> = OnceLock::new();
    SET.get_or_init(|| {
        CacheSet::open_namespaces(
            crate::db::data_dir(),
            &[
                (ATTACH_NAMESPACE, Config::new(SHARED_BUDGET_BYTES)),
                ("imgcache", Config::new(SHARED_BUDGET_BYTES)),
            ],
        )
        .map(|set| set.with_budget(SHARED_BUDGET_BYTES))
        .map_err(|e| format!("open attachcache/imgcache: {e}"))
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Applies the combined budget after a write to either namespace.
pub(crate) fn enforce_shared_budget() {
    if let Ok(set) = cache_set() {
        set.enforce_budget();
    }
}

fn cache() -> Result<&'static DiskCache, String> {
    cache_set()?
        .get(ATTACH_NAMESPACE)
        .ok_or_else(|| "attachcache namespace missing".to_string())
}

/// The cache slot for one attachment. The account is the group, so
/// `clear_account` can remove every entry for it.
fn key(account_id: &str, message_id: &str, attachment_id: &str) -> Key {
    Key::grouped(&[account_id], &[message_id, attachment_id])
}

/// Fetch-or-cache: read the cache first; on a miss, call `fetch` (the Gmail
/// API download), write the result through to the cache, then return it. On a
/// fetch error the cache is left untouched. Cache failures never fail the
/// caller: a read miss fetches, and a write error just means the next open
/// fetches again.
pub async fn get_or_fetch<F, Fut>(
    account_id: &str,
    message_id: &str,
    attachment_id: &str,
    fetch: F,
) -> Result<Vec<u8>, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    let cache = cache().ok();
    if let Some(cache) = cache {
        let slot = key(account_id, message_id, attachment_id);
        let hit = tokio::task::spawn_blocking(move || cache.get(&slot))
            .await
            .ok()
            .flatten();
        if let Some(bytes) = hit {
            return Ok(bytes);
        }
    }
    let bytes = fetch().await?;
    if let Some(cache) = cache {
        let slot = key(account_id, message_id, attachment_id);
        let payload = bytes.clone();
        // Write-through is best-effort: the caller already has the bytes.
        let _ = tokio::task::spawn_blocking(move || {
            let _ = cache.put(&slot, &payload);
            enforce_shared_budget();
        })
        .await;
    }
    Ok(bytes)
}

/// Remove every cache entry belonging to `account_id`. Called from
/// `remove_account`. Returns how many entries were removed.
pub fn clear_account(account_id: &str) -> Result<usize, String> {
    Ok(cache()?.clear_group(&[account_id]))
}

#[cfg(test)]
mod tests;
