//! Shell authorization for the native PDF view (feature `pdf-host`, Orthic-Labs/rightkit PK08-6).
//!
//! `rightkit-pdf`'s `host` feature owns the view lifecycle: one PDFKit view per window, owner
//! isolation, the suspended-view cache, find, print. It knows no window system and no IPC. This
//! module is the other half: it decides **who may drive that view** and hands the owner the native
//! parent view it needs. The dependency runs one way, shell to PDF owner (never the reverse).
//!
//! # Who may call
//!
//! Every `rightkit_pdf_*` command reads its caller from the native invocation context (the
//! `Webview` Tauri injects), never from an argument, and asks the host policy first
//! ([`crate::host::HostServices::service_for`] under [`PDF_SERVICE`]): an app window (not a child
//! webview), not a registered content webview, not a preview document, on the app's own origin, and
//! trusted by the service's [`crate::host::ServiceTrust`] (labels, optional origins, optional
//! actions: `open`, `update`, `state`, `find`, `print`, `suspend`, `close`). A refusal answers
//! `permission_denied` before any path is resolved or any native object is touched. The window the
//! view belongs to is that webview's own label, so a window cannot name another window's document;
//! the owner token inside a window is a stale-call guard supplied by the page.
//!
//! # Which file
//!
//! The renderer sends a path string; the native layer never opens it. A [`PdfPathAuthority`] maps
//! it to a file the app granted (default: [`BrokerAuthority`] over the shell's `PathBroker`, the
//! same grants `rk_shell_path_require` checks) and only that answer reaches the view. Grants are
//! app-wide in the shipped broker, so an app that needs per-window grants supplies its own
//! authority; it is told the window label.
//!
//! # Threads
//!
//! The view's objects are main-thread objects. The commands are `async`, hop to the main thread
//! for each operation (`with_webview` where the parent geometry is needed) and wait on a channel off
//! the async executor. The native geometry pointers never leave the callback that produced them.
//! A window that is destroyed releases its views (the service hooks the window's destroy event the
//! first time it opens a view there; [`cleanup_window`] is the explicit form), and every operation
//! also releases the views of windows that no longer exist. One short-lived thread sleeps until the
//! oldest suspended view expires and then dispatches the sweep to the main thread; none runs while
//! nothing is suspended.
//!
//! # Platforms
//!
//! macOS drives PDFKit. Elsewhere every command answers `unsupported` (typed, no panic, nothing
//! allocated), before the path authority is consulted.
//!
//! # Wire
//!
//! Commands: `rightkit_pdf_open`, `_update`, `_state`, `_find`, `_print`, `_suspend`, `_close`
//! (automatically registered by the shared `handler!`). Events to the owning window only:
//! `rightkit://pdf-state` ([`rightkit_pdf::host::HostState`]), `rightkit://pdf-resource`
//! ([`rightkit_pdf::host::HostResourceEvent`]) and `rightkit://pdf-key` for the keys the app claimed
//! while the PDF view has focus.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rightkit_pdf::host::{
    AuthorizedPdfPath, FindRequest, HostConfig, HostError as PdfError, HostFindResult, HostFrame,
    HostHooks, HostKey, HostState, HostViewMeta, OpenRequest, PdfHost, PdfPassword, UpdateRequest,
};
use tauri::{AppHandle, Emitter, Manager, Webview, Wry};

use crate::broker::PathBroker;
use crate::caller_gate;
use crate::error::ShellError;
use crate::host::{HostError, HostServices, HostServicesBuilder, ServiceId, ServiceTrust};
use crate::ShellState;

/// The host service the PDF commands are registered under.
pub const PDF_SERVICE: ServiceId = ServiceId::new("pdf");
/// Sent to the owning window after `open` and `update`: the view's [`HostState`].
pub const EVENT_STATE: &str = "rightkit://pdf-state";
/// Sent to the owning window on every lifecycle change: a `HostResourceEvent`.
pub const EVENT_RESOURCE: &str = "rightkit://pdf-resource";
/// Sent to the owning window for a key the app claimed while the PDF view has focus.
pub const EVENT_KEY: &str = "rightkit://pdf-key";

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Map a PDF host failure to the shell's host error. The code is the host's own stable code and the
/// message carries no path or document text.
pub fn from_pdf_error(error: PdfError) -> HostError {
    HostError::new(error.code(), error.to_string())
}

/// Which PDF file a window may open: the app's file grants.
pub trait PdfPathAuthority: Send + Sync {
    /// Resolve the renderer-supplied `path` to the absolute path of an existing file the app granted
    /// for reading, or refuse. `window` is the label of the caller's own window.
    fn authorize_read(&self, window: &str, path: &str) -> Result<PathBuf, ShellError>;
}

/// The default authority: the shell's [`PathBroker`] (`require_file`), app-wide grants.
pub struct BrokerAuthority {
    broker: Arc<PathBroker>,
}

impl BrokerAuthority {
    pub fn new(broker: Arc<PathBroker>) -> Self {
        Self { broker }
    }

    /// The broker the installed shell holds. Fails when the shell is not installed yet.
    pub fn from_app(app: &AppHandle<Wry>) -> Result<Self, HostError> {
        let state = app.try_state::<ShellState>().ok_or_else(|| {
            HostError::new("permission_denied", "forbidden: the shell is not installed")
        })?;
        Ok(Self::new(Arc::clone(&state.broker)))
    }
}

impl PdfPathAuthority for BrokerAuthority {
    fn authorize_read(&self, _window: &str, path: &str) -> Result<PathBuf, ShellError> {
        self.broker.require_file(path).map(PathBuf::from)
    }
}

/// The PDF host service: the native host, the file authority and the bookkeeping the shell adds.
/// Register it with [`PdfServiceExt::pdf`].
pub struct PdfHostService {
    host: Arc<PdfHost>,
    authority: Arc<dyn PdfPathAuthority>,
    sweeping: Arc<AtomicBool>,
    hooked: Arc<Mutex<HashSet<String>>>,
}

impl PdfHostService {
    /// Wrap a host and a file authority.
    pub fn new(host: Arc<PdfHost>, authority: Arc<dyn PdfPathAuthority>) -> Self {
        Self {
            host,
            authority,
            sweeping: Arc::new(AtomicBool::new(false)),
            hooked: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Build the platform host wired to the app: links open through the shell's checked opener
    /// (http, https and mailto only), keys `claim_key` accepts are sent to the owning window as
    /// [`EVENT_KEY`], and files come from the shell's broker. Call after the shell plugin is set up.
    /// Only one PDFKit host may exist per process.
    pub fn create(
        app: &AppHandle<Wry>,
        config: HostConfig,
        claim_key: impl Fn(&HostKey) -> bool + Send + Sync + 'static,
    ) -> Result<Self, HostError> {
        let authority = BrokerAuthority::from_app(app)?;
        let link_app = app.clone();
        let key_app = app.clone();
        let hooks = HostHooks::default()
            .with_open_link(move |url| {
                link_app
                    .try_state::<ShellState>()
                    .is_some_and(|state| crate::opener::open_checked(&*state.opener, url).is_ok())
            })
            .with_key_claim(claim_key, move |window, key| {
                let _ = key_app.emit_to(window, EVENT_KEY, key);
            });
        let host = PdfHost::new(config, hooks).map_err(from_pdf_error)?;
        Ok(Self::new(Arc::new(host), Arc::new(authority)))
    }

    /// The native host.
    pub fn host(&self) -> &Arc<PdfHost> {
        &self.host
    }

    /// The typed answer on a platform with no native PDF view.
    pub fn require_supported(&self) -> Result<(), HostError> {
        if self.host.supported() {
            Ok(())
        } else {
            Err(from_pdf_error(PdfError::Unsupported))
        }
    }

    /// Turn the renderer's strings into an open request. The only path that reaches the host is the
    /// authority's answer.
    #[allow(clippy::too_many_arguments)]
    pub fn open_request(
        &self,
        window: &str,
        owner: String,
        path: &str,
        frame: HostFrame,
        reader_rgb: Option<[u8; 3]>,
        initial_view: Option<HostViewMeta>,
        password: Option<String>,
    ) -> Result<OpenRequest, HostError> {
        let granted = self.authority.authorize_read(window, path)?;
        Ok(OpenRequest {
            owner,
            path: AuthorizedPdfPath::from_authorized(granted).map_err(from_pdf_error)?,
            frame,
            reader_rgb,
            initial_view,
            password: password.map(PdfPassword::new),
        })
    }

    /// The path form the host stored for a document: the authority's answer, or the renderer's own
    /// string when the file can no longer be authorized (closing and suspending only compare it).
    fn stored_path(&self, window: &str, path: &str) -> String {
        self.authority
            .authorize_read(window, path)
            .ok()
            .and_then(|granted| granted.to_str().map(str::to_owned))
            .unwrap_or_else(|| path.to_owned())
    }

    // -- operations (each hops to the main thread) ------------------------------------------

    /// Show a granted PDF in the caller's window.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        app: &AppHandle<Wry>,
        window: &str,
        owner: String,
        path: &str,
        frame: HostFrame,
        reader_rgb: Option<[u8; 3]>,
        initial_view: Option<HostViewMeta>,
        password: Option<String>,
    ) -> Result<HostState, HostError> {
        self.require_supported()?;
        let request = self.open_request(
            window,
            owner,
            path,
            frame,
            reader_rgb,
            initial_view,
            password,
        )?;
        let host = Arc::clone(&self.host);
        let live = live_windows(app);
        let label = window.to_owned();
        let state = with_parent(app, window, move |parent| {
            host.reap(&live);
            host.open(&label, parent, request)
        });
        self.flush(app);
        let state = state?;
        self.hook_destroy(app, window);
        let _ = app.emit_to(window, EVENT_STATE, &state);
        Ok(state)
    }

    /// Change the open view.
    pub fn update(
        &self,
        app: &AppHandle<Wry>,
        window: &str,
        owner: String,
        request: UpdateRequest,
    ) -> Result<HostState, HostError> {
        self.require_supported()?;
        let host = Arc::clone(&self.host);
        let live = live_windows(app);
        let label = window.to_owned();
        let state = with_parent(app, window, move |parent| {
            host.reap(&live);
            host.update(&label, parent, &owner, &request)
        });
        self.flush(app);
        let state = state?;
        let _ = app.emit_to(window, EVENT_STATE, &state);
        Ok(state)
    }

    /// The open view's state.
    pub fn state(
        &self,
        app: &AppHandle<Wry>,
        window: &str,
        owner: String,
    ) -> Result<HostState, HostError> {
        self.require_supported()?;
        let label = window.to_owned();
        self.on_main(app, move |host| host.state(&label, &owner))
    }

    /// Find text, or step through matches.
    pub fn find(
        &self,
        app: &AppHandle<Wry>,
        window: &str,
        owner: String,
        request: FindRequest,
    ) -> Result<HostFindResult, HostError> {
        self.require_supported()?;
        let label = window.to_owned();
        self.on_main(app, move |host| host.find(&label, &owner, &request))
    }

    /// Show the print panel for the open view.
    pub fn print(
        &self,
        app: &AppHandle<Wry>,
        window: &str,
        owner: String,
    ) -> Result<bool, HostError> {
        self.require_supported()?;
        let label = window.to_owned();
        self.on_main(app, move |host| host.print(&label, &owner))
    }

    /// Hide the view and keep it briefly for reuse.
    pub fn suspend(
        &self,
        app: &AppHandle<Wry>,
        window: &str,
        owner: String,
        path: &str,
    ) -> Result<(), HostError> {
        self.require_supported()?;
        let label = window.to_owned();
        let path = self.stored_path(window, path);
        let result = self.on_main(app, move |host| host.suspend(&label, &owner, &path));
        self.schedule_sweep(app);
        result
    }

    /// Close the window's views. An empty `owner` is the app-level close by path.
    pub fn close(
        &self,
        app: &AppHandle<Wry>,
        window: &str,
        owner: String,
        path: Option<String>,
        reason: Option<String>,
    ) -> Result<(), HostError> {
        self.require_supported()?;
        let label = window.to_owned();
        let path = path
            .filter(|p| !p.is_empty())
            .map(|p| self.stored_path(window, &p));
        let reason = reason.unwrap_or_else(|| "terminal-close".to_owned());
        self.on_main(app, move |host| {
            let owner = (!owner.is_empty()).then_some(owner.as_str());
            host.close(&label, owner, path.as_deref(), &reason)
        })
    }

    // -- plumbing ---------------------------------------------------------------------------

    /// Run `f` on the main thread (after releasing the views of windows that are gone), wait for it,
    /// then emit the lifecycle events it recorded.
    fn on_main<T: Send + 'static>(
        &self,
        app: &AppHandle<Wry>,
        f: impl FnOnce(&PdfHost) -> Result<T, PdfError> + Send + 'static,
    ) -> Result<T, HostError> {
        let host = Arc::clone(&self.host);
        let live = live_windows(app);
        let result = run_on_main(app, move || {
            host.reap(&live);
            f(host.as_ref())
        });
        self.flush(app);
        result
    }

    fn flush(&self, app: &AppHandle<Wry>) {
        flush_events(&self.host, app);
    }

    /// Release a window's views when the window is destroyed. Once per window label.
    fn hook_destroy(&self, app: &AppHandle<Wry>, window: &str) {
        if !locked(&self.hooked).insert(window.to_owned()) {
            return;
        }
        let Some(target) = app.get_webview_window(window) else {
            locked(&self.hooked).remove(window);
            return;
        };
        let host = Arc::clone(&self.host);
        let hooked = Arc::clone(&self.hooked);
        let app = app.clone();
        let label = window.to_owned();
        target.on_window_event(move |event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                locked(&hooked).remove(&label);
                release_window_later(&host, &app, &label);
            }
        });
    }

    /// Dispatch one sweep to the main thread when the oldest suspended view expires. At most one
    /// helper thread exists, and none while nothing is suspended.
    fn schedule_sweep(&self, app: &AppHandle<Wry>) {
        if self.host.next_sweep_in().is_none() || self.sweeping.swap(true, Ordering::AcqRel) {
            return;
        }
        let host = Arc::clone(&self.host);
        let flag = Arc::clone(&self.sweeping);
        let app = app.clone();
        let spawned = std::thread::Builder::new()
            .name("rk-pdf-sweep".into())
            .spawn({
                let flag = Arc::clone(&flag);
                move || loop {
                    match host.next_sweep_in() {
                        Some(wait) => {
                            let wait =
                                wait.max(Duration::from_millis(250)) + Duration::from_millis(50);
                            std::thread::sleep(wait);
                            let host = Arc::clone(&host);
                            let app2 = app.clone();
                            let _ = app.run_on_main_thread(move || {
                                host.sweep();
                                flush_events(&host, &app2);
                            });
                            // Let the main thread run it before the next look.
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        None => {
                            flag.store(false, Ordering::Release);
                            // A view suspended after the look above must not be left unscheduled.
                            if host.next_sweep_in().is_some() && !flag.swap(true, Ordering::AcqRel)
                            {
                                continue;
                            }
                            break;
                        }
                    }
                }
            });
        if spawned.is_err() {
            flag.store(false, Ordering::Release);
        }
    }
}

fn live_windows(app: &AppHandle<Wry>) -> Vec<String> {
    app.webview_windows().keys().cloned().collect()
}

fn flush_events(host: &PdfHost, app: &AppHandle<Wry>) {
    for event in host.drain_events() {
        let _ = app.emit_to(event.window.clone(), EVENT_RESOURCE, &event);
    }
}

fn release_window_later(host: &Arc<PdfHost>, app: &AppHandle<Wry>, label: &str) {
    let host = Arc::clone(host);
    let app2 = app.clone();
    let label = label.to_owned();
    let _ = app.run_on_main_thread(move || {
        host.release_window(&label);
        flush_events(&host, &app2);
    });
}

/// Run `f` on the main thread and wait for its answer.
fn run_on_main<T: Send + 'static>(
    app: &AppHandle<Wry>,
    f: impl FnOnce() -> Result<T, PdfError> + Send + 'static,
) -> Result<T, HostError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.send(f());
    })
    .map_err(|e| HostError::other(format!("dispatch native PDF view: {e}")))?;
    receiver
        .recv()
        .map_err(|_| HostError::other("the native PDF operation was interrupted"))?
        .map_err(from_pdf_error)
}

/// Run `f` on the main thread with the caller window's native parent view, and wait.
#[cfg(target_os = "macos")]
fn with_parent<T: Send + 'static>(
    app: &AppHandle<Wry>,
    window: &str,
    f: impl FnOnce(&rightkit_pdf::host::NativeParent) -> Result<T, PdfError> + Send + 'static,
) -> Result<T, HostError> {
    let target = app
        .get_webview_window(window)
        .ok_or_else(|| from_pdf_error(PdfError::NotOpen))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    target
        .with_webview(move |platform| {
            let result = (|| {
                // SAFETY: this runs inside Tauri's platform callback, on the AppKit main thread, and
                // the pointers (`inner()` is the window's WKWebView, `ns_window()` its NSWindow) are
                // valid until the callback returns. `NativeParent` is `!Send` and dies here.
                let parent = unsafe {
                    rightkit_pdf::host::NativeParent::from_wkwebview(
                        platform.inner(),
                        platform.ns_window(),
                    )
                }?;
                f(&parent)
            })();
            let _ = sender.send(result);
        })
        .map_err(|e| HostError::other(format!("dispatch native PDF view: {e}")))?;
    receiver
        .recv()
        .map_err(|_| HostError::other("the native PDF operation was interrupted"))?
        .map_err(from_pdf_error)
}

/// No native PDF view off macOS: the typed answer, with nothing dispatched.
#[cfg(not(target_os = "macos"))]
fn with_parent<T: Send + 'static>(
    _app: &AppHandle<Wry>,
    _window: &str,
    _f: impl FnOnce(&rightkit_pdf::host::NativeParent) -> Result<T, PdfError> + Send + 'static,
) -> Result<T, HostError> {
    Err(from_pdf_error(PdfError::Unsupported))
}

/// Register [`PdfHostService`] with its trust.
pub trait PdfServiceExt: Sized {
    /// Register the PDF service under [`PDF_SERVICE`]. Trust the window that shows documents; a
    /// registered content webview is refused whatever the trust says.
    fn pdf(self, trust: ServiceTrust, service: PdfHostService) -> Self;
}

impl PdfServiceExt for HostServicesBuilder {
    fn pdf(self, trust: ServiceTrust, service: PdfHostService) -> Self {
        self.service(PDF_SERVICE, trust, service)
    }
}

/// Release every view of a window now (explicit form of the destroy hook). No-op when no PDF
/// service is installed.
pub fn cleanup_window(app: &AppHandle<Wry>, label: &str) {
    if let Some(service) = installed(app) {
        release_window_later(service.host(), app, label);
    }
}

/// Release every view (app exit). Runs on the main thread and does not wait.
pub fn dispose_all(app: &AppHandle<Wry>) {
    if let Some(service) = installed(app) {
        let host = Arc::clone(service.host());
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            host.dispose_all();
            flush_events(&host, &app2);
        });
    }
}

fn installed(app: &AppHandle<Wry>) -> Option<Arc<PdfHostService>> {
    app.try_state::<HostServices>()?
        .service::<PdfHostService>(PDF_SERVICE)
}

/// The caller's service and window, after the host policy allowed `action`. The window is the
/// invoking webview's own label.
fn caller(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<(Arc<PdfHostService>, String), HostError> {
    let app = webview.app_handle();
    let services = crate::host::managed(app)?;
    let owned = caller_gate::gather(webview)?;
    let service = authorize(&services, action, &owned.facts())?;
    Ok((service, owned.label))
}

/// The pure decision behind every command: may a caller with these facts run `action` of the PDF
/// service. On success, the service instance.
pub fn authorize(
    services: &HostServices,
    action: &str,
    facts: &caller_gate::CallerFacts<'_>,
) -> Result<Arc<PdfHostService>, HostError> {
    services.service_for::<PdfHostService>(PDF_SERVICE, action, facts)
}

// -- commands ---------------------------------------------------------------------------------

/// Show a granted PDF in the caller's window. Answers the view's state.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn rightkit_pdf_open(
    webview: Webview<Wry>,
    owner: String,
    path: String,
    frame: HostFrame,
    reader_rgb: Option<[u8; 3]>,
    initial_view: Option<HostViewMeta>,
    password: Option<String>,
) -> Result<HostState, HostError> {
    let (service, window) = caller(&webview, "open")?;
    let app = webview.app_handle().clone();
    crate::host::blocking(move || {
        service.open(
            &app,
            &window,
            owner,
            &path,
            frame,
            reader_rgb,
            initial_view,
            password,
        )
    })
    .await
}

/// Change the open view (frame, page, zoom, fit, rotation, panel, visibility).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn rightkit_pdf_update(
    webview: Webview<Wry>,
    owner: String,
    frame: Option<HostFrame>,
    reader_rgb: Option<[u8; 3]>,
    page: Option<u32>,
    scale: Option<f64>,
    fit: Option<String>,
    rotation: Option<i32>,
    panel: Option<String>,
    visible: Option<bool>,
) -> Result<HostState, HostError> {
    let (service, window) = caller(&webview, "update")?;
    let app = webview.app_handle().clone();
    let request = UpdateRequest {
        frame,
        reader_rgb,
        page,
        scale,
        fit,
        rotation,
        panel,
        visible,
    };
    crate::host::blocking(move || service.update(&app, &window, owner, request)).await
}

/// The open view's state.
#[tauri::command]
pub async fn rightkit_pdf_state(
    webview: Webview<Wry>,
    owner: String,
) -> Result<HostState, HostError> {
    let (service, window) = caller(&webview, "state")?;
    let app = webview.app_handle().clone();
    crate::host::blocking(move || service.state(&app, &window, owner)).await
}

/// Find text in the open view, or step through the matches of the last query.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn rightkit_pdf_find(
    webview: Webview<Wry>,
    owner: String,
    query: Option<String>,
    match_case: Option<bool>,
    whole_word: Option<bool>,
    direction: Option<i32>,
    index: Option<usize>,
    clear: Option<bool>,
) -> Result<HostFindResult, HostError> {
    let (service, window) = caller(&webview, "find")?;
    let app = webview.app_handle().clone();
    let request = FindRequest {
        query,
        match_case,
        whole_word,
        direction,
        index,
        clear,
    };
    crate::host::blocking(move || service.find(&app, &window, owner, request)).await
}

/// Show the print panel for the open view. `false` when it is locked, forbids printing, or the
/// person cancelled.
#[tauri::command]
pub async fn rightkit_pdf_print(webview: Webview<Wry>, owner: String) -> Result<bool, HostError> {
    let (service, window) = caller(&webview, "print")?;
    let app = webview.app_handle().clone();
    crate::host::blocking(move || service.print(&app, &window, owner)).await
}

/// Hide the view (a background tab) and keep it briefly for reuse.
#[tauri::command]
pub async fn rightkit_pdf_suspend(
    webview: Webview<Wry>,
    owner: String,
    path: String,
) -> Result<(), HostError> {
    let (service, window) = caller(&webview, "suspend")?;
    let app = webview.app_handle().clone();
    crate::host::blocking(move || service.suspend(&app, &window, owner, &path)).await
}

/// Close the window's views. An empty `owner` with a `path` is the app-level close after a tab is
/// removed; at least one of the two is required.
#[tauri::command]
pub async fn rightkit_pdf_close(
    webview: Webview<Wry>,
    owner: String,
    path: Option<String>,
    reason: Option<String>,
) -> Result<(), HostError> {
    let (service, window) = caller(&webview, "close")?;
    let app = webview.app_handle().clone();
    crate::host::blocking(move || service.close(&app, &window, owner, path, reason)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller_gate::CallerFacts;
    use crate::nav::NavPolicy;
    use std::sync::Mutex as StdMutex;

    /// An authority that answers one fixed path and records what it was asked.
    struct Recording {
        answer: Result<PathBuf, ShellError>,
        asked: StdMutex<Vec<(String, String)>>,
    }

    impl Recording {
        fn granting(path: &str) -> Arc<Self> {
            Arc::new(Self {
                answer: Ok(PathBuf::from(path)),
                asked: StdMutex::new(Vec::new()),
            })
        }

        fn denying() -> Arc<Self> {
            Arc::new(Self {
                answer: Err(ShellError::BrokerDenied("not granted".into())),
                asked: StdMutex::new(Vec::new()),
            })
        }
    }

    impl PdfPathAuthority for Recording {
        fn authorize_read(&self, window: &str, path: &str) -> Result<PathBuf, ShellError> {
            self.asked
                .lock()
                .unwrap()
                .push((window.to_owned(), path.to_owned()));
            self.answer.clone()
        }
    }

    fn service(authority: Arc<Recording>) -> PdfHostService {
        PdfHostService::new(Arc::new(PdfHost::unsupported()), authority)
    }

    fn services(trust: ServiceTrust) -> HostServices {
        HostServices::builder()
            .pdf(trust, service(Recording::granting("/granted.pdf")))
            .build()
            .unwrap()
    }

    /// An absolute path on every platform (a leading slash is not absolute on Windows).
    fn real_path() -> String {
        std::env::temp_dir()
            .join("rightkit-pdf-host-real.pdf")
            .to_str()
            .unwrap()
            .to_owned()
    }

    fn frame() -> HostFrame {
        HostFrame {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        }
    }

    fn url(s: &str) -> tauri::Url {
        tauri::Url::parse(s).unwrap()
    }

    fn facts<'a>(
        label: &'a str,
        is_app_window: bool,
        content: &'a [String],
        url: Option<&'a tauri::Url>,
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

    fn refusal(result: Result<Arc<PdfHostService>, HostError>) -> HostError {
        match result {
            Ok(_) => panic!("the caller must be refused"),
            Err(error) => error,
        }
    }

    #[test]
    fn a_trusted_app_window_reaches_the_service() {
        let services = services(ServiceTrust::for_labels(["main", "reader-*"]));
        let nav = NavPolicy::default();
        let page = url("tauri://localhost/");
        for label in ["main", "reader-2"] {
            let caller = facts(label, true, &[], Some(&page), &nav);
            for action in [
                "open", "update", "state", "find", "print", "suspend", "close",
            ] {
                assert!(
                    authorize(&services, action, &caller).is_ok(),
                    "{label} {action}"
                );
            }
        }
    }

    #[test]
    fn a_content_webview_is_refused_before_any_service_rule() {
        // The registry of content webviews wins even for a label the service trusts.
        let services = services(ServiceTrust::for_labels(["main", "html-content"]));
        let nav = NavPolicy::default();
        let page = url("tauri://localhost/");
        let content = vec!["html-content".to_owned(), "doc-*".to_owned()];
        for label in ["html-content", "doc-7"] {
            let caller = facts(label, true, &content, Some(&page), &nav);
            let error = refusal(authorize(&services, "open", &caller));
            assert_eq!(error.code, "permission_denied", "{label}");
            assert!(
                error.message.contains("content webview"),
                "{}",
                error.message
            );
        }
    }

    #[test]
    fn a_page_that_is_not_the_apps_own_is_refused() {
        let services = services(ServiceTrust::for_labels(["main"]));
        let nav = NavPolicy::default();
        for page in [
            "https://evil.example/",
            "about:blank",
            "data:text/html,<p>x</p>",
            "blob:https://x/1",
            "http://tauri.localhost.evil.test/",
        ] {
            let page = url(page);
            let caller = facts("main", true, &[], Some(&page), &nav);
            assert_eq!(
                refusal(authorize(&services, "state", &caller)).code,
                "permission_denied",
                "{page}"
            );
        }
        // An unreadable URL fails closed.
        let caller = facts("main", true, &[], None, &nav);
        assert!(authorize(&services, "state", &caller).is_err());
    }

    #[test]
    fn a_child_webview_and_an_untrusted_window_are_refused() {
        let services = services(ServiceTrust::for_labels(["main"]));
        let nav = NavPolicy::default();
        let page = url("tauri://localhost/");
        // A webview that is not a top-level app window.
        let child = facts("main-child", false, &[], Some(&page), &nav);
        assert!(authorize(&services, "open", &child).is_err());
        // An app window the service does not trust.
        let other = facts("settings", true, &[], Some(&page), &nav);
        let error = refusal(authorize(&services, "open", &other));
        assert_eq!(error.code, "permission_denied");
        assert!(error.message.contains("not trusted"), "{}", error.message);
    }

    #[test]
    fn trust_can_enable_only_some_actions() {
        let services = services(ServiceTrust::for_labels(["main"]).with_actions(["state", "find"]));
        let nav = NavPolicy::default();
        let page = url("tauri://localhost/");
        let caller = facts("main", true, &[], Some(&page), &nav);
        assert!(authorize(&services, "state", &caller).is_ok());
        assert!(authorize(&services, "find", &caller).is_ok());
        for action in ["open", "update", "print", "suspend", "close"] {
            let error = refusal(authorize(&services, action, &caller));
            assert!(
                error.message.contains("not enabled"),
                "{action}: {}",
                error.message
            );
        }
    }

    #[test]
    fn origins_in_the_trust_narrow_the_caller_further() {
        let services =
            services(ServiceTrust::for_labels(["main"]).with_origins(["http://tauri.localhost"]));
        let nav = NavPolicy::default();
        let right = url("http://tauri.localhost/index.html");
        let wrong = url("tauri://localhost/index.html");
        assert!(authorize(
            &services,
            "open",
            &facts("main", true, &[], Some(&right), &nav)
        )
        .is_ok());
        assert!(authorize(
            &services,
            "open",
            &facts("main", true, &[], Some(&wrong), &nav)
        )
        .is_err());
    }

    #[test]
    fn an_app_that_registered_no_pdf_service_refuses_every_caller() {
        let services = HostServices::builder()
            .logs(ServiceTrust::for_labels(["main"]))
            .build()
            .unwrap();
        let nav = NavPolicy::default();
        let page = url("tauri://localhost/");
        let caller = facts("main", true, &[], Some(&page), &nav);
        let error = refusal(authorize(&services, "open", &caller));
        assert_eq!(error.code, "permission_denied");
        assert!(
            error.message.contains("not registered"),
            "{}",
            error.message
        );
    }

    #[test]
    fn the_only_path_that_reaches_the_host_is_the_authoritys_answer() {
        let real = real_path();
        let authority = Recording::granting(&real);
        let service = service(authority.clone());
        let request = service
            .open_request(
                "reader-1",
                "tab-1".into(),
                "../../etc/passwd",
                frame(),
                None,
                None,
                Some("secret".into()),
            )
            .unwrap();
        assert_eq!(request.path.as_str(), real);
        assert_eq!(request.owner, "tab-1");
        // The authority was asked about the renderer's string, for the caller's own window.
        assert_eq!(
            authority.asked.lock().unwrap().as_slice(),
            &[("reader-1".to_owned(), "../../etc/passwd".to_owned())]
        );
        // The password rides along without being printable.
        assert!(!format!("{request:?}").contains("secret"));
    }

    #[test]
    fn a_path_the_authority_refuses_never_becomes_a_request() {
        let service = service(Recording::denying());
        let error = service
            .open_request(
                "main",
                "tab".into(),
                "/not/granted.pdf",
                frame(),
                None,
                None,
                None,
            )
            .unwrap_err();
        assert_eq!(error.code, "broker_denied");
    }

    #[test]
    fn an_authority_answer_that_is_not_an_absolute_path_is_refused() {
        let service = service(Recording::granting("relative.pdf"));
        let error = service
            .open_request(
                "main",
                "tab".into(),
                "relative.pdf",
                frame(),
                None,
                None,
                None,
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid");
    }

    #[test]
    fn closing_compares_the_authorized_form_and_falls_back_to_the_given_one() {
        let granted = service(Recording::granting("/granted/real.pdf"));
        assert_eq!(
            granted.stored_path("main", "alias.pdf"),
            "/granted/real.pdf"
        );
        let revoked = service(Recording::denying());
        assert_eq!(
            revoked.stored_path("main", "/was/here.pdf"),
            "/was/here.pdf"
        );
    }

    #[test]
    fn an_unsupported_platform_answers_a_typed_error_first() {
        let service = service(Recording::granting("/granted.pdf"));
        let error = service.require_supported().unwrap_err();
        assert_eq!(error.code, "unsupported");
        assert!(error.message.contains("not available on this platform"));
    }

    #[test]
    fn pdf_errors_keep_their_codes_and_carry_no_path() {
        for (error, code) in [
            (PdfError::Unsupported, "unsupported"),
            (PdfError::NotOpen, "not_open"),
            (PdfError::StaleOwner, "stale_owner"),
            (PdfError::Invalid("x".into()), "invalid"),
            (PdfError::File("the file does not exist".into()), "file"),
            (
                PdfError::Load("PDFKit could not load the file".into()),
                "load_failed",
            ),
            (PdfError::NotMainThread, "not_main_thread"),
            (PdfError::Native("n".into()), "native"),
        ] {
            let mapped = from_pdf_error(error);
            assert_eq!(mapped.code, code);
            assert!(!mapped.message.contains('/'), "{}", mapped.message);
        }
    }

    #[test]
    fn the_wire_names_are_stable() {
        assert_eq!(PDF_SERVICE.name(), "pdf");
        assert!(EVENT_STATE.starts_with("rightkit://pdf-"));
        assert!(EVENT_RESOURCE.starts_with("rightkit://pdf-"));
        assert!(EVENT_KEY.starts_with("rightkit://pdf-"));
    }

    #[test]
    fn the_service_can_be_registered_once() {
        let twice = HostServices::builder()
            .pdf(
                ServiceTrust::for_labels(["main"]),
                service(Recording::granting("/a.pdf")),
            )
            .pdf(
                ServiceTrust::for_labels(["main"]),
                service(Recording::granting("/b.pdf")),
            )
            .build();
        assert!(twice.is_err());
        // A trust that would admit every window does not build either.
        let all = HostServices::builder()
            .pdf(
                ServiceTrust::for_labels(["*"]),
                service(Recording::granting("/a.pdf")),
            )
            .build();
        assert!(all.is_err());
    }
}
