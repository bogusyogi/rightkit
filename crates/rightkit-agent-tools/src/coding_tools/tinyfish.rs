//! TinyFish REST backend — Search and Fetch.
//!
//! Contract verified against <https://docs.tinyfish.ai> (2026-07-15):
//!
//! ```text
//!   Search: GET  https://api.search.tinyfish.ai/  ?query=<q>  [&page=0..10]
//!   Fetch:  POST https://api.fetch.tinyfish.ai/   {"urls":[..1..10], "format":"markdown"}
//! ```
//!
//! Auth is `X-API-Key` on both. Search and Fetch are free (30 queries/min,
//! 150 URLs/min); TinyFish's Browser and Agent APIs are separate paid products
//! and are deliberately not wired here.
//!
//! Responses are parsed into typed structs, not sniffed out of a `Value`. If
//! TinyFish changes its schema we want a loud parse error, not a silent empty
//! result set.

use serde::Deserialize;

const SEARCH_URL: &str = "https://api.search.tinyfish.ai/";
const FETCH_URL: &str = "https://api.fetch.tinyfish.ai/";
const FETCH_MAX_URLS: usize = 10;
const FETCH_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Canonical resolution order for the TinyFish key, shared by `web_search` and
/// `browser_fetch` so the two tools can never disagree about where it lives.
/// An explicit `[tools.web_search] api_key` wins; otherwise the OS keyring slot
/// that Settings > API Keys writes to; otherwise the environment.
pub fn resolve_api_key(config_key: Option<&str>) -> Result<String, String> {
    if let Some(k) = config_key {
        if !k.trim().is_empty() {
            return Ok(k.to_string());
        }
    }
    if let Some(k) = rightkit_llm::agent_providers::load_api_key("tinyfish") {
        return Ok(k);
    }
    // Keys come from the app's settings (the OS credential store), never the environment.
    let hint = "TinyFish API key not set. Add it under Settings > Providers > TinyFish Search. Search and Fetch are free, get a key at https://agent.tinyfish.ai.";
    Err(hint.to_string())
}

fn client(url: &str) -> Result<rightkit_http::AsyncClient, String> {
    crate::coding_tools::http_client::client_following(url, std::time::Duration::from_secs(30))
        .map_err(|e| format!("TinyFish client build: {e}"))
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchItem>,
}

#[derive(Deserialize)]
struct SearchItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    url: String,
}

/// Search the web. `count` truncates client-side — the API pages (0..10) rather
/// than taking a result count, so we ask for page 0 and cut to `count`.
pub async fn search(
    query: &str,
    count: usize,
    api_key: &str,
) -> Result<Vec<crate::coding_tools::web_search::SearchResult>, String> {
    if api_key.trim().is_empty() {
        return Err("TinyFish API key is empty".to_string());
    }

    let request = rightkit_http::Request::get(format!(
        "{SEARCH_URL}?{}",
        rightkit_http::form_encode(&[("query", query)])
    ))
    .secret_header("X-API-Key", api_key)
    .header("Accept", "application/json");
    let resp = client(SEARCH_URL)?
        .send_any(&request)
        .await
        .map_err(|e| format!("TinyFish search request: {e}"))?;

    let status = resp.status;
    if !(200..300).contains(&status) {
        return Err(format!("TinyFish search HTTP {status}: {}", resp.text()));
    }

    let parsed: SearchResponse = resp
        .json()
        .map_err(|e| format!("TinyFish search response did not match the expected schema: {e}"))?;

    Ok(parsed
        .results
        .into_iter()
        .take(count)
        .map(|item| crate::coding_tools::web_search::SearchResult {
            title: item.title,
            snippet: item.snippet,
            url: item.url,
        })
        .collect())
}

#[derive(Deserialize)]
struct FetchResponse {
    #[serde(default)]
    results: Vec<FetchItem>,
    #[serde(default)]
    errors: Vec<FetchErrorItem>,
}

#[derive(Deserialize)]
struct FetchItem {
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: Option<String>,
    /// Markdown body when `format = "markdown"` (our only request shape).
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct FetchErrorItem {
    #[serde(default)]
    url: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    status: Option<u16>,
}

/// One page fetched and rendered by TinyFish (JS executed server-side).
#[derive(Debug)]
pub struct FetchedPage {
    pub url: String,
    pub title: String,
    pub markdown: String,
}

/// Fetch a single URL as markdown.
pub async fn fetch(api_key: &str, url: &str) -> Result<FetchedPage, String> {
    let mut pages = fetch_batch(api_key, std::slice::from_ref(&url.to_string())).await?;
    if pages.is_empty() {
        return Err(format!("TinyFish returned no content for '{url}'"));
    }
    Ok(pages.remove(0))
}

/// Fetch up to 10 URLs in one request. Per-URL failures come back in the
/// response's `errors` array; a batch where *every* URL failed is an error.
pub async fn fetch_batch(api_key: &str, urls: &[String]) -> Result<Vec<FetchedPage>, String> {
    if api_key.trim().is_empty() {
        return Err("TinyFish API key is empty".to_string());
    }
    if urls.is_empty() {
        return Ok(Vec::new());
    }
    if urls.len() > FETCH_MAX_URLS {
        return Err(format!(
            "TinyFish fetch accepts at most {FETCH_MAX_URLS} URLs per request (got {})",
            urls.len()
        ));
    }

    let request = rightkit_http::Request::post(FETCH_URL)
        .secret_header("X-API-Key", api_key)
        .header("Accept", "application/json")
        .max_response_bytes(FETCH_MAX_RESPONSE_BYTES)
        .json(&serde_json::json!({ "urls": urls, "format": "markdown" }))
        .map_err(|e| format!("TinyFish fetch request: {e}"))?;
    let resp = client(FETCH_URL)?
        .send_any(&request)
        .await
        .map_err(|e| match e {
            rightkit_http::HttpError::BodyTooLarge { .. } => {
                format!("TinyFish fetch response exceeds the {FETCH_MAX_RESPONSE_BYTES}-byte limit")
            }
            other => format!("TinyFish fetch request: {other}"),
        })?;

    let status = resp.status;
    let body = resp.body;
    if !(200..300).contains(&status) {
        return Err(format!(
            "TinyFish fetch HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        ));
    }

    let parsed: FetchResponse = serde_json::from_slice(&body)
        .map_err(|e| format!("TinyFish fetch response did not match the expected schema: {e}"))?;

    if parsed.results.is_empty() && !parsed.errors.is_empty() {
        return Err(format_fetch_errors(&parsed.errors));
    }

    Ok(parsed
        .results
        .into_iter()
        .map(|item| FetchedPage {
            url: item.url,
            title: item.title.unwrap_or_default(),
            markdown: item.text.unwrap_or_default(),
        })
        .collect())
}

fn format_fetch_errors(errors: &[FetchErrorItem]) -> String {
    let detail = errors
        .iter()
        .map(|e| match e.status {
            Some(s) => format!("{} → {} (HTTP {s})", e.url, e.error),
            None => format!("{} → {}", e.url, e.error),
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("TinyFish fetch failed: {detail}")
}
