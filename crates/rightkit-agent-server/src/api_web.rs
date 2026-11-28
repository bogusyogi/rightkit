//! The browser client (`{app} web`): the static files a host supplies, served by the daemon
//! under `/web`.
//!
//! The files are the host's data, not this crate's: a host passes them as a `&'static` slice
//! (`host_options::set_web_client_assets`), each as `(route path, content type, body)`, and with
//! an empty slice (the default) no `/web` route exists. A route path is `/web` (the page) or
//! `/web/<file>` with a plain file name; anything else is skipped with a warning, so a host can
//! never mount a file outside that prefix.
//!
//! These routes are the only static files the daemon serves. They hold no data, but on the owner
//! listener they sit behind the same bearer as every other route (the loopback host checks it
//! before the router runs; only the trigger delivery routes are public there). The LAN listener
//! (guest links) answers exactly these paths. The page reads the daemon token from its URL
//! fragment (which a browser never sends to a server), keeps it in `sessionStorage`, and calls
//! the ordinary API with `Authorization: Bearer`. Every API call stays behind `require_token`.

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

/// No inline script, no third-party origin, no framing, no referrer.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
frame-ancestors 'none'";

fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        body,
    )
}

/// `/web`, or `/web/` followed by a file name of letters, digits, `.`, `-` and `_` (no `..`).
fn valid_asset_path(path: &str) -> bool {
    if path == "/web" {
        return true;
    }
    path.strip_prefix("/web/").is_some_and(|name| {
        !name.is_empty()
            && !name.contains("..")
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    })
}

/// Whether this host serves a browser client at all.
pub(crate) fn web_client_enabled() -> bool {
    !crate::host_options::web_client_assets().is_empty()
}

/// Whether `path` is one of the files this host declared (the LAN listener answers exactly
/// these).
pub(crate) fn is_web_path(path: &str) -> bool {
    valid_asset_path(path)
        && crate::host_options::web_client_assets()
            .iter()
            .any(|(declared, _, _)| *declared == path)
}

/// The routes of the files the host declared; empty when it declared none.
pub(crate) fn web_routes() -> Router {
    let mut router = Router::new();
    for &(path, content_type, body) in crate::host_options::web_client_assets() {
        if !valid_asset_path(path) {
            tracing::warn!(path, "web client file skipped: not under /web");
            continue;
        }
        router = router.route(path, get(move || async move { asset(content_type, body) }));
    }
    router
}

/// The page URL for a daemon at `base_url`, with the token in the fragment.
pub(crate) fn web_url(base_url: &str, token: &str) -> String {
    format!("{}/web#token={token}", base_url.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::{valid_asset_path, web_url};

    #[test]
    fn only_the_page_and_plain_files_under_web_are_mountable() {
        assert!(valid_asset_path("/web"));
        assert!(valid_asset_path("/web/app.js"));
        assert!(valid_asset_path("/web/app-2_x.css"));
        assert!(!valid_asset_path("/"));
        assert!(!valid_asset_path("/web/"));
        assert!(!valid_asset_path("/web/../sessions"));
        assert!(!valid_asset_path("/web/a/b.js"));
        assert!(!valid_asset_path("/web/{id}"));
        assert!(!valid_asset_path("/health"));
        assert!(!valid_asset_path("/webx"));
    }

    #[test]
    fn the_token_rides_the_fragment() {
        assert_eq!(
            web_url("http://127.0.0.1:7/", "t"),
            "http://127.0.0.1:7/web#token=t"
        );
    }
}
