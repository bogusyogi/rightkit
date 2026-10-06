//! Navigation and window-open policy: what a webview may load in place, what leaves for the
//! system browser, and what is refused.
//!
//! Merges ViewRight's `external_navigation` broker (an `https://open.<app>.invalid/?target=`
//! hop that the frontend uses to ask the host to open a link) with CodeRight's `popup_policy`
//! (web schemes leave the shell; only a size-shaped OAuth popup may become a child window).
//! The decision functions are pure so every platform shares one policy.

use serde::{Deserialize, Serialize};
use tauri::Url;

/// Schemes that may leave the shell for the system handler without a second consent step.
pub const EXTERNAL_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// Schemes a webview loads in place (the app's own content and in-page documents).
const INTERNAL_SCHEMES: &[&str] = &["tauri", "asset", "ipc", "about", "blob", "data", "http-ipc"];

/// OAuth authorization origins allowed to open as an in-app popup (CodeRight list).
pub const OAUTH_POPUP_ORIGINS: &[&str] = &[
    "https://github.com",
    "https://accounts.google.com",
    "https://slack.com",
    "https://login.microsoftonline.com",
    "https://appleid.apple.com",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NavDecision {
    /// Load in the webview.
    Allow,
    /// Hand to the system handler (browser, mail client); the webview stays put.
    External { url: String },
    /// Refuse (unknown or dangerous scheme).
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WindowOpenDecision {
    External {
        url: String,
    },
    /// An allow-listed popup (OAuth) that may become an in-app child window.
    Popup,
    Deny,
}

#[derive(Debug, Clone)]
pub struct NavPolicy {
    /// Extra hosts served in place, e.g. an app's own `https://app.example` surface.
    pub internal_hosts: Vec<String>,
    /// Allow `localhost` / loopback (Vite dev server). Defaults to debug builds only.
    pub allow_loopback: bool,
    /// ViewRight-style broker host. `https://<host>/?target=<url>` navigates nothing and asks the
    /// shell to open `target` externally (only when `target` has an external scheme).
    pub broker_host: Option<String>,
    /// Origins (scheme://host[:port]) allowed to open popup windows in-app.
    pub popup_origins: Vec<String>,
}

impl Default for NavPolicy {
    fn default() -> Self {
        Self {
            internal_hosts: Vec::new(),
            allow_loopback: cfg!(debug_assertions),
            broker_host: None,
            popup_origins: OAUTH_POPUP_ORIGINS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

pub fn external_scheme_allowed(target: &str) -> bool {
    let lower = target.trim_start().to_ascii_lowercase();
    EXTERNAL_SCHEMES
        .iter()
        .any(|s| lower.starts_with(&format!("{s}:")))
        && !target.chars().any(char::is_control)
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// Origin serialisation matching JavaScript's `URL#origin` for http(s).
pub fn origin_of(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let default = match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    let port = url.port().filter(|p| Some(*p) != default);
    Some(match port {
        Some(p) => format!("{}://{host}:{p}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

impl NavPolicy {
    fn serves_in_place(&self, url: &Url) -> bool {
        let scheme = url.scheme();
        if INTERNAL_SCHEMES.contains(&scheme) {
            return true;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        // Tauri serves the bundle from `tauri.localhost` on Windows/Android.
        if matches!(scheme, "http" | "https") && host == "tauri.localhost" {
            return true;
        }
        if self.internal_hosts.iter().any(|h| h == host) && matches!(scheme, "http" | "https") {
            return true;
        }
        self.allow_loopback && matches!(scheme, "http" | "https") && is_loopback(host)
    }

    /// Decide a top-level (or `on_navigation`) load.
    pub fn decide(&self, url: &Url) -> NavDecision {
        if let Some(host) = &self.broker_host {
            if url.scheme() == "https" && url.host_str() == Some(host.as_str()) {
                let target = url
                    .query_pairs()
                    .find_map(|(k, v)| (k == "target").then(|| v.into_owned()));
                return match target {
                    Some(t) if external_scheme_allowed(&t) => NavDecision::External { url: t },
                    _ => NavDecision::Block,
                };
            }
        }
        if self.serves_in_place(url) {
            return NavDecision::Allow;
        }
        if EXTERNAL_SCHEMES.contains(&url.scheme()) {
            return NavDecision::External {
                url: url.to_string(),
            };
        }
        NavDecision::Block
    }

    /// Decide a page-initiated `window.open` / `target=_blank`.
    /// `popup_shaped` is true for a sized popup request; only those from a pinned origin to an
    /// allow-listed HTTPS origin become windows, everything else leaves for the browser.
    pub fn decide_window_open(
        &self,
        url: &Url,
        popup_shaped: bool,
        opener: Option<&Url>,
    ) -> WindowOpenDecision {
        if !EXTERNAL_SCHEMES.contains(&url.scheme()) {
            return WindowOpenDecision::Deny;
        }
        if popup_shaped && url.scheme() == "https" {
            let target = origin_of(url);
            // Wry reports only the target; when the opener is unknown the origin allow-list decides.
            let from_trusted = opener.is_none_or(|o| self.serves_in_place(o));
            if from_trusted && target.is_some_and(|t| self.popup_origins.contains(&t)) {
                return WindowOpenDecision::Popup;
            }
        }
        WindowOpenDecision::External {
            url: url.to_string(),
        }
    }
}
