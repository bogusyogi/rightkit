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

/// OAuth authorization origins allowed to open as an in-app popup (CodeRight list, plus the Atlassian
/// and Salesforce sign-in origins). An app adds its own through [`NavPolicy::popup_origins`] or
/// [`popup_origins_from_json`].
pub const OAUTH_POPUP_ORIGINS: &[&str] = &[
    "https://github.com",
    "https://accounts.google.com",
    "https://slack.com",
    "https://login.microsoftonline.com",
    "https://appleid.apple.com",
    "https://mcp.atlassian.com",
    "https://auth.atlassian.com",
    "https://login.salesforce.com",
    "https://test.salesforce.com",
];

/// Headers removed from an OAuth popup's response: a cross-origin opener policy severs `window.opener`,
/// which breaks the OAuth `postMessage` hand-back.
const COOP_HEADERS: &[&str] = &[
    "cross-origin-opener-policy",
    "cross-origin-opener-policy-report-only",
];

/// Remove the cross-origin opener policy headers (case-insensitively) from a response's header map.
/// `None` in, `None` out. For a webview response hook on an OAuth popup.
pub fn strip_coop_headers(
    headers: Option<&std::collections::BTreeMap<String, Vec<String>>>,
) -> Option<std::collections::BTreeMap<String, Vec<String>>> {
    headers.map(|h| {
        h.iter()
            .filter(|(name, _)| !COOP_HEADERS.contains(&name.to_ascii_lowercase().as_str()))
            .map(|(name, values)| (name.clone(), values.clone()))
            .collect()
    })
}

/// Parse an app-settings `popup_allowed_origins` value (a JSON array of origin strings) into popup
/// origins. Invalid entries are ignored: anything that is not a bare `https://host[:port]` origin in
/// canonical form, or a non-array value, yields no entry. Duplicates are dropped.
pub fn popup_origins_from_json(raw: &str) -> Vec<String> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let Some(text) = item.as_str() else { continue };
        let Ok(url) = Url::parse(text) else { continue };
        if url.scheme() != "https" || origin_of(&url).as_deref() != Some(text) {
            continue;
        }
        if !out.iter().any(|o| o == text) {
            out.push(text.to_string());
        }
    }
    out
}

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
    /// Allow `localhost` / loopback (Vite dev server). Defaults on only with the `dev-overrides` feature.
    pub allow_loopback: bool,
    /// ViewRight-style broker host. `https://<host>/?target=<url>` navigates nothing and asks the
    /// shell to open `target` externally (only when `target` has an external scheme).
    pub broker_host: Option<String>,
    /// Origins (scheme://host`[:port]`) allowed to open popup windows in-app.
    pub popup_origins: Vec<String>,
    /// Extra schemes the host serves as its own content, loaded in place like `tauri:` (a custom
    /// protocol registered by the app). Empty by default; add with [`NavPolicy::with_internal_scheme`].
    pub internal_schemes: Vec<String>,
}

/// Schemes no host may mark internal: web and script schemes would let the webview load the world,
/// and `file:` the disk, in place.
const NEVER_INTERNAL: &[&str] = &[
    "http",
    "https",
    "mailto",
    "file",
    "javascript",
    "vbscript",
    "ftp",
    "ws",
    "wss",
];

/// `Ok(lowercased scheme)` when `scheme` can be treated as internal content.
pub fn validate_internal_scheme(scheme: &str) -> Result<String, String> {
    let lower = scheme.to_ascii_lowercase();
    let mut chars = lower.chars();
    let well_formed = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'));
    if !well_formed {
        return Err(format!("{scheme:?} is not a URL scheme"));
    }
    if NEVER_INTERNAL.contains(&lower.as_str()) {
        return Err(format!("{lower}: cannot be an internal scheme"));
    }
    Ok(lower)
}

impl Default for NavPolicy {
    fn default() -> Self {
        Self {
            internal_hosts: Vec::new(),
            allow_loopback: cfg!(feature = "dev-overrides"),
            broker_host: None,
            popup_origins: OAUTH_POPUP_ORIGINS.iter().map(|s| s.to_string()).collect(),
            internal_schemes: Vec::new(),
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
    /// This policy with `scheme` also loaded in place (see [`NavPolicy::internal_schemes`]). Errors
    /// on a malformed scheme or a web, script or file scheme. The built-in set is unchanged.
    pub fn with_internal_scheme(mut self, scheme: &str) -> Result<Self, String> {
        let scheme = validate_internal_scheme(scheme)?;
        if !self.internal_schemes.contains(&scheme) {
            self.internal_schemes.push(scheme);
        }
        Ok(self)
    }

    fn serves_in_place(&self, url: &Url) -> bool {
        let scheme = url.scheme();
        if INTERNAL_SCHEMES.contains(&scheme) || self.internal_schemes.iter().any(|s| s == scheme) {
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
    /// `popup_shaped` is true for a sized popup request; only those from a trusted opener to an
    /// allow-listed HTTPS origin become windows, everything else leaves for the browser. A non-web
    /// scheme is `Deny`; ask [`NavPolicy::protocol_consent_scheme`] whether it needs person consent.
    pub fn decide_window_open(
        &self,
        url: &Url,
        popup_shaped: bool,
        opener: Option<&Url>,
    ) -> WindowOpenDecision {
        self.decide_window_open_pinned(url, popup_shaped, opener, None)
    }

    /// [`NavPolicy::decide_window_open`] with the app's pinned server origin (`scheme://host[:port]`).
    /// When `pinned_origin` is `Some`, a popup opens only when the opener's origin equals it (an unknown
    /// opener is external); when `None`, an unknown opener defers to the origin allow-list and a known
    /// one must be served in place.
    pub fn decide_window_open_pinned(
        &self,
        url: &Url,
        popup_shaped: bool,
        opener: Option<&Url>,
        pinned_origin: Option<&str>,
    ) -> WindowOpenDecision {
        if !EXTERNAL_SCHEMES.contains(&url.scheme()) {
            return WindowOpenDecision::Deny;
        }
        if popup_shaped && url.scheme() == "https" {
            let target = origin_of(url);
            // Wry reports only the target.
            let from_trusted = match pinned_origin {
                Some(pin) => opener.and_then(origin_of).as_deref() == Some(pin),
                None => opener.is_none_or(|o| self.serves_in_place(o)),
            };
            if from_trusted && target.is_some_and(|t| self.popup_origins.contains(&t)) {
                return WindowOpenDecision::Popup;
            }
        }
        WindowOpenDecision::External {
            url: url.to_string(),
        }
    }

    /// The scheme of a non-web `window.open` target (for example `zoom:` or `ms-settings:`) that an app
    /// with a consent surface may confirm with the person before handing it to the system. `None` for
    /// web schemes and for schemes the shell serves or refuses outright (internal, `file:`, `javascript:`).
    /// [`NavPolicy::decide_window_open`] returns `Deny` for every such target either way.
    pub fn protocol_consent_scheme(&self, url: &Url) -> Option<String> {
        let scheme = url.scheme();
        if EXTERNAL_SCHEMES.contains(&scheme)
            || INTERNAL_SCHEMES.contains(&scheme)
            || NEVER_INTERNAL.contains(&scheme)
            || self.internal_schemes.iter().any(|s| s == scheme)
        {
            return None;
        }
        Some(scheme.to_string())
    }
}
