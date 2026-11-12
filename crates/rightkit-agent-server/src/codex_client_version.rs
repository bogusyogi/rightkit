//! Codex client version sent to the ChatGPT model catalog.
//!
//! The backend hides models whose minimum client version exceeds the one we
//! send, so it must track the current `@openai/codex` release without a
//! hard-coded bump per release. Resolution order:
//!
//! 1. `RIGHTKIT_CHATGPT_CLIENT_VERSION` (explicit override) wins;
//! 2. a cached value in `~/.{app}/state/codex-client-version.json`
//!    younger than 24h;
//! 3. the npm registry's `@openai/codex/latest` `version` (5s timeout),
//!    written back to the cache;
//! 4. on fetch failure, the stale cached value;
//! 5. the built-in fallback constant.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const ENV_OVERRIDE: &str = "RIGHTKIT_CHATGPT_CLIENT_VERSION";
pub(crate) const FALLBACK_VERSION: &str = "0.159.2";
const REGISTRY_URL: &str = "https://registry.npmjs.org/@openai/codex/latest";
const CACHE_FILE: &str = "codex-client-version.json";
const FRESH_FOR_SECS: u64 = 24 * 60 * 60;
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

fn cache_path() -> Option<PathBuf> {
    // Same `~/.{app}/state` directory as `model-selection.json`.
    let selection = crate::main_runtime::model_selection_path()?;
    Some(selection.parent()?.join(CACHE_FILE))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Extract the `version` field of an npm "latest" document.
pub(crate) fn parse_npm_version(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = value.get("version")?.as_str()?.trim();
    (!version.is_empty()).then(|| version.to_string())
}

async fn fetch_latest_from_npm() -> Result<String, String> {
    let client = rightkit_agent_daemon::http_client::public_client(FETCH_TIMEOUT)?;
    let response = rightkit_agent_daemon::http_client::send_any(
        &client,
        &rightkit_http::Request::get(REGISTRY_URL),
    )
    .await?;
    if !rightkit_agent_daemon::http_client::is_success(response.status) {
        return Err(format!("npm registry answered HTTP {}", response.status));
    }
    let body = response.text();
    parse_npm_version(&body).ok_or_else(|| "npm response had no version".to_string())
}

/// Production entry point: env + real cache path + clock + npm.
pub(crate) async fn resolve_client_version() -> String {
    let env = std::env::var(ENV_OVERRIDE).ok();
    resolve_with(
        env.as_deref(),
        cache_path().as_deref(),
        now_secs(),
        fetch_latest_from_npm,
    )
    .await
}

/// Testable core: every input (env value, cache path, clock, fetcher) is
/// injected.
pub(crate) async fn resolve_with<F, Fut>(
    env_override: Option<&str>,
    cache: Option<&Path>,
    now: u64,
    fetch: F,
) -> String
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    if let Some(value) = env_override.map(str::trim).filter(|v| !v.is_empty()) {
        return value.to_string();
    }
    let cached = cache.and_then(read_cache);
    if let Some((version, fetched_at)) = &cached {
        if now.saturating_sub(*fetched_at) < FRESH_FOR_SECS {
            return version.clone();
        }
    }
    match fetch().await {
        Ok(version) if !version.trim().is_empty() => {
            let version = version.trim().to_string();
            if let Some(path) = cache {
                write_cache(path, &version, now);
            }
            version
        }
        _ => cached
            .map(|(version, _)| version)
            .unwrap_or_else(|| FALLBACK_VERSION.to_string()),
    }
}

fn read_cache(path: &Path) -> Option<(String, u64)> {
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let version = value.get("version")?.as_str()?.trim().to_string();
    let fetched_at = value.get("fetched_at")?.as_u64()?;
    (!version.is_empty()).then_some((version, fetched_at))
}

fn write_cache(path: &Path, version: &str, now: u64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({ "version": version, "fetched_at": now });
    let _ = std::fs::write(path, body.to_string());
}
