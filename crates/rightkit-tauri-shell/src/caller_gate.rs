//! Who may invoke a privileged shell command: an app webview, never content.
//!
//! A command that reads diagnostics, grants files or (later) touches licence and update state must
//! not be callable from the webview that renders opened content, nor from a page that is not the
//! app's own. [`require_app_caller`] is the one check. It takes the invoking [`Webview`] (a command
//! parameter `webview: Webview<Wry>`) and answers [`CallerError::Forbidden`] unless all of these hold:
//!
//! 1. the shell is installed (fail closed: without [`ShellState`] nothing is known);
//! 2. the caller's label is the shell's main webview or a top-level webview window of the app, so a
//!    child webview that merely shares a window is not an app webview;
//! 3. the label matches no registered content webview ([`crate::DevToolsGate::content_labels`], exact
//!    labels and `prefix-*` patterns), the registry the shell already keeps for DevTools and
//!    hardening;
//! 4. the webview's current URL is the app's own origin: a page the navigation policy
//!    ([`crate::NavPolicy`]) serves in place, which is not an `about:`, `blob:`, `data:`, `ipc:`,
//!    `asset:` or `http-ipc:` document. An unreadable URL is refused.
//!
//! The decision is pure ([`decide`]) so every host tests it; the Tauri adapter only gathers facts.
//! A forbidden call converts to [`ShellError::PermissionDenied`] with a `forbidden:` message.

use std::sync::Arc;

use tauri::{Manager, Url, Webview, Wry};

use crate::devtools::glob_match;
use crate::error::ShellError;
use crate::nav::{NavDecision, NavPolicy};
use crate::ShellState;

/// Why a caller was refused. The text is for logs and the rejection message; it names no secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// The shell's state is not managed, so no registry exists to check against.
    ShellNotInstalled,
    /// The label is not a top-level webview window of the app.
    NotAnAppWebview,
    /// The label is a registered content webview.
    ContentWebview,
    /// The webview's URL could not be read.
    UnknownOrigin,
    /// The webview shows a page that is not the app's own origin.
    ForeignOrigin,
    /// No `crate::host::HostServices` was installed, so no per-service trust is known.
    HostServicesNotInstalled,
    /// The service was never registered with the host, or the app declared it with no trust.
    ServiceNotRegistered,
    /// The caller's label is not one of the labels the service trusts.
    UntrustedLabel,
    /// The caller's origin is not one of the origins the service trusts.
    UntrustedOrigin,
    /// The service does not enable the requested action.
    ActionNotEnabled,
    /// The webview shows a preview document (one of the schemes registered with
    /// `ShellBuilder::preview_protocol`). A preview is untrusted content and gets no privileged IPC,
    /// even where the navigation policy serves its scheme in place.
    PreviewDocument,
}

impl Denial {
    pub fn reason(self) -> &'static str {
        match self {
            Self::ShellNotInstalled => "the shell is not installed",
            Self::NotAnAppWebview => "the caller is not an app webview",
            Self::ContentWebview => "the caller is a content webview",
            Self::UnknownOrigin => "the caller's origin is unknown",
            Self::ForeignOrigin => "the caller's origin is not the app's own",
            Self::HostServicesNotInstalled => "host services are not installed",
            Self::ServiceNotRegistered => "the service is not registered",
            Self::UntrustedLabel => "the caller's window is not trusted by the service",
            Self::UntrustedOrigin => "the caller's origin is not trusted by the service",
            Self::ActionNotEnabled => "the action is not enabled for the service",
            Self::PreviewDocument => "the caller is a preview document",
        }
    }
}

/// The caller check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerError {
    Forbidden(Denial),
}

impl std::fmt::Display for CallerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Forbidden(denial) = self;
        write!(f, "forbidden: {}", denial.reason())
    }
}

impl std::error::Error for CallerError {}

impl From<CallerError> for ShellError {
    fn from(e: CallerError) -> Self {
        ShellError::PermissionDenied(e.to_string())
    }
}

/// Schemes the navigation policy serves in place that are still not an app page: documents a page
/// can fabricate for itself, and Tauri's own transport schemes.
const NOT_AN_APP_PAGE: &[&str] = &["about", "blob", "data", "ipc", "asset", "http-ipc"];

/// True when `url` is a page of the app itself under `nav`.
pub fn is_app_origin(nav: &NavPolicy, url: &Url) -> bool {
    !NOT_AN_APP_PAGE.contains(&url.scheme()) && matches!(nav.decide(url), NavDecision::Allow)
}

/// True when `label` equals a registered content label or matches a `prefix-*` pattern.
pub fn is_content_label(content_labels: &[String], label: &str) -> bool {
    content_labels
        .iter()
        .any(|pattern| glob_match(pattern, label))
}

/// What the check knows about a caller.
#[derive(Debug, Clone, Copy)]
pub struct CallerFacts<'a> {
    pub label: &'a str,
    /// The label names a top-level webview window of the app.
    pub is_app_window: bool,
    /// Registered content webview labels or patterns.
    pub content_labels: &'a [String],
    /// The webview's current URL; `None` when it could not be read.
    pub url: Option<&'a Url>,
    pub nav: &'a NavPolicy,
}

/// The decision. Order matters only for the reason reported: any failed rule refuses the call.
pub fn decide(facts: &CallerFacts<'_>) -> Result<(), CallerError> {
    if !facts.is_app_window {
        return Err(CallerError::Forbidden(Denial::NotAnAppWebview));
    }
    if is_content_label(facts.content_labels, facts.label) {
        return Err(CallerError::Forbidden(Denial::ContentWebview));
    }
    match facts.url {
        None => Err(CallerError::Forbidden(Denial::UnknownOrigin)),
        Some(url) if is_app_origin(facts.nav, url) => Ok(()),
        Some(_) => Err(CallerError::Forbidden(Denial::ForeignOrigin)),
    }
}

/// The facts about one invocation, owned so they outlive the webview borrow. Every field comes
/// from Tauri's native invocation context (the `Webview` the IPC layer injected) and the shell's
/// own registries; none is read from the renderer's arguments.
#[derive(Debug, Clone)]
pub struct OwnedCallerFacts {
    pub label: String,
    pub is_app_window: bool,
    pub content_labels: Vec<String>,
    pub url: Option<Url>,
    pub nav: Arc<NavPolicy>,
}

impl OwnedCallerFacts {
    /// The borrowed view [`decide`] and the host policy take.
    pub fn facts(&self) -> CallerFacts<'_> {
        CallerFacts {
            label: &self.label,
            is_app_window: self.is_app_window,
            content_labels: &self.content_labels,
            url: self.url.as_ref(),
            nav: &self.nav,
        }
    }
}

/// Read the invoking webview's identity from the native context. Fails closed
/// ([`Denial::ShellNotInstalled`]) when the shell's state is not managed.
pub fn gather(webview: &Webview<Wry>) -> Result<OwnedCallerFacts, CallerError> {
    let app = webview.app_handle();
    let Some(state) = app.try_state::<ShellState>() else {
        return Err(CallerError::Forbidden(Denial::ShellNotInstalled));
    };
    let label = webview.label();
    let url = webview.url().ok();
    // A webview whose page is a preview document is untrusted content: refused before any
    // service rule, whatever the navigation policy says about the scheme.
    if let Some(origins) = app.try_state::<crate::preview::PreviewOrigins>() {
        refuse_preview_document(&origins, url.as_ref())?;
    }
    Ok(OwnedCallerFacts {
        label: label.to_owned(),
        is_app_window: label == state.main_label || app.webview_windows().contains_key(label),
        content_labels: app
            .try_state::<crate::DevToolsGate>()
            .map(|gate| gate.content_labels())
            .unwrap_or_default(),
        url,
        nav: Arc::clone(&state.nav),
    })
}

/// Refuse a caller whose current page is a document served by one of the app's preview schemes
/// ([`crate::preview::PreviewOrigins`]): a preview document is untrusted content and never reaches a
/// privileged command, whatever label it was given and whatever the navigation policy says about its
/// scheme. A webview whose URL could not be read passes here (the origin rule refuses it later).
pub fn refuse_preview_document(
    origins: &crate::preview::PreviewOrigins,
    url: Option<&Url>,
) -> Result<(), CallerError> {
    match url {
        Some(page) if origins.is_preview_url(page) => {
            Err(CallerError::Forbidden(Denial::PreviewDocument))
        }
        _ => Ok(()),
    }
}

/// Refuse the invoking webview unless it is an app webview registered by the shell (see the module
/// documentation). Call it first in every privileged command.
pub fn require_app_caller(webview: &Webview<Wry>) -> Result<(), CallerError> {
    let owned = gather(webview)?;
    decide(&owned.facts())
}

/// The origin of `url` as `scheme://host[:port]`, lower-cased by the URL parser. Unlike
/// `Url::origin`, it is also defined for the custom schemes Tauri serves the app from
/// (`tauri://localhost`), which the URL standard treats as opaque. `None` for a URL with no host
/// (`about:`, `data:`, `file:` without a host).
pub fn origin_of(url: &Url) -> Option<String> {
    let host = url.host_str().filter(|h| !h.is_empty())?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}
