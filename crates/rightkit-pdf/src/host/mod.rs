//! Native PDF view host (feature `host`): one PDF view per window, owned and released here.
//!
//! Donor: ViewRight `mac_pdf_view.rs` (macOS PDFKit reader, `native_pdf_*`). The donor kept the
//! lifecycle, the PDFKit calls and the Tauri window plumbing in one file. This module keeps the
//! first two and nothing of the third: [`PdfHost`] never depends on a window system. It is handed a
//! [`NativeParent`] (the native view the PDF view is attached next to) by the caller on each call
//! that needs one, and reports through [`HostHooks`]. The caller that knows windows, IPC and which
//! webview may ask is `rightkit-tauri-shell` (optional `pdf-host` feature), which owns the caller,
//! window and path authorization; this crate owns the view lifecycle. The dependency runs only that
//! way.
//!
//! # Model
//!
//! * A **window** (an opaque label the caller reads from the platform, never from a request) holds
//!   at most one active PDF view. Windows never see each other's views: every operation is keyed by
//!   the window label first, so a window that names another window's owner id finds nothing
//!   ([`HostError::NotOpen`]).
//! * The **owner** is a pane-local token the page picks (a stale-call guard for tab hand-off and
//!   reload races, not a secret). A call whose owner is not the active view's owner is refused
//!   ([`HostError::StaleOwner`]) and changes nothing.
//! * A **suspended** view (a hidden tab) is parked for reuse: at most [`HostConfig::parked_limit`]
//!   views for at most [`HostConfig::parked_max_age`], reused only by the same window and path while
//!   a SHA-256 of the file taken at open still matches, never when the document is encrypted. The
//!   crate runs no timer: expiry happens at every call and in [`PdfHost::sweep`]; a caller that
//!   wants views released while nothing is happening schedules `sweep` itself
//!   ([`PdfHost::next_sweep_in`] says when).
//! * The file path is an [`AuthorizedPdfPath`]: the caller proves the app's file broker granted it.
//!   This module never resolves a page-supplied string.
//! * The host is read-only: the document is never written; widgets are read-only; link clicks go
//!   through `HostHooks::open_link` (default: refuse); file-launch, script and remote-document
//!   actions are removed from the in-memory annotations.
//!
//! # Platforms
//!
//! macOS builds the PDFKit backend and requires every call to run on the AppKit main thread (a call
//! from another thread fails closed with [`HostError::NotMainThread`]). Every other platform builds
//! no native code: [`PdfHost::new`] answers a host whose every call is [`HostError::Unsupported`].
//!
//! # Events
//!
//! Lifecycle changes are recorded as [`HostResourceEvent`]s in a bounded outbox; the caller drains
//! it with [`PdfHost::drain_events`] after the call returns (a call may run inside a platform
//! callback that cannot emit). Key events the page must see while the PDF view has focus go through
//! [`HostHooks`] directly.

mod backend;
#[cfg(target_os = "macos")]
mod pdfkit;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use self::backend::{BackendOpen, FindQuery, HostBackend, UnsupportedBackend, ViewId, ViewState};

/// Longest window label accepted.
pub const MAX_WINDOW_LABEL_BYTES: usize = 128;
/// Longest owner token accepted.
pub const MAX_OWNER_BYTES: usize = 256;
/// Longest find query accepted, in bytes.
pub const MAX_FIND_QUERY_BYTES: usize = 4096;
/// Longest document password accepted, in bytes.
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Largest absolute frame coordinate or size accepted (CSS pixels).
pub const MAX_FRAME_EXTENT: f64 = 1_000_000.0;
/// Resource events kept for [`PdfHost::drain_events`]; the oldest is dropped past this.
pub const EVENT_OUTBOX_CAP: usize = 256;

/// The host's failures. The message never carries a path or document text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// No native PDF view exists on this platform or build.
    #[error("the native PDF view is not available on this platform")]
    Unsupported,
    /// The window has no active view (including: another window's view).
    #[error("native PDF view is not open")]
    NotOpen,
    /// The window's active view belongs to another owner.
    #[error("stale native PDF owner")]
    StaleOwner,
    /// The request is malformed.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The file is missing, not a regular file, or larger than [`HostConfig::max_file_bytes`].
    #[error("PDF file unavailable: {0}")]
    File(String),
    /// The platform could not load the document.
    #[error("the PDF could not be loaded: {0}")]
    Load(String),
    /// A native call ran off the thread the platform requires.
    #[error("the native PDF view must be used from the platform's main thread")]
    NotMainThread,
    /// The platform view failed.
    #[error("native PDF view error: {0}")]
    Native(String),
}

impl HostError {
    /// A stable code for the wire.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::NotOpen => "not_open",
            Self::StaleOwner => "stale_owner",
            Self::Invalid(_) => "invalid",
            Self::File(_) => "file",
            Self::Load(_) => "load_failed",
            Self::NotMainThread => "not_main_thread",
            Self::Native(_) => "native",
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

/// Where the PDF view sits in the window's content, in CSS pixels from the webview's top-left.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// View settings applied when a document opens.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostViewMeta {
    /// One-based page.
    pub page: Option<u32>,
    pub scale: Option<f64>,
    /// `"width"` or `"page"`.
    pub fit: Option<String>,
    /// Degrees; normalised to 0, 90, 180 or 270.
    pub rotation: Option<i32>,
}

/// One outline (bookmark) entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostOutlineItem {
    pub title: String,
    /// One-based page, when the destination resolves.
    pub page: Option<u32>,
    pub children: Vec<HostOutlineItem>,
}

/// What an open view looks like now.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostState {
    pub owner: String,
    pub page: u32,
    pub num_pages: u32,
    pub scale: f64,
    pub fit: Option<String>,
    pub rotation: i32,
    pub panel: Option<String>,
    pub locked: bool,
    pub has_outline: bool,
    pub outline: Vec<HostOutlineItem>,
}

impl HostState {
    fn new(owner: &str, view: ViewState) -> Self {
        Self {
            owner: owner.to_owned(),
            page: view.page,
            num_pages: view.num_pages,
            scale: view.scale,
            fit: view.fit,
            rotation: view.rotation,
            panel: view.panel,
            locked: view.locked,
            has_outline: !view.outline.is_empty(),
            outline: view.outline,
        }
    }
}

/// Result of a find: how many matches and which one is active (`-1` for none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFindResult {
    pub count: usize,
    pub active_index: i32,
}

/// Where a view is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourcePhase {
    Active,
    Parked,
    Released,
}

/// A lifecycle change, for the page and for leak checks.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResourceEvent {
    /// The window the event belongs to (routing only; not sent to the page).
    #[serde(skip)]
    pub window: String,
    pub owner: String,
    pub path: String,
    pub identity: Option<String>,
    pub generation: u64,
    pub phase: ResourcePhase,
    pub reason: String,
    pub active_count: usize,
    pub parked_count: usize,
    pub released_count: u64,
}

/// Active, parked and released-so-far view counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostCounts {
    pub active: usize,
    pub parked: usize,
    pub released: u64,
}

/// A key press the page must see while the PDF view has focus (the view would otherwise swallow
/// it).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKey {
    pub owner: String,
    pub key: String,
    /// A DOM `KeyboardEvent.code` where the platform key code has a known mapping.
    pub code: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
}

type DeliverKey = dyn Fn(&str, HostKey) + Send + Sync;

/// Callbacks the host cannot decide itself. Cheap to clone.
#[derive(Clone)]
pub struct HostHooks {
    open_link: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    claim_key: Arc<dyn Fn(&HostKey) -> bool + Send + Sync>,
    deliver_key: Arc<DeliverKey>,
}

impl Default for HostHooks {
    /// Links are refused; no key is claimed.
    fn default() -> Self {
        Self {
            open_link: Arc::new(|_| false),
            claim_key: Arc::new(|_| false),
            deliver_key: Arc::new(|_, _| {}),
        }
    }
}

impl HostHooks {
    /// Called with the URL of a clicked link; answer `true` when it was opened, `false` when it was
    /// refused. The app's own checked opener belongs here. Default: refuse every link.
    pub fn with_open_link(mut self, f: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        self.open_link = Arc::new(f);
        self
    }

    /// Decide which key presses (typed in the PDF view) belong to the app, not to the view. A
    /// claimed key is not delivered to the view and goes to `deliver` instead. Default: none.
    pub fn with_key_claim(
        mut self,
        claim: impl Fn(&HostKey) -> bool + Send + Sync + 'static,
        deliver: impl Fn(&str, HostKey) + Send + Sync + 'static,
    ) -> Self {
        self.claim_key = Arc::new(claim);
        self.deliver_key = Arc::new(deliver);
        self
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn open_link(&self, url: &str) -> bool {
        (self.open_link)(url)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn claim_key(&self, key: &HostKey) -> bool {
        (self.claim_key)(key)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn deliver_key(&self, window: &str, key: HostKey) {
        (self.deliver_key)(window, key);
    }
}

impl std::fmt::Debug for HostHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostHooks { .. }")
    }
}

/// Bounds and sizes of a [`PdfHost`].
#[derive(Clone, Debug, PartialEq)]
pub struct HostConfig {
    /// Most suspended views kept (`0` parks nothing). At most 64.
    pub parked_limit: usize,
    /// How long a suspended view is kept. At most ten minutes.
    pub parked_max_age: Duration,
    /// Largest PDF file the host opens.
    pub max_file_bytes: u64,
    /// Width of the native thumbnail rail the page reserves left of the view, in CSS pixels.
    pub thumbnail_rail: f64,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            parked_limit: 8,
            parked_max_age: Duration::from_secs(30),
            max_file_bytes: 1024 * 1024 * 1024,
            thumbnail_rail: 176.0,
        }
    }
}

impl HostConfig {
    /// Check the configuration is within the host's bounds.
    pub fn validate(&self) -> Result<(), HostError> {
        if self.parked_limit > 64 {
            return Err(HostError::invalid("parked_limit is at most 64"));
        }
        if self.parked_max_age > Duration::from_secs(600) {
            return Err(HostError::invalid("parked_max_age is at most ten minutes"));
        }
        if self.max_file_bytes == 0 {
            return Err(HostError::invalid("max_file_bytes must be positive"));
        }
        if !self.thumbnail_rail.is_finite() || !(0.0..=1000.0).contains(&self.thumbnail_rail) {
            return Err(HostError::invalid("thumbnail_rail is between 0 and 1000"));
        }
        Ok(())
    }
}

/// A PDF file path the app's file broker granted for reading. Build it only from the broker's
/// answer; the host never resolves a string the page supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedPdfPath(String);

impl AuthorizedPdfPath {
    /// Wrap an absolute, UTF-8 path the broker authorized.
    pub fn from_authorized(path: impl AsRef<Path>) -> Result<Self, HostError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| HostError::invalid("the path is not valid UTF-8"))?;
        if text.is_empty() || text.len() > 32 * 1024 || text.contains('\0') {
            return Err(HostError::invalid(
                "the path is empty, too long or holds a NUL",
            ));
        }
        if !path.is_absolute() {
            return Err(HostError::invalid("the path is not absolute"));
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A document password. No `Serialize`, no `Clone`, redacted `Debug`.
pub struct PdfPassword(String);

impl PdfPassword {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PdfPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PdfPassword(<redacted>)")
    }
}

/// Open a document in the window's PDF view.
#[derive(Debug)]
pub struct OpenRequest {
    pub owner: String,
    pub path: AuthorizedPdfPath,
    pub frame: HostFrame,
    /// Canvas colour behind the pages (sRGB).
    pub reader_rgb: Option<[u8; 3]>,
    pub initial_view: Option<HostViewMeta>,
    pub password: Option<PdfPassword>,
}

/// Change the open view. Every field is optional; absent fields change nothing.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequest {
    pub frame: Option<HostFrame>,
    pub reader_rgb: Option<[u8; 3]>,
    /// One-based page (clamped to the document).
    pub page: Option<u32>,
    pub scale: Option<f64>,
    /// `"width"`, `"page"`, or `""` to stop fitting.
    pub fit: Option<String>,
    pub rotation: Option<i32>,
    /// `"thumbs"` (native rail), `"outline"` (drawn by the page), `"none"` or `""`.
    pub panel: Option<String>,
    pub visible: Option<bool>,
}

/// Find text in the open view, or step through the matches of the last query.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindRequest {
    /// A new query; absent repeats the last one.
    pub query: Option<String>,
    pub match_case: Option<bool>,
    pub whole_word: Option<bool>,
    /// Positive: next; negative: previous.
    pub direction: Option<i32>,
    /// Zero-based match to start from.
    pub index: Option<usize>,
    /// Clear the highlight and forget the query.
    pub clear: Option<bool>,
}

/// The platform view's parent: the native webview the PDF view is added beside, and its geometry.
///
/// The pointers are valid only inside the platform callback that produced them, so the type is not
/// `Send` and not `Sync`: build it inside the callback, use it for one call, drop it there.
#[derive(Clone, Copy, Debug)]
pub struct NativeParent {
    pub(crate) webview: usize,
    pub(crate) superview: usize,
    #[cfg(target_os = "macos")]
    pub(crate) window_number: isize,
    #[cfg(target_os = "macos")]
    pub(crate) frame: HostFrame,
    _not_send: PhantomData<*const ()>,
}

impl NativeParent {
    /// No native view. Calls that need a parent are refused with a typed error; the unsupported
    /// host answers [`HostError::Unsupported`] before it looks.
    pub fn detached() -> Self {
        Self {
            webview: 0,
            superview: 0,
            #[cfg(target_os = "macos")]
            window_number: 0,
            #[cfg(target_os = "macos")]
            frame: HostFrame {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            _not_send: PhantomData,
        }
    }

    /// True when no native view is attached.
    pub fn is_detached(&self) -> bool {
        self.webview == 0 || self.superview == 0
    }

    #[cfg(test)]
    pub(crate) fn fake() -> Self {
        Self {
            webview: 1,
            superview: 2,
            #[cfg(target_os = "macos")]
            window_number: 1,
            #[cfg(target_os = "macos")]
            frame: HostFrame {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
            },
            _not_send: PhantomData,
        }
    }

    /// Read the parent from the platform's webview and window pointers (Tauri's
    /// `PlatformWebview::inner()` and `ns_window()`).
    ///
    /// # Safety
    ///
    /// `webview` must be a live `WKWebView` (an `NSView`) and `ns_window` null or a live `NSWindow`,
    /// both valid for the whole life of the returned value. Call it on the AppKit main thread, inside
    /// the callback that handed out the pointers, and drop the result there.
    #[cfg(target_os = "macos")]
    pub unsafe fn from_wkwebview(
        webview: *mut std::ffi::c_void,
        ns_window: *mut std::ffi::c_void,
    ) -> Result<Self, HostError> {
        pdfkit::native_parent(webview, ns_window)
    }
}

// ---------------------------------------------------------------------------------------------
// Validation and pure rules
// ---------------------------------------------------------------------------------------------

fn bad_text(text: &str) -> bool {
    text.chars().any(char::is_control)
}

pub(crate) fn validate_window(window: &str) -> Result<(), HostError> {
    if window.is_empty() || window.len() > MAX_WINDOW_LABEL_BYTES || bad_text(window) {
        return Err(HostError::invalid(
            "the window label is empty, too long or holds a control character",
        ));
    }
    Ok(())
}

pub(crate) fn validate_owner(owner: &str) -> Result<(), HostError> {
    if owner.is_empty() || owner.len() > MAX_OWNER_BYTES || bad_text(owner) {
        return Err(HostError::invalid(
            "the owner is empty, too long or holds a control character",
        ));
    }
    Ok(())
}

pub(crate) fn validate_frame(frame: HostFrame) -> Result<(), HostError> {
    let ok = [frame.x, frame.y, frame.width, frame.height]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= MAX_FRAME_EXTENT)
        && frame.width >= 0.0
        && frame.height >= 0.0;
    if ok {
        Ok(())
    } else {
        Err(HostError::invalid(
            "the frame is not finite, is negative in size or is out of range",
        ))
    }
}

fn validate_scale(scale: Option<f64>) -> Result<(), HostError> {
    match scale {
        Some(v) if !(v.is_finite() && v > 0.0 && v <= 100.0) => Err(HostError::invalid(
            "the scale must be a positive number up to 100",
        )),
        _ => Ok(()),
    }
}

fn validate_view_meta(meta: Option<&HostViewMeta>) -> Result<(), HostError> {
    let Some(meta) = meta else { return Ok(()) };
    validate_scale(meta.scale)?;
    match meta.fit.as_deref() {
        None | Some("width") | Some("page") => Ok(()),
        Some(_) => Err(HostError::invalid("fit is \"width\" or \"page\"")),
    }
}

pub(crate) fn validate_update(request: &UpdateRequest) -> Result<(), HostError> {
    if let Some(frame) = request.frame {
        validate_frame(frame)?;
    }
    validate_scale(request.scale)?;
    match request.fit.as_deref() {
        None | Some("") | Some("width") | Some("page") => {}
        Some(_) => return Err(HostError::invalid("fit is \"width\", \"page\" or empty")),
    }
    match request.panel.as_deref() {
        None | Some("") | Some("none") | Some("outline") | Some("thumbs") => {}
        Some(_) => return Err(HostError::invalid("unknown native PDF panel")),
    }
    Ok(())
}

pub(crate) fn validate_find(request: &FindRequest) -> Result<(), HostError> {
    if request
        .query
        .as_deref()
        .is_some_and(|q| q.len() > MAX_FIND_QUERY_BYTES)
    {
        return Err(HostError::invalid("the find query is too long"));
    }
    Ok(())
}

/// Round a rotation in degrees to 0, 90, 180 or 270.
pub fn normalized_rotation(rotation: i32) -> i32 {
    let value = rotation.rem_euclid(360);
    if !(45..315).contains(&value) {
        0
    } else if value < 135 {
        90
    } else if value < 225 {
        180
    } else {
        270
    }
}

/// Clamp a one-based page into `1..=count` (1 for an empty document).
pub fn clamp_page(page: u32, count: u32) -> u32 {
    if count == 0 {
        1
    } else {
        page.max(1).min(count)
    }
}

/// The active match after a find. A base index (held from the last find, or requested) is clamped;
/// without one the first match, or the last when `direction` is negative. Then, only when there was
/// a base and `direction` is non-zero, one step in that direction with wrap-around.
pub fn pick_active(count: usize, base: Option<usize>, direction: Option<i32>) -> usize {
    if count == 0 {
        return 0;
    }
    let active = match base {
        Some(index) => index.min(count - 1),
        None if direction.unwrap_or(1) < 0 => count - 1,
        None => 0,
    };
    match (base, direction.filter(|d| *d != 0)) {
        (Some(_), Some(d)) if d > 0 => (active + 1) % count,
        (Some(_), Some(_)) if active == 0 => count - 1,
        (Some(_), Some(_)) => active - 1,
        _ => active,
    }
}

fn content_identity(path: &str) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    let mut out = String::from("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Slot {
    window: String,
    owner: String,
    path: String,
    identity: Option<String>,
    generation: u64,
    view: ViewId,
}

struct FindSession {
    query: String,
    match_case: bool,
    whole_word: bool,
    index: Option<usize>,
}

struct Active {
    slot: Slot,
    find: Option<FindSession>,
}

struct Parked {
    slot: Slot,
    parked_at: Instant,
}

#[derive(Default)]
struct Table {
    active: HashMap<String, Active>,
    parked: Vec<Parked>,
}

/// The per-process native PDF host. Share it as `Arc<PdfHost>`.
pub struct PdfHost {
    backend: Box<dyn HostBackend>,
    config: HostConfig,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    table: Mutex<Table>,
    next_generation: AtomicU64,
    released: AtomicU64,
    outbox: Mutex<VecDeque<HostResourceEvent>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PdfHost {
    /// The host for this platform: PDFKit on macOS, the typed-unsupported host elsewhere. Only one
    /// PDFKit host may exist per process (the AppKit delegate class and key monitors are
    /// process-wide); a second call on macOS fails with [`HostError::Native`].
    pub fn new(config: HostConfig, hooks: HostHooks) -> Result<Self, HostError> {
        config.validate()?;
        #[cfg(target_os = "macos")]
        {
            let backend = pdfkit::PdfKitBackend::new(&config, hooks)?;
            Ok(Self::with_backend(Box::new(backend), config))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = hooks;
            Ok(Self::with_backend(Box::new(UnsupportedBackend), config))
        }
    }

    /// A host whose every call is [`HostError::Unsupported`], on any platform.
    pub fn unsupported() -> Self {
        Self::with_backend(Box::new(UnsupportedBackend), HostConfig::default())
    }

    pub(crate) fn with_backend(backend: Box<dyn HostBackend>, config: HostConfig) -> Self {
        Self::with_backend_and_clock(backend, config, Box::new(Instant::now))
    }

    pub(crate) fn with_backend_and_clock(
        backend: Box<dyn HostBackend>,
        config: HostConfig,
        clock: Box<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            backend,
            config,
            clock,
            table: Mutex::new(Table::default()),
            next_generation: AtomicU64::new(1),
            released: AtomicU64::new(0),
            outbox: Mutex::new(VecDeque::new()),
        }
    }

    /// True when this build can show a native PDF view.
    pub fn supported(&self) -> bool {
        self.backend.supported()
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    fn require_supported(&self) -> Result<(), HostError> {
        if self.backend.supported() {
            Ok(())
        } else {
            Err(HostError::Unsupported)
        }
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        locked(&self.table)
    }

    /// Active, parked and released-so-far counts.
    pub fn counts(&self) -> HostCounts {
        let table = self.table();
        HostCounts {
            active: table.active.len(),
            parked: table.parked.len(),
            released: self.released.load(Ordering::Relaxed),
        }
    }

    /// The events recorded since the last drain, oldest first.
    pub fn drain_events(&self) -> Vec<HostResourceEvent> {
        locked(&self.outbox).drain(..).collect()
    }

    /// Record an event. Never call this while holding the table lock.
    fn emit(&self, slot: &Slot, phase: ResourcePhase, reason: &str) {
        let counts = self.counts();
        let event = HostResourceEvent {
            window: slot.window.clone(),
            owner: slot.owner.clone(),
            path: slot.path.clone(),
            identity: slot.identity.clone(),
            generation: slot.generation,
            phase,
            reason: reason.to_owned(),
            active_count: counts.active,
            parked_count: counts.parked,
            released_count: counts.released,
        };
        let mut outbox = locked(&self.outbox);
        if outbox.len() >= EVENT_OUTBOX_CAP {
            outbox.pop_front();
        }
        outbox.push_back(event);
    }

    /// Release one view natively, count it, and record why. Never call it with the table locked.
    fn release_slot(&self, slot: &Slot, restore_focus: bool, reason: &str) {
        self.backend.release(slot.view, restore_focus);
        self.released.fetch_add(1, Ordering::Relaxed);
        self.emit(slot, ResourcePhase::Released, reason);
    }

    // -- open ------------------------------------------------------------------------------

    /// Show `request.path` in `window`'s PDF view, replacing the window's active view. A suspended
    /// view of the same window and path is reused when the file's digest still matches.
    pub fn open(
        &self,
        window: &str,
        parent: &NativeParent,
        request: OpenRequest,
    ) -> Result<HostState, HostError> {
        self.require_supported()?;
        validate_window(window)?;
        validate_owner(&request.owner)?;
        validate_frame(request.frame)?;
        validate_view_meta(request.initial_view.as_ref())?;
        if request
            .password
            .as_ref()
            .is_some_and(|p| p.as_str().len() > MAX_PASSWORD_BYTES)
        {
            return Err(HostError::invalid("the password is too long"));
        }
        let path = request.path.as_str().to_owned();
        self.check_file(&path)?;
        let identity = content_identity(&path);
        self.sweep();

        let reused = self.take_parked(window, &path, &identity);
        let (reuse_view, generation) = match &reused {
            Some(parked) => (Some(parked.slot.view), parked.slot.generation),
            None => (None, self.next_generation.fetch_add(1, Ordering::Relaxed)),
        };
        let opened = self.backend.open(&BackendOpen {
            window,
            owner: &request.owner,
            path: &path,
            parent,
            frame: request.frame,
            reader_rgb: request.reader_rgb,
            initial_view: request.initial_view.as_ref(),
            password: request.password.as_ref().map(PdfPassword::as_str),
            reuse: reuse_view,
        });
        let (view, view_state) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                // A reused view left the cache when it was taken; do not leak it.
                if let Some(parked) = reused {
                    self.release_slot(&parked.slot, false, "open-failed");
                }
                return Err(error);
            }
        };
        let slot = Slot {
            window: window.to_owned(),
            owner: request.owner.clone(),
            path,
            identity,
            generation,
            view,
        };
        let previous = self.table().active.insert(
            window.to_owned(),
            Active {
                slot: slot.clone(),
                find: None,
            },
        );
        if let Some(previous) = previous {
            self.release_slot(&previous.slot, true, "open-replaced");
        }
        self.emit(
            &slot,
            ResourcePhase::Active,
            if reuse_view.is_some() {
                "resume"
            } else {
                "open"
            },
        );
        Ok(HostState::new(&request.owner, view_state))
    }

    fn check_file(&self, path: &str) -> Result<(), HostError> {
        let meta = std::fs::metadata(path).map_err(|e| {
            HostError::File(match e.kind() {
                std::io::ErrorKind::NotFound => "the file does not exist".into(),
                std::io::ErrorKind::PermissionDenied => "the file cannot be read".into(),
                _ => "the file cannot be inspected".into(),
            })
        })?;
        if !meta.is_file() {
            return Err(HostError::File("the path is not a regular file".into()));
        }
        if meta.len() > self.config.max_file_bytes {
            return Err(HostError::File(
                "the file is larger than the host limit".into(),
            ));
        }
        Ok(())
    }

    /// Remove a parked view of this window and path. It is returned only when fresh and its digest
    /// equals `identity`; any other is released here with the reason.
    fn take_parked(&self, window: &str, path: &str, identity: &Option<String>) -> Option<Parked> {
        let entry = {
            let mut table = self.table();
            let index = table
                .parked
                .iter()
                .position(|p| p.slot.window == window && p.slot.path == path)?;
            table.parked.remove(index)
        };
        let expired =
            self.now().saturating_duration_since(entry.parked_at) > self.config.parked_max_age;
        let same = entry.slot.identity.is_some() && entry.slot.identity == *identity;
        if !expired && same {
            Some(entry)
        } else {
            let reason = if expired {
                "age-expired"
            } else {
                "identity-mismatch"
            };
            self.release_slot(&entry.slot, false, reason);
            None
        }
    }

    // -- per-view calls --------------------------------------------------------------------

    /// The view of `window`, when `owner` is its owner. The window key comes first, so a view of
    /// another window is never reachable.
    fn view_of(&self, window: &str, owner: &str) -> Result<ViewId, HostError> {
        let table = self.table();
        let active = table.active.get(window).ok_or(HostError::NotOpen)?;
        if active.slot.owner != owner {
            return Err(HostError::StaleOwner);
        }
        Ok(active.slot.view)
    }

    /// Change the open view. Needs the parent for frame and rail geometry.
    pub fn update(
        &self,
        window: &str,
        parent: &NativeParent,
        owner: &str,
        request: &UpdateRequest,
    ) -> Result<HostState, HostError> {
        self.require_supported()?;
        validate_window(window)?;
        validate_update(request)?;
        let view = self.view_of(window, owner)?;
        let state = self.backend.update(view, parent, request)?;
        Ok(HostState::new(owner, state))
    }

    /// The open view's state.
    pub fn state(&self, window: &str, owner: &str) -> Result<HostState, HostError> {
        self.require_supported()?;
        let view = self.view_of(window, owner)?;
        let state = self.backend.state(view)?;
        Ok(HostState::new(owner, state))
    }

    /// Find text, or step to the next or previous match of the last query.
    pub fn find(
        &self,
        window: &str,
        owner: &str,
        request: &FindRequest,
    ) -> Result<HostFindResult, HostError> {
        self.require_supported()?;
        validate_find(request)?;
        let view = self.view_of(window, owner)?;
        let none = HostFindResult {
            count: 0,
            active_index: -1,
        };
        if request.clear.unwrap_or(false) || request.query.as_deref().is_some_and(str::is_empty) {
            self.backend.clear_find(view)?;
            if let Some(active) = self.table().active.get_mut(window) {
                active.find = None;
            }
            return Ok(none);
        }
        // The session is read and written in short critical sections, never across the backend.
        let (query, match_case, whole_word, held) = {
            let mut table = self.table();
            let active = table.active.get_mut(window).ok_or(HostError::NotOpen)?;
            if let Some(query) = request.query.as_deref().filter(|q| !q.is_empty()) {
                active.find = Some(FindSession {
                    query: query.to_owned(),
                    match_case: request.match_case.unwrap_or(false),
                    whole_word: request.whole_word.unwrap_or(false),
                    index: request.index,
                });
            }
            let session = active
                .find
                .as_ref()
                .ok_or_else(|| HostError::invalid("the find query is empty"))?;
            (
                session.query.clone(),
                session.match_case,
                session.whole_word,
                session.index,
            )
        };
        let base = request.index.or(held);
        let direction = request.direction;
        let (count, active_index) = self.backend.find(
            view,
            &FindQuery {
                text: &query,
                match_case,
                whole_word,
            },
            &|count| pick_active(count, base, direction),
        )?;
        if let Some(active) = self.table().active.get_mut(window) {
            if let Some(session) = active.find.as_mut() {
                session.index = active_index;
            }
        }
        Ok(match active_index {
            Some(index) if count > 0 => HostFindResult {
                count,
                active_index: index as i32,
            },
            _ => none,
        })
    }

    /// Show the print panel for the open view. `false` when the document is locked or forbids
    /// printing, or the person cancelled. Runs a modal panel: the host holds no lock meanwhile.
    pub fn print(&self, window: &str, owner: &str) -> Result<bool, HostError> {
        self.require_supported()?;
        let view = self.view_of(window, owner)?;
        self.backend.print(view)
    }

    // -- suspend / close -------------------------------------------------------------------

    /// Hide the window's view and keep it briefly for reuse. An encrypted document, or one whose
    /// digest could not be taken, is released instead.
    pub fn suspend(&self, window: &str, owner: &str, path: &str) -> Result<(), HostError> {
        self.require_supported()?;
        let removed = {
            let mut table = self.table();
            let active = table.active.get(window).ok_or(HostError::NotOpen)?;
            if active.slot.owner != owner || active.slot.path != path {
                return Err(HostError::StaleOwner);
            }
            table.active.remove(window)
        };
        let Some(active) = removed else {
            return Ok(());
        };
        let slot = active.slot;
        let encrypted = self.backend.is_encrypted(slot.view);
        if encrypted || slot.identity.is_none() {
            let reason = if encrypted {
                "encrypted"
            } else {
                "identity-unavailable"
            };
            self.release_slot(&slot, true, reason);
            return Ok(());
        }
        if self.config.parked_limit == 0 || self.backend.park(slot.view).is_err() {
            self.release_slot(&slot, true, "park-failed");
            return Ok(());
        }
        let now = self.now();
        let evicted = {
            let mut table = self.table();
            let mut evicted: Vec<(Slot, &'static str)> = Vec::new();
            let max_age = self.config.parked_max_age;
            let (expired, kept): (Vec<_>, Vec<_>) = table
                .parked
                .drain(..)
                .partition(|p| now.saturating_duration_since(p.parked_at) > max_age);
            table.parked = kept;
            evicted.extend(expired.into_iter().map(|p| (p.slot, "age-expired")));
            if let Some(index) = table
                .parked
                .iter()
                .position(|p| p.slot.window == slot.window && p.slot.path == slot.path)
            {
                evicted.push((table.parked.remove(index).slot, "cache-evicted"));
            }
            table.parked.push(Parked {
                slot: slot.clone(),
                parked_at: now,
            });
            let overflow = table.parked.len().saturating_sub(self.config.parked_limit);
            evicted.extend(
                table
                    .parked
                    .drain(..overflow)
                    .map(|p| (p.slot, "cache-evicted")),
            );
            evicted
        };
        for (slot, reason) in &evicted {
            self.release_slot(slot, false, reason);
        }
        self.emit(&slot, ResourcePhase::Parked, "suspend");
        Ok(())
    }

    /// Close the window's views. `owner` guards the active view against a stale call; `path`
    /// closes the active view and any suspended view of that path (an app-level close after a tab is
    /// removed). At least one of the two is required. An owner that is not the active view's owner
    /// is refused unless a suspended view was released.
    pub fn close(
        &self,
        window: &str,
        owner: Option<&str>,
        path: Option<&str>,
        reason: &str,
    ) -> Result<(), HostError> {
        self.require_supported()?;
        validate_window(window)?;
        let owner = owner.filter(|o| !o.is_empty());
        let path = path.filter(|p| !p.is_empty());
        if owner.is_none() && path.is_none() {
            return Err(HostError::invalid("close needs an owner or a path"));
        }
        let reason = if reason.is_empty() || reason.len() > 64 || bad_text(reason) {
            "terminal-close"
        } else {
            reason
        };
        let (removed, parked, owner_mismatch) = {
            let mut table = self.table();
            let (owner_mismatch, path_mismatch) = match table.active.get(window) {
                Some(active) => (
                    owner.is_some_and(|o| active.slot.owner != o),
                    path.is_some_and(|p| active.slot.path != p),
                ),
                None => (false, false),
            };
            let removed = if owner_mismatch || path_mismatch {
                None
            } else {
                table.active.remove(window)
            };
            let mut taken = Vec::new();
            let mut kept = Vec::new();
            for entry in table.parked.drain(..) {
                let matches = entry.slot.window == window
                    && match path {
                        Some(path) => entry.slot.path == path,
                        None => owner.is_some_and(|o| entry.slot.owner == o),
                    };
                if matches {
                    taken.push(entry);
                } else {
                    kept.push(entry);
                }
            }
            table.parked = kept;
            (removed, taken, owner_mismatch)
        };
        if owner_mismatch && parked.is_empty() {
            return Err(HostError::StaleOwner);
        }
        if let Some(active) = removed {
            self.release_slot(&active.slot, true, reason);
        }
        for entry in &parked {
            self.release_slot(&entry.slot, false, reason);
        }
        Ok(())
    }

    // -- teardown --------------------------------------------------------------------------

    /// Release every view of a window that is gone (active and suspended).
    pub fn release_window(&self, window: &str) {
        self.release_matching(|w| w == window, "window-destroyed");
    }

    /// Release the views of every window not in `live`. Safety net for a missed destroy event.
    pub fn reap<S: AsRef<str>>(&self, live: &[S]) {
        self.release_matching(|w| !live.iter().any(|l| l.as_ref() == w), "window-gone");
    }

    /// Release everything (app exit).
    pub fn dispose_all(&self) {
        self.release_matching(|_| true, "shutdown");
    }

    fn release_matching(&self, select: impl Fn(&str) -> bool, reason: &str) {
        let (active, parked) = {
            let mut table = self.table();
            let windows: Vec<String> = table.active.keys().filter(|w| select(w)).cloned().collect();
            let active: Vec<Active> = windows
                .iter()
                .filter_map(|w| table.active.remove(w))
                .collect();
            let mut taken = Vec::new();
            let mut kept = Vec::new();
            for entry in table.parked.drain(..) {
                if select(&entry.slot.window) {
                    taken.push(entry);
                } else {
                    kept.push(entry);
                }
            }
            table.parked = kept;
            (active, taken)
        };
        for entry in &active {
            self.release_slot(&entry.slot, false, reason);
        }
        for entry in &parked {
            self.release_slot(&entry.slot, false, reason);
        }
    }

    /// Release suspended views older than [`HostConfig::parked_max_age`]. Returns how many.
    pub fn sweep(&self) -> usize {
        let now = self.now();
        let max_age = self.config.parked_max_age;
        let expired = {
            let mut table = self.table();
            let (expired, kept): (Vec<_>, Vec<_>) = table
                .parked
                .drain(..)
                .partition(|p| now.saturating_duration_since(p.parked_at) > max_age);
            table.parked = kept;
            expired
        };
        for entry in &expired {
            self.release_slot(&entry.slot, false, "age-expired");
        }
        expired.len()
    }

    /// How long until the oldest suspended view expires; `None` when none is suspended.
    pub fn next_sweep_in(&self) -> Option<Duration> {
        let now = self.now();
        let table = self.table();
        table
            .parked
            .iter()
            .map(|p| {
                let age = now.saturating_duration_since(p.parked_at);
                self.config.parked_max_age.saturating_sub(age)
            })
            .min()
    }
}

impl std::fmt::Debug for PdfHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfHost")
            .field("supported", &self.supported())
            .field("counts", &self.counts())
            .finish()
    }
}
