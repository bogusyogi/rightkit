//! Remote-origin content webview with a profile slot (#963).
//!
//! [`windows::WindowSpec`](crate::windows::WindowSpec) opens app-relative paths only. A
//! [`ContentWebviewSpec`] opens a window onto a remote `https` origin the app names, for the apps
//! that show third-party pages inside the shell (PlayRight's per-service sign-in windows,
//! ScrapeRight's in-app sign-in, ViewRight's HTML content windows). Everything the page may do is
//! stated in the spec:
//!
//! * **Origin allow-list.** Top-level navigation stays on `allowed_origins`; anything else is
//!   refused, never handed to the system browser (a subframe's navigation can reach this hook on
//!   some engines, and opening a browser for it would be wrong). The start URL's origin must be on
//!   the list.
//! * **Popup allow-list.** A page's `window.open` becomes an in-app popup only for a sized request
//!   to an `https` origin on `popup_origins` (the authentication origins). Any other web link opens
//!   in the system browser; every other scheme is refused.
//! * **Profile slot.** A [`ContentProfile`] gives the window its own persistent cookie and storage
//!   area: `data_directory` on Windows and Linux (registered with the DevTools tripwire before the
//!   webview exists), a `data_store_identifier` derived from the slot on macOS 14+ and iOS 17+.
//!   With no profile the window is incognito: nothing outlives it.
//! * **No IPC.** The webview takes no capability. [`ContentWebviewSpec::validate`] refuses a spec
//!   whose attested capability documents grant any permission to its label (or to its parent
//!   window's), or when the app does not attest an application ACL manifest.
//! * **DevTools.** The shell's gate decides for every webview. Under `DevToolsPolicy::Denied` the
//!   window is non-inspectable. An app on the `ContentOnly` allow-list hands
//!   [`ContentWebviewSpec::registration`] to `ShellBuilder` so the gate lets exactly this content
//!   (and nothing of the app's own UI) be inspected.
//! * **Destroy on close.** Closing the window destroys the webview and forgets its registration; the
//!   profile's files stay for the next open of the same slot.
//!
//! The shell's own navigation hook (`Module::NavigationPolicy`) stands aside for a registered content
//! window, whose spec decides instead; the shell's hardening script and native DevTools switches
//! still apply to it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder, Wry};

use crate::devtools::{self, ContentWebview};
use crate::lifecycle::{reveal_mode, RevealMode, RevealSource};
use crate::nav::{origin_of, WindowOpenDecision, EXTERNAL_SCHEMES};
use crate::opener::open_checked;
use crate::windows::{reveal_with, safe_label, show_window, RevealStyle};
use crate::{ShellState, EVENT_WINDOW};

/// One persistent cookie and storage area, named by a slot (one per signed-in service or account).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentProfile {
    slot: String,
    root: PathBuf,
}

impl ContentProfile {
    /// `slot`: 1 to 64 of `A-Z a-z 0-9 - _`. `root`: an absolute folder the app owns; the slot's data
    /// folder is `root/<slot>`.
    pub fn new(slot: &str, root: impl Into<PathBuf>) -> Result<Self, String> {
        let valid = !slot.is_empty()
            && slot.len() <= 64
            && slot
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        if !valid {
            return Err(format!(
                "invalid profile slot {slot:?}: use 1 to 64 of A-Z a-z 0-9 - _"
            ));
        }
        let root = root.into();
        if !root.is_absolute() {
            return Err("the profile root must be an absolute folder".into());
        }
        Ok(Self {
            slot: slot.to_owned(),
            root,
        })
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }

    /// The folder this slot keeps its data in (Windows and Linux `data_directory`).
    pub fn data_dir(&self) -> PathBuf {
        self.root.join(&self.slot)
    }

    /// The 16-byte WKWebView data store identifier of this slot (macOS 14+, iOS 17+): FNV-1a 128 of
    /// the slot under a fixed prefix, so the same slot names the same store in every launch and every
    /// version. It names a store; it is not a secret.
    pub fn store_identifier(&self) -> [u8; 16] {
        const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
        const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
        let mut hash = OFFSET;
        for byte in b"rightkit-content-profile:"
            .iter()
            .chain(self.slot.as_bytes())
        {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash.to_be_bytes()
    }
}

/// What a remote-origin content window is and may do. See the module documentation.
#[derive(Debug, Clone)]
pub struct ContentWebviewSpec {
    /// Window and webview label (`A-Z a-z 0-9 - _`, at most 80). Never the main window's label.
    pub label: String,
    pub title: Option<String>,
    /// The start page: `https`, and its origin must be in `allowed_origins`.
    pub url: String,
    /// Origins (`https://host[:port]`, canonical, no path) the window may navigate to.
    pub allowed_origins: Vec<String>,
    /// Origins allowed to open as an in-app popup (authentication origins).
    pub popup_origins: Vec<String>,
    /// The persistent profile; `None` opens an incognito window.
    pub profile: Option<ContentProfile>,
    /// `http://` or `socks5://` proxy for every request of this webview.
    pub proxy_url: Option<String>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    /// Label of an open window to make this window's native parent.
    pub parent: Option<String>,
    /// Every capability document of the app (JSON text, `include_str!` of each file under
    /// `capabilities/`). None may grant a permission to this window's label or its parent's.
    pub capabilities: Vec<String>,
    /// The app declares an ACL manifest for its own commands (`tauri_build::AppManifest`).
    pub app_acl_manifest: bool,
}

impl ContentWebviewSpec {
    pub fn new(label: &str, url: &str) -> Self {
        Self {
            label: label.to_owned(),
            title: None,
            url: url.to_owned(),
            allowed_origins: Vec::new(),
            popup_origins: Vec::new(),
            profile: None,
            proxy_url: None,
            width: None,
            height: None,
            parent: None,
            capabilities: Vec::new(),
            app_acl_manifest: false,
        }
    }

    pub fn allow_origin(mut self, origin: &str) -> Self {
        self.allowed_origins.push(origin.to_owned());
        self
    }

    pub fn popup_origin(mut self, origin: &str) -> Self {
        self.popup_origins.push(origin.to_owned());
        self
    }

    pub fn profile(mut self, profile: ContentProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    pub fn proxy(mut self, url: &str) -> Self {
        self.proxy_url = Some(url.to_owned());
        self
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }

    pub fn size(mut self, width: f64, height: f64) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    pub fn parent(mut self, label: &str) -> Self {
        self.parent = Some(label.to_owned());
        self
    }

    /// Add one capability document (JSON text) of the app.
    pub fn capability(mut self, json: &str) -> Self {
        self.capabilities.push(json.to_owned());
        self
    }

    /// Attest that the app declares an ACL manifest for its commands.
    pub fn app_acl_manifest(mut self, declared: bool) -> Self {
        self.app_acl_manifest = declared;
        self
    }

    /// The DevTools registration of this window, for `ShellBuilder` under
    /// `DevToolsPolicy::ContentOnly` (ViewRight's allowance): the gate then lets this window's
    /// content be inspected and nothing of the app's own UI. Under `Denied`, register nothing.
    pub fn registration(&self) -> ContentWebview {
        let mut registration =
            ContentWebview::new(&self.label, self.parent.as_deref().unwrap_or(&self.label))
                .app_acl_manifest(self.app_acl_manifest);
        for doc in &self.capabilities {
            registration = registration.capability(doc);
        }
        registration
    }

    /// Check the spec and return the start URL. Refuses: a bad label, a start URL that is not
    /// `https` or whose origin is not allowed, an empty or non-canonical origin list, a bad proxy URL,
    /// a missing ACL manifest attestation, and any capability that grants this window IPC.
    pub fn validate(&self) -> Result<Url, String> {
        let label = safe_label(&self.label).ok_or("invalid window label")?;
        if self.allowed_origins.is_empty() {
            return Err("allowed_origins is empty".into());
        }
        check_origins("allowed_origins", &self.allowed_origins)?;
        check_origins("popup_origins", &self.popup_origins)?;
        let start = Url::parse(&self.url).map_err(|e| format!("start url: {e}"))?;
        if start.scheme() != "https" {
            return Err("the start url must be https".into());
        }
        if !start.username().is_empty() || start.password().is_some() {
            return Err("the start url carries credentials".into());
        }
        let origin = origin_of(&start).ok_or("the start url has no host")?;
        if !self.allowed_origins.contains(&origin) {
            return Err(format!(
                "the start origin {origin} is not in allowed_origins"
            ));
        }
        if let Some(raw) = &self.proxy_url {
            let proxy = Url::parse(raw).map_err(|e| format!("proxy url: {e}"))?;
            if !matches!(proxy.scheme(), "http" | "socks5") {
                return Err("the proxy url must be http:// or socks5://".into());
            }
        }
        if !self.app_acl_manifest {
            return Err(
                "the app must attest an application ACL manifest: without one a local-origin page may call every app command"
                    .into(),
            );
        }
        let window_label = self.parent.as_deref().unwrap_or(&label);
        for doc in &self.capabilities {
            if let Some(capability) = devtools::capability_granting_ipc(doc, &label, window_label)
                .map_err(|e| e.to_string())?
            {
                return Err(format!(
                    "capability {capability} grants permissions to {label}: a content webview takes no IPC"
                ));
            }
        }
        Ok(start)
    }

    /// Whether the window may load `url` as a top-level page: `https` on an allowed origin, or
    /// `about:blank`. Pure.
    pub fn allows_navigation(&self, url: &Url) -> bool {
        match url.scheme() {
            "https" => {
                url.username().is_empty()
                    && url.password().is_none()
                    && origin_of(url).is_some_and(|o| self.allowed_origins.contains(&o))
            }
            "about" => url.as_str() == "about:blank",
            _ => false,
        }
    }

    /// What a page's `window.open` does. `popup_shaped` is true for a sized request, `opener` the
    /// opening page's URL when known. A sized request to a popup origin from an allowed page is a
    /// `Popup`; any other web link is `External`; every other scheme is `Deny`. Pure.
    pub fn decide_window_open(
        &self,
        url: &Url,
        popup_shaped: bool,
        opener: Option<&Url>,
    ) -> WindowOpenDecision {
        if !EXTERNAL_SCHEMES.contains(&url.scheme()) {
            return WindowOpenDecision::Deny;
        }
        if popup_shaped
            && url.scheme() == "https"
            && opener.is_none_or(|o| self.allows_navigation(o))
            && origin_of(url).is_some_and(|o| self.popup_origins.contains(&o))
        {
            return WindowOpenDecision::Popup;
        }
        WindowOpenDecision::External {
            url: url.to_string(),
        }
    }
}

fn check_origins(name: &str, origins: &[String]) -> Result<(), String> {
    for origin in origins {
        let url = Url::parse(origin).map_err(|e| format!("{name}: {origin}: {e}"))?;
        if url.scheme() != "https" || origin_of(&url).as_deref() != Some(origin.as_str()) {
            return Err(format!(
                "{name}: {origin} is not a canonical https origin (https://host[:port], no path)"
            ));
        }
    }
    Ok(())
}

type Registry = Mutex<HashMap<String, Arc<ContentWebviewSpec>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `label` names an open (or opening) content window. The shell's navigation hook stands
/// aside for it.
pub(crate) fn is_content(label: &str) -> bool {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(label)
}

/// Forget a content window's registration (its window was destroyed, or it never opened).
pub(crate) fn forget(label: &str) {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(label);
}

/// Open (or reveal) the content window `spec` describes and return its label. The spec is validated
/// first and nothing is created when it is refused. Opening the same label again reveals the open
/// window.
pub fn open(app: &AppHandle<Wry>, spec: &ContentWebviewSpec) -> Result<String, String> {
    let start = spec.validate()?;
    let label = safe_label(&spec.label).ok_or("invalid window label")?;
    let state = app.state::<ShellState>();
    if label == state.main_label {
        return Err("a content webview cannot take the main window's label".into());
    }
    if let Some(existing) = app.get_webview_window(&label) {
        if !is_content(&label) {
            return Err(format!("window label {label} is in use by another window"));
        }
        reveal_with(app, Some(&label), RevealSource::OpenWindow);
        return Ok(existing.label().to_owned());
    }
    let title = spec
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&state.app_name)
        .to_owned();
    let focused = reveal_mode(app, RevealSource::OpenWindow) == RevealMode::Foreground
        && !crate::lifecycle::never_show(app);
    let pick = |v: Option<f64>, default: f64| {
        v.filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(default)
            .clamp(320.0, 16_384.0)
    };
    let (width, height) = (pick(spec.width, 1000.0), pick(spec.height, 700.0));
    let shared = Arc::new(spec.clone());

    let nav_spec = Arc::clone(&shared);
    let (popup_app, popup_label) = (app.clone(), label.clone());
    let popup_spec = Arc::clone(&shared);
    let opener = Arc::clone(&state.opener);
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(start))
        .title(title)
        .inner_size(width, height)
        .min_inner_size(320.0, 240.0)
        .visible(false)
        .focused(focused)
        .on_navigation(move |url| nav_spec.allows_navigation(url))
        .on_new_window(move |url, features| {
            let from = popup_app
                .get_webview_window(&popup_label)
                .and_then(|w| w.url().ok());
            match popup_spec.decide_window_open(&url, features.size().is_some(), from.as_ref()) {
                WindowOpenDecision::External { url } => {
                    let _ = open_checked(opener.as_ref(), &url);
                    NewWindowResponse::Deny
                }
                WindowOpenDecision::Popup => NewWindowResponse::Allow,
                WindowOpenDecision::ProtocolConsent { .. } | WindowOpenDecision::Deny => {
                    NewWindowResponse::Deny
                }
            }
        });
    match &spec.profile {
        Some(profile) => {
            let dir = profile.data_dir();
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("profile folder {}: {e}", dir.display()))?;
            // The tripwire must know the folder before the webview exists (it looks for a live
            // remote-debugging port file in every folder an app gives a webview).
            devtools::register_user_data_dir(dir.clone());
            builder = builder
                .data_directory(dir)
                .data_store_identifier(profile.store_identifier());
        }
        None => builder = builder.incognito(true),
    }
    if let Some(raw) = &spec.proxy_url {
        let proxy = Url::parse(raw).map_err(|e| format!("proxy url: {e}"))?;
        builder = builder.proxy_url(proxy);
    }
    if let Some(raw) = spec.parent.as_deref() {
        let parent_label = safe_label(raw).ok_or("invalid parent window label")?;
        let parent = app
            .get_webview_window(&parent_label)
            .ok_or_else(|| format!("unknown parent window {parent_label}"))?;
        builder = builder
            .parent(&parent)
            .map_err(|e| format!("parent window {parent_label}: {e}"))?;
    }
    // Registered before the webview exists, so its first navigation already reaches the spec's policy.
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(label.clone(), shared);
    let window = match builder.build() {
        Ok(window) => window,
        Err(e) => {
            forget(&label);
            return Err(format!("create content window {label}: {e}"));
        }
    };
    let _ = app.emit(
        EVENT_WINDOW,
        serde_json::json!({"label": label, "kind": "created"}),
    );
    show_window(
        app,
        &window,
        RevealSource::OpenWindow,
        RevealStyle::ShowOnly,
    )?;
    Ok(label)
}
