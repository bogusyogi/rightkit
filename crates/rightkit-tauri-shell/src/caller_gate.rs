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
    /// No [`crate::host::HostServices`] was installed, so no per-service trust is known.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn facts<'a>(
        label: &'a str,
        is_app_window: bool,
        content: &'a [String],
        url: Option<&'a Url>,
        nav: &'a NavPolicy,
    ) -> CallerFacts<'a> {
        CallerFacts {
            label,
            is_app_window,
            content_labels: content,
            url,
            nav,
        }
    }

    #[test]
    fn a_preview_document_is_refused_before_any_service_rule() {
        let origins = crate::preview::PreviewOrigins::new(["demo-preview".to_owned()]);
        let refused = CallerError::Forbidden(Denial::PreviewDocument);
        // Both URL forms a platform serves the scheme under, and a mixed-case host.
        for page in [
            "demo-preview://localhost/abc",
            "DEMO-PREVIEW://localhost/abc",
            "http://demo-preview.localhost/abc",
            "https://Demo-Preview.localhost/abc",
        ] {
            assert_eq!(
                refuse_preview_document(&origins, Some(&url(page))),
                Err(refused),
                "{page}"
            );
        }
        // The app's own pages, another scheme, a lookalike host and an unreadable URL pass this rule.
        for page in [
            "tauri://localhost/",
            "http://tauri.localhost/",
            "http://demo-preview.example/abc",
            "http://evil-demo-preview.localhost/abc",
            "https://example.com/demo-preview.localhost",
        ] {
            assert_eq!(
                refuse_preview_document(&origins, Some(&url(page))),
                Ok(()),
                "{page}"
            );
        }
        assert_eq!(refuse_preview_document(&origins, None), Ok(()));
        // No schemes registered: nothing is a preview document.
        let none = crate::preview::PreviewOrigins::default();
        assert_eq!(
            refuse_preview_document(&none, Some(&url("demo-preview://localhost/abc"))),
            Ok(())
        );
    }

    #[test]
    fn the_app_webview_on_its_own_origin_is_allowed() {
        let nav = NavPolicy::default();
        for page in ["tauri://localhost/", "http://tauri.localhost/index.html"] {
            let u = url(page);
            assert_eq!(
                decide(&facts("main", true, &[], Some(&u), &nav)),
                Ok(()),
                "{page}"
            );
        }
    }

    #[test]
    fn a_label_that_is_not_an_app_window_is_refused() {
        let nav = NavPolicy::default();
        let u = url("tauri://localhost/");
        assert_eq!(
            decide(&facts("child", false, &[], Some(&u), &nav)),
            Err(CallerError::Forbidden(Denial::NotAnAppWebview))
        );
    }

    #[test]
    fn registered_content_webviews_are_refused_by_exact_label_or_pattern() {
        let nav = NavPolicy::default();
        let u = url("tauri://localhost/");
        let content = vec!["html-content".to_owned(), "doc-*".to_owned()];
        for label in ["html-content", "doc-1", "doc-"] {
            assert_eq!(
                decide(&facts(label, true, &content, Some(&u), &nav)),
                Err(CallerError::Forbidden(Denial::ContentWebview)),
                "{label}"
            );
        }
        for label in ["main", "html-content2", "xdoc-1", "settings"] {
            assert_eq!(
                decide(&facts(label, true, &content, Some(&u), &nav)),
                Ok(()),
                "{label}"
            );
        }
    }

    #[test]
    fn a_foreign_or_fabricated_origin_is_refused() {
        let nav = NavPolicy::default();
        for page in [
            "https://example.com/",
            "http://localhost:5173/",
            "http://tauri.localhost.evil.test/",
            "about:blank",
            "blob:https://x/1",
            "data:text/html,<p>x</p>",
            "file:///C:/x.html",
        ] {
            let u = url(page);
            assert_eq!(
                decide(&facts("main", true, &[], Some(&u), &nav)),
                Err(CallerError::Forbidden(Denial::ForeignOrigin)),
                "{page}"
            );
        }
    }

    #[test]
    fn an_unreadable_url_fails_closed() {
        let nav = NavPolicy::default();
        assert_eq!(
            decide(&facts("main", true, &[], None, &nav)),
            Err(CallerError::Forbidden(Denial::UnknownOrigin))
        );
    }

    #[test]
    fn host_internal_origins_follow_the_navigation_policy() {
        let nav = NavPolicy {
            internal_hosts: vec!["app.example".into()],
            ..NavPolicy::default()
        }
        .with_internal_scheme("myapp")
        .unwrap();
        for page in ["https://app.example/x", "myapp://localhost/"] {
            let u = url(page);
            assert_eq!(
                decide(&facts("main", true, &[], Some(&u), &nav)),
                Ok(()),
                "{page}"
            );
        }
        let other = url("https://other.example/");
        assert!(decide(&facts("main", true, &[], Some(&other), &nav)).is_err());
    }

    #[test]
    fn loopback_dev_servers_are_app_origin_only_when_the_policy_allows_them() {
        let dev = NavPolicy {
            allow_loopback: true,
            ..NavPolicy::default()
        };
        let u = url("http://localhost:5173/");
        assert_eq!(decide(&facts("main", true, &[], Some(&u), &dev)), Ok(()));
    }

    #[test]
    fn origin_of_covers_custom_schemes_and_ports() {
        assert_eq!(
            origin_of(&url("tauri://localhost/index.html")).as_deref(),
            Some("tauri://localhost")
        );
        assert_eq!(
            origin_of(&url("http://tauri.localhost/x")).as_deref(),
            Some("http://tauri.localhost")
        );
        assert_eq!(
            origin_of(&url("http://localhost:5173/")).as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(origin_of(&url("about:blank")), None);
        assert_eq!(origin_of(&url("data:text/html,x")), None);
    }

    #[test]
    fn a_refusal_is_a_permission_error_with_a_forbidden_message() {
        let e: ShellError = CallerError::Forbidden(Denial::ContentWebview).into();
        assert_eq!(e.code(), "permission_denied");
        assert!(e.message().starts_with("forbidden: "), "{}", e.message());
    }
}
