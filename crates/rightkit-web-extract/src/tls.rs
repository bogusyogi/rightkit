//! The TLS-fingerprint tier: fetch a page through the native tls-client shared library, which
//! presents a browser-like TLS handshake. The library is third-party code loaded from the
//! install bundle; every pointer it returns is treated as untrusted (see [`crate::tls_ffi`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::filters::apply_article_filters;
use crate::net::ArticleNet;
use crate::parse::{extract_title, readable_text, resolve_redirect, resource_policy};
use crate::tls_ffi::call_tls_client_request;
use crate::{
    ArticleExtract, ArticleSource, FetchMetadata, MAX_ARTICLE_HTML_BYTES, MAX_ARTICLE_REDIRECTS,
};

/// One tls-client response.
#[derive(Debug, serde::Deserialize)]
pub struct TlsClientResponse {
    pub id: String,
    pub status: u16,
    pub body: String,
    pub headers: Option<HashMap<String, Vec<String>>>,
}

/// Where the tls-client library lives and whether a path may be loaded.
#[derive(Clone)]
pub struct TlsClientConfig {
    /// Environment variable that overrides the library path (for example
    /// `RIGHTKIT_TLS_CLIENT_DLL`).
    pub env_var: String,
    /// File name of the platform's tls-client library.
    pub library_name: String,
    /// The app data root; `<root>/runtime/tls-client/<library_name>` is the last candidate.
    pub app_data_root: Option<PathBuf>,
    /// Extra candidate paths searched first after the override (developer builds).
    pub extra_candidates: Vec<PathBuf>,
    /// Whether a library path may be loaded into the process (for example "inside the installed
    /// bundle"). A refused path is never loaded.
    pub trusted: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
    /// Apply the pruning and BM25 fit filters to fetched text.
    pub fit_filter: bool,
}

impl TlsClientConfig {
    /// The library path: the override variable when it points at an existing file, else the
    /// first existing candidate.
    pub fn library_path(&self) -> Option<PathBuf> {
        if let Some(path) = std::env::var_os(&self.env_var) {
            let path = PathBuf::from(path);
            if path.exists() {
                return Some(path);
            }
        }
        self.library_candidates()
            .into_iter()
            .find(|candidate| candidate.exists())
    }

    /// Every place the library is searched for, in order.
    pub fn library_candidates(&self) -> Vec<PathBuf> {
        let mut candidates = self.extra_candidates.clone();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(
                    dir.join("resources")
                        .join("runtime")
                        .join("tls-client")
                        .join(&self.library_name),
                );
                candidates.push(
                    dir.join("..")
                        .join("resources")
                        .join("runtime")
                        .join("tls-client")
                        .join(&self.library_name),
                );
                candidates.push(dir.join(&self.library_name));
            }
        }
        if let Some(root) = &self.app_data_root {
            candidates.push(
                root.join("runtime")
                    .join("tls-client")
                    .join(&self.library_name),
            );
        }
        candidates
    }
}

/// Fetch an article through the tls-client library, following redirects by hand so every hop is
/// vetted by `net` before the native client connects.
pub fn fetch_article_tls_fingerprint(
    net: &dyn ArticleNet,
    config: &TlsClientConfig,
    url: &str,
) -> Result<ArticleExtract, String> {
    net.ensure_public_http_url(url)?;
    let dll = config.library_path().ok_or_else(|| {
        format!(
            "tls-client shared library not found; set {} or package runtime",
            config.env_var
        )
    })?;
    let started = Instant::now();
    let mut current = url.to_string();
    let mut redirects = 0;
    let response = loop {
        // The native client does its own DNS: resolve and vet every address (first request
        // and every redirect hop) before it connects. A refusal ends the whole fetch.
        net.vet_resolved_host(&current)?;
        let response = fetch_tls_client_once(&dll, &current, &*config.trusted)?;
        if (300..400).contains(&response.status) {
            redirects += 1;
            if redirects > MAX_ARTICLE_REDIRECTS {
                return Err("tls-client article fetch: too many redirects".to_string());
            }
            let location = header_value(&response.headers, "location").ok_or_else(|| {
                format!(
                    "tls-client redirect {} without a Location header",
                    response.status
                )
            })?;
            let next = resolve_redirect(&current, &location)?;
            net.ensure_public_http_url(&next)?;
            current = next;
            continue;
        }
        break response;
    };
    if !(200..400).contains(&response.status) {
        return Err(format!(
            "tls-client article fetch returned status {}: {}",
            response.status,
            response.body.chars().take(240).collect::<String>()
        ));
    }
    let content_type = header_value(&response.headers, "content-type");
    if !content_type
        .as_deref()
        .unwrap_or("text/html")
        .to_ascii_lowercase()
        .contains("html")
    {
        return Err(format!(
            "tls-client article fetch returned non-html content: {}",
            content_type.unwrap_or_else(|| "unknown".to_string())
        ));
    }
    enforce_article_body_limit(response.body.len())?;
    let bytes_read = response.body.len();
    let title = extract_title(&response.body);
    let text = readable_text(&response.body)?;
    let (pruned_text, fit_text) = apply_article_filters(&text, config.fit_filter);
    Ok(ArticleExtract {
        title,
        text: pruned_text,
        fit_text: Some(fit_text),
        source: ArticleSource::TlsFingerprint,
        metadata: FetchMetadata {
            url: current,
            status: Some(response.status),
            content_type,
            elapsed_ms: started.elapsed().as_millis(),
            bytes_read,
            resource_policy: resource_policy(url),
        },
        html: Some(response.body),
    })
}

/// The tls-client tier holds the body in memory, so it applies the same article cap as the
/// direct tier (`MAX_ARTICLE_HTML_BYTES`) instead of the much larger FFI response cap.
pub fn enforce_article_body_limit(len: usize) -> Result<(), String> {
    if len as u64 > MAX_ARTICLE_HTML_BYTES {
        return Err(format!(
            "the response body is larger than request limit: {MAX_ARTICLE_HTML_BYTES}"
        ));
    }
    Ok(())
}

/// One GET through the library at `dll`.
pub fn fetch_tls_client_once(
    dll: &Path,
    url: &str,
    trusted: &dyn Fn(&Path) -> bool,
) -> Result<TlsClientResponse, String> {
    let payload = serde_json::json!({
        "tlsClientIdentifier": "chrome_146",
        "followRedirects": false,
        "insecureSkipVerify": false,
        "withoutCookieJar": false,
        "withCustomCookieJar": false,
        "isByteRequest": false,
        "isByteResponse": false,
        "catchPanics": true,
        "withDebug": false,
        "forceHttp1": false,
        "disableHttp3": true,
        "withRandomTLSExtensionOrder": false,
        "timeoutSeconds": 20,
        "timeoutMilliseconds": 0,
        "proxyUrl": "",
        "isRotatingProxy": false,
        "certificatePinningHosts": {},
        "headers": {
            "accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8",
            "accept-language": "en-US,en;q=0.9",
            "cache-control": "no-cache",
            "sec-fetch-dest": "document",
            "sec-fetch-mode": "navigate",
            "sec-fetch-site": "none",
            "sec-fetch-user": "?1",
            "upgrade-insecure-requests": "1",
            "user-agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36"
        },
        "headerOrder": [
            "accept",
            "accept-language",
            "cache-control",
            "sec-fetch-dest",
            "sec-fetch-mode",
            "sec-fetch-site",
            "sec-fetch-user",
            "upgrade-insecure-requests",
            "user-agent"
        ],
        "requestUrl": url,
        "requestMethod": "GET",
        "requestBody": "",
        "requestCookies": []
    });
    // SAFETY: `dll` is a tls-client library the host marked loadable (`trusted`); every pointer
    // it returns is bounded and validated inside `call_tls_client_request`.
    unsafe { call_tls_client_request(dll, &payload.to_string(), trusted) }
        .map_err(|e| format!("tls-client article fetch failed: {e}"))
}

/// The first value of header `name` (case-insensitive).
pub fn header_value(headers: &Option<HashMap<String, Vec<String>>>, name: &str) -> Option<String> {
    let headers = headers.as_ref()?;
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, values)| values.first())
        .cloned()
}

#[cfg(test)]
mod body_limit_tests {
    use super::{enforce_article_body_limit, MAX_ARTICLE_HTML_BYTES};

    #[test]
    fn body_at_the_article_cap_is_accepted() {
        assert!(enforce_article_body_limit(MAX_ARTICLE_HTML_BYTES as usize).is_ok());
    }

    #[test]
    fn body_over_the_article_cap_is_refused_with_the_shared_text() {
        let err = enforce_article_body_limit(MAX_ARTICLE_HTML_BYTES as usize + 1).unwrap_err();
        assert!(err.starts_with("the response body is larger than request limit"));
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn config() -> TlsClientConfig {
        TlsClientConfig {
            env_var: "RIGHTKIT_TLS_CLIENT_DLL_TEST_UNSET".to_string(),
            library_name: "tls-client-test.lib".to_string(),
            app_data_root: Some(std::env::temp_dir().join("rightkit-tls-config-test")),
            extra_candidates: vec![std::env::temp_dir().join("rightkit-tls-extra.lib")],
            trusted: Arc::new(|_| true),
            fit_filter: true,
        }
    }

    #[test]
    fn candidates_keep_extra_first_and_app_data_last() {
        let candidates = config().library_candidates();
        assert_eq!(
            candidates.first(),
            Some(&std::env::temp_dir().join("rightkit-tls-extra.lib"))
        );
        let tail = Path::new("runtime")
            .join("tls-client")
            .join("tls-client-test.lib");
        assert!(candidates.last().is_some_and(|p| p.ends_with(&tail)));
    }

    #[test]
    fn missing_library_resolves_to_none() {
        assert!(config().library_path().is_none());
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let mut map = HashMap::new();
        map.insert("Content-Type".to_string(), vec!["text/html".to_string()]);
        assert_eq!(
            header_value(&Some(map), "content-type").as_deref(),
            Some("text/html")
        );
    }
}
