//! In-memory preview documents served from their own custom scheme (Orthic-Labs/rightkit#386).
//!
//! Donor: ViewRight `preview_protocol.rs` (`viewright-preview://`). A `srcdoc` iframe inherits the
//! embedding window's CSP, so previewing content that needs `'unsafe-eval'` (compiled MDX) or remote
//! `http(s)` scripts (interactive HTML) would force the MAIN window to allow both. Serving each
//! preview from its own scheme gives it its own response CSP and keeps the main window strict.
//!
//! # Security model
//!
//! * Content is registered by the app under a random token (256 bits from the OS entropy source, hex)
//!   and is served ONLY by that token. Never by file path; nothing here touches disk.
//! * A token expires after [`PreviewLimits::ttl`] and can be removed at any time ([`PreviewStore::revoke`],
//!   [`PreviewStore::revoke_all`]). An expired, revoked or unknown token answers 404 and is never
//!   reused.
//! * Each response carries a `Content-Security-Policy` chosen by the document's registered KIND from
//!   the app's own kind-to-policy map, not by the content, so a document cannot widen its own policy.
//!   This crate bakes in no policy string except the deny-all [`DENY_CSP`] for refusals.
//! * The iframe should keep `sandbox="allow-scripts"` (never `allow-same-origin`), so the document
//!   runs in an opaque origin and cannot reach the app, IPC, or the parent DOM.
//! * Registration is bounded in per-document size, total size and entry count.
//!
//! The crate owns the store, the token rules and the response; the app owns the scheme name, the kind
//! map, the commands that register and revoke (they belong in the app's own ACL manifest and should
//! start with [`crate::caller_gate::require_app_caller`]) and the iframe:
//!
//! ```ignore
//! let store = Arc::new(PreviewStore::new([
//!     ("mdx", "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' blob:"),
//!     ("html", "default-src 'none'; img-src data:"),
//! ])?);
//! Shell::builder("myapp").preview_protocol("myapp-preview", store.clone()) /* ... */;
//! // commands: store.register("mdx", html) -> token, store.revoke(&token)
//! // iframe src: preview_url("myapp-preview", &token)
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::http::{header, Method, Request, Response, StatusCode};
#[cfg(feature = "host-services")]
use tauri::Manager;

use crate::error::ShellError;

/// Largest single registered document by default.
pub const MAX_PREVIEW_BYTES: usize = 64 * 1024 * 1024;
/// Largest sum of all live registered documents by default.
pub const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
/// Most live registrations at once by default.
pub const MAX_PREVIEWS: usize = 64;
/// How long a token lives by default.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
/// Longest token lifetime a store accepts.
pub const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const TOKEN_BYTES: usize = 32;
/// Length of a token in hex digits.
pub const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;
const MAX_KIND_LEN: usize = 32;
const MAX_CSP_LEN: usize = 8192;

/// Served for unknown, expired or revoked tokens and for non-GET requests: nothing may load.
pub const DENY_CSP: &str = "default-src 'none'";

/// The bounds of a [`PreviewStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewLimits {
    /// Largest single document.
    pub max_bytes: usize,
    /// Largest sum of all live documents.
    pub max_total_bytes: usize,
    /// Most live documents.
    pub max_previews: usize,
    /// Lifetime of a token from registration.
    pub ttl: Duration,
}

impl Default for PreviewLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_PREVIEW_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_previews: MAX_PREVIEWS,
            ttl: DEFAULT_TTL,
        }
    }
}

impl PreviewLimits {
    fn validate(&self) -> Result<(), ShellError> {
        if self.max_bytes == 0
            || self.max_previews == 0
            || self.max_total_bytes < self.max_bytes
            || self.ttl.is_zero()
            || self.ttl > MAX_TTL
        {
            return Err(ShellError::Invalid(
                "preview limits need a non-zero size and count, a ttl of at most seven days, and a total of at least one document".into(),
            ));
        }
        Ok(())
    }
}

/// A document kind: `a-z`, `0-9`, `-`, `_`, starting with a letter or digit, at most 32 characters.
pub fn valid_kind(kind: &str) -> bool {
    let mut chars = kind.chars();
    kind.len() <= MAX_KIND_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

/// A header-safe policy: non-empty, visible ASCII and spaces only, bounded.
pub fn valid_csp(csp: &str) -> bool {
    !csp.trim().is_empty()
        && csp.len() <= MAX_CSP_LEN
        && csp.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

struct Entry {
    kind: String,
    html: Arc<[u8]>,
    expires: Instant,
}

/// In-memory, token-keyed preview documents. Share it as `Arc<PreviewStore>`: the app's register and
/// revoke commands hold one clone and [`crate::ShellBuilder::preview_protocol`] the other.
pub struct PreviewStore {
    csp: BTreeMap<String, String>,
    limits: PreviewLimits,
    inner: Mutex<HashMap<String, Entry>>,
}

#[cfg(unix)]
fn fill_entropy(buf: &mut [u8]) -> std::io::Result<()> {
    extern "C" {
        fn getentropy(buf: *mut std::ffi::c_void, len: usize) -> std::ffi::c_int;
    }
    // SAFETY: `buf` is a valid writable slice of at most 256 bytes.
    if unsafe { getentropy(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn fill_entropy(buf: &mut [u8]) -> std::io::Result<()> {
    #[link(name = "advapi32")]
    extern "system" {
        #[link_name = "SystemFunction036"]
        fn RtlGenRandom(buf: *mut u8, len: u32) -> u8;
    }
    // SAFETY: `buf` is a valid writable slice for `len` bytes.
    if unsafe { RtlGenRandom(buf.as_mut_ptr(), buf.len() as u32) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// `bytes` random bytes from the OS entropy source, as lowercase hex. At most 256 bytes.
pub(crate) fn random_hex(bytes: usize) -> Result<String, ShellError> {
    let mut buf = vec![0_u8; bytes.min(256)];
    fill_entropy(&mut buf)
        .map_err(|e| ShellError::Other(format!("could not generate a random token: {e}")))?;
    let mut out = String::with_capacity(buf.len() * 2);
    for byte in buf {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

fn new_token() -> Result<String, ShellError> {
    random_hex(TOKEN_BYTES)
        .map_err(|_| ShellError::Other("could not generate a preview token".into()))
}

impl PreviewStore {
    /// A store with the app's kind-to-policy map and the default limits. Every kind must satisfy
    /// [`valid_kind`] and its policy [`valid_csp`]; the map needs at least one kind. The same kind
    /// given twice keeps the last policy.
    pub fn new<I, K, C>(csp_by_kind: I) -> Result<Self, ShellError>
    where
        I: IntoIterator<Item = (K, C)>,
        K: Into<String>,
        C: Into<String>,
    {
        let mut csp = BTreeMap::new();
        for (kind, policy) in csp_by_kind {
            let (kind, policy) = (kind.into(), policy.into());
            if !valid_kind(&kind) {
                return Err(ShellError::Invalid(format!(
                    "{kind:?} is not a preview kind (a-z, 0-9, '-', '_'; at most {MAX_KIND_LEN})"
                )));
            }
            if !valid_csp(&policy) {
                return Err(ShellError::Invalid(format!(
                    "the policy for preview kind {kind:?} is empty, too long or not plain ASCII"
                )));
            }
            csp.insert(kind, policy);
        }
        if csp.is_empty() {
            return Err(ShellError::Invalid(
                "a preview store needs at least one kind".into(),
            ));
        }
        Ok(Self {
            csp,
            limits: PreviewLimits::default(),
            inner: Mutex::new(HashMap::new()),
        })
    }

    /// Replace the limits (validated).
    pub fn with_limits(mut self, limits: PreviewLimits) -> Result<Self, ShellError> {
        limits.validate()?;
        self.limits = limits;
        Ok(self)
    }

    pub fn limits(&self) -> PreviewLimits {
        self.limits
    }

    /// The policy `kind` is served under, or `None` for a kind the app did not register.
    pub fn csp(&self, kind: &str) -> Option<&str> {
        self.csp.get(kind).map(String::as_str)
    }

    /// The registered kinds, sorted.
    pub fn kinds(&self) -> Vec<&str> {
        self.csp.keys().map(String::as_str).collect()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register `html` under `kind` and return its token. Bounded by the store's limits; expired
    /// documents are dropped first and do not count. An unknown `kind` is refused.
    pub fn register(&self, kind: &str, html: String) -> Result<String, ShellError> {
        self.register_at(kind, html, Instant::now())
    }

    /// [`PreviewStore::register`] with an explicit clock (tests).
    pub fn register_at(
        &self,
        kind: &str,
        html: String,
        now: Instant,
    ) -> Result<String, ShellError> {
        if !self.csp.contains_key(kind) {
            return Err(ShellError::Invalid(format!(
                "{kind:?} is not a registered preview kind"
            )));
        }
        if html.len() > self.limits.max_bytes {
            return Err(ShellError::DocumentTooLarge(
                "preview document is too large".into(),
            ));
        }
        let mut map = self.map();
        map.retain(|_, e| e.expires > now);
        if map.len() >= self.limits.max_previews {
            return Err(ShellError::Invalid("too many open previews".into()));
        }
        let total: usize = map.values().map(|e| e.html.len()).sum();
        if total.saturating_add(html.len()) > self.limits.max_total_bytes {
            return Err(ShellError::DocumentTooLarge(
                "preview memory budget exceeded".into(),
            ));
        }
        let token = loop {
            let token = new_token()?;
            if !map.contains_key(&token) {
                break token;
            }
        };
        map.insert(
            token.clone(),
            Entry {
                kind: kind.to_owned(),
                html: Arc::from(html.into_bytes()),
                expires: now + self.limits.ttl,
            },
        );
        Ok(token)
    }

    /// Drop a registration. Unknown tokens are ignored.
    pub fn revoke(&self, token: &str) {
        self.map().remove(token);
    }

    /// Drop every registration (the window closed, the document set changed).
    pub fn revoke_all(&self) {
        self.map().clear();
    }

    /// Drop expired registrations now and return how many went.
    pub fn purge_expired(&self, now: Instant) -> usize {
        let mut map = self.map();
        let before = map.len();
        map.retain(|_, e| e.expires > now);
        before - map.len()
    }

    /// Live (unexpired as of `now`) registrations.
    pub fn len_at(&self, now: Instant) -> usize {
        self.map().values().filter(|e| e.expires > now).count()
    }

    /// Registrations held, expired ones included until the next register, lookup or purge.
    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes held by registrations, expired ones included until dropped.
    pub fn total_bytes(&self) -> usize {
        self.map().values().map(|e| e.html.len()).sum()
    }

    /// The document for a live token: its kind and bytes. An expired token is removed and answers
    /// `None`, like an unknown or revoked one.
    pub fn lookup_at(&self, token: &str, now: Instant) -> Option<(String, Arc<[u8]>)> {
        let mut map = self.map();
        let found = map
            .get(token)
            .map(|e| (e.expires > now, e.kind.clone(), Arc::clone(&e.html)));
        match found {
            Some((true, kind, html)) => Some((kind, html)),
            Some((false, _, _)) => {
                map.remove(token);
                None
            }
            None => None,
        }
    }

    /// The kind a live token was registered under, or `None` for an unknown, revoked or expired
    /// token. An app uses it to decide what else it may do with a document (ViewRight: only an
    /// interactive document may be opened in DevTools) without holding a second token table.
    pub fn kind_of(&self, token: &str) -> Option<String> {
        self.kind_of_at(token, Instant::now())
    }

    /// [`PreviewStore::kind_of`] with an explicit clock (tests).
    pub fn kind_of_at(&self, token: &str, now: Instant) -> Option<String> {
        self.lookup_at(token, now).map(|(kind, _)| kind)
    }

    /// Drop every live registration of `kind`; returns how many went. For a window that owned
    /// one kind of document and closed.
    pub fn revoke_kind(&self, kind: &str) -> usize {
        let mut map = self.map();
        let before = map.len();
        map.retain(|_, e| e.kind != kind);
        before - map.len()
    }
}

/// The preview schemes this app registered with [`crate::ShellBuilder::preview_protocol`]. Managed
/// Tauri state, so the caller gate can refuse a webview whose page IS a preview document.
#[derive(Debug, Clone, Default)]
pub struct PreviewOrigins {
    schemes: Vec<String>,
}

impl PreviewOrigins {
    pub(crate) fn new(schemes: impl IntoIterator<Item = String>) -> Self {
        Self {
            schemes: schemes.into_iter().collect(),
        }
    }

    /// The registered scheme names, lower-case.
    pub fn schemes(&self) -> &[String] {
        &self.schemes
    }

    /// True when `url` is served by one of the preview schemes, in either of the forms a platform
    /// uses: `<scheme>://...` (macOS, Linux) or `http(s)://<scheme>.localhost/...` (Windows).
    pub fn is_preview_url(&self, url: &tauri::Url) -> bool {
        is_preview_url(&self.schemes, url)
    }
}

/// See [`PreviewOrigins::is_preview_url`].
pub fn is_preview_url(schemes: &[String], url: &tauri::Url) -> bool {
    let scheme = url.scheme().to_ascii_lowercase();
    if schemes.contains(&scheme) {
        return true;
    }
    matches!(scheme.as_str(), "http" | "https")
        && url
            .host_str()
            .map(str::to_ascii_lowercase)
            .and_then(|h| h.strip_suffix(".localhost").map(str::to_owned))
            .is_some_and(|name| schemes.contains(&name))
}

/// What the preview commands answer for a registered document: the token and the URL its iframe
/// loads. The URL is built here from the app's scheme, never taken from the renderer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewHandle {
    pub token: String,
    pub url: String,
}

/// The preview service the shell's `rightkit_preview_*` commands drive (feature `host-services`):
/// one store and the scheme it is served under. Register it with
/// `crate::host::HostServicesBuilder::service` under `crate::host::ServiceId::PREVIEW`.
#[derive(Clone)]
pub struct PreviewService {
    store: Arc<PreviewStore>,
    scheme: String,
}

impl PreviewService {
    /// `scheme` must be the one the store is registered under with
    /// [`crate::ShellBuilder::preview_protocol`].
    pub fn new(store: Arc<PreviewStore>, scheme: &str) -> Result<Self, ShellError> {
        let scheme = crate::nav::validate_internal_scheme(scheme).map_err(ShellError::Invalid)?;
        Ok(Self { store, scheme })
    }

    pub fn store(&self) -> &Arc<PreviewStore> {
        &self.store
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Register `html` under `kind` and answer the token and iframe URL.
    pub fn register(&self, kind: &str, html: String) -> Result<PreviewHandle, ShellError> {
        let token = self.store.register(kind, html)?;
        let url = preview_url(&self.scheme, &token);
        Ok(PreviewHandle { token, url })
    }

    /// Revoke one token. Unknown tokens are ignored.
    pub fn revoke(&self, token: &str) {
        self.store.revoke(token);
    }

    /// Revoke every token.
    pub fn revoke_all(&self) {
        self.store.revoke_all();
    }
}

#[cfg(feature = "host-services")]
fn preview_service(
    webview: &tauri::Webview<tauri::Wry>,
    action: &str,
) -> Result<Arc<PreviewService>, crate::host::HostError> {
    let app = webview.app_handle();
    let services = crate::host::managed(app)?;
    let owned = crate::caller_gate::gather(webview)?;
    services.service_for::<PreviewService>(crate::host::ServiceId::PREVIEW, action, &owned.facts())
}

/// Register a preview document. Answers `{ token, url }`; the content is served only under a
/// kind the app registered, with that kind's policy.
#[cfg(feature = "host-services")]
#[tauri::command]
pub async fn rightkit_preview_register(
    webview: tauri::Webview<tauri::Wry>,
    kind: String,
    html: String,
) -> Result<PreviewHandle, crate::host::HostError> {
    let service = preview_service(&webview, "register")?;
    service
        .register(&kind, html)
        .map_err(crate::host::HostError::from)
}

/// Revoke one preview token.
#[cfg(feature = "host-services")]
#[tauri::command]
pub async fn rightkit_preview_revoke(
    webview: tauri::Webview<tauri::Wry>,
    token: String,
) -> Result<(), crate::host::HostError> {
    let service = preview_service(&webview, "revoke")?;
    service.revoke(&token);
    Ok(())
}

/// Revoke every preview token (the document set changed).
#[cfg(feature = "host-services")]
#[tauri::command]
pub async fn rightkit_preview_revoke_all(
    webview: tauri::Webview<tauri::Wry>,
) -> Result<(), crate::host::HostError> {
    let service = preview_service(&webview, "revoke")?;
    service.revoke_all();
    Ok(())
}

/// The URL an iframe loads for `token`, per platform convention (WebView2 maps a custom scheme to
/// `http://<scheme>.localhost/`).
pub fn preview_url(scheme: &str, token: &str) -> String {
    if cfg!(windows) {
        format!("http://{scheme}.localhost/{token}")
    } else {
        format!("{scheme}://localhost/{token}")
    }
}

fn base_response(status: StatusCode, csp: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, csp)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

fn deny(status: StatusCode) -> Response<Vec<u8>> {
    base_response(status, DENY_CSP, Vec::new())
}

/// Serve a `<scheme>://localhost/<token>` request (`http://<scheme>.localhost/<token>` on Windows).
/// Only the exact token path resolves; every other path is 404, every non-GET/HEAD is 405.
pub fn serve_preview_request(
    store: &PreviewStore,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    serve_preview_request_at(store, request, Instant::now())
}

/// [`serve_preview_request`] with an explicit clock (tests).
pub fn serve_preview_request_at(
    store: &PreviewStore,
    request: &Request<Vec<u8>>,
    now: Instant,
) -> Response<Vec<u8>> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return deny(StatusCode::METHOD_NOT_ALLOWED);
    }
    let path = request.uri().path();
    let token = path.strip_prefix('/').unwrap_or(path);
    let token = token.strip_suffix('/').unwrap_or(token);
    if token.len() != TOKEN_HEX_LEN || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return deny(StatusCode::NOT_FOUND);
    }
    // Tokens are lowercase hex; an upper-case spelling is a different string and never resolves.
    let Some((kind, html)) = store.lookup_at(token, now) else {
        return deny(StatusCode::NOT_FOUND);
    };
    // A kind is checked at registration, so a miss here is a refusal, never a policy-less response.
    let Some(csp) = store.csp(&kind) else {
        return deny(StatusCode::NOT_FOUND);
    };
    let body = if request.method() == Method::HEAD {
        Vec::new()
    } else {
        html.to_vec()
    };
    base_response(StatusCode::OK, csp, body)
}
