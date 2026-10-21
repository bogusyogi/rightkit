//! In-app control server (Tauri plugin, feature `control`).
//!
//! QA ONLY, NEVER SHIPPED. This module is compiled only with the explicit `control` cargo feature.
//! A binary that contains it carries [`QA_CONTROL_MARKER`] and `right-release` refuses to seal or upload
//! a release or update build that does; a compiled-in server still serves nothing unless the launcher
//! sets `RIGHTKIT_CONTROL_SERVICE`, and then logs one loud line to stderr.
//!
//! Started only when the launcher sets `RIGHTKIT_CONTROL_SERVICE=<name>`
//! (a lowercase service name such as `control-genright`); normal user launches
//! never set it, so nothing listens. The server is a `rightkit-daemon` host:
//! discovery file `<suite>/runtime/<name>/service.json`, a current-user-only
//! Unix socket (macOS) or named pipe (Windows), and a per-app request
//! allowlist. There is no TCP listener and no bearer token. The suite root is
//! resolved by `Suite::resolve` (honours `RIGHTKIT_SUITE_ROOT` for isolated
//! runs) and the server **fails closed**: if no absolute root exists the plugin
//! does not start, it never falls back to the working directory.
//!
//! Input is delivered inside the process straight to the webview: macOS
//! dispatches synthesized `NSEvent`s to the `WKWebView` (no window activation,
//! no first-mouse swallow), Windows drives WebView2 through
//! `CallDevToolsProtocolMethod` `Input.dispatch*`.
//!
//! Methods (x/y are viewport CSS pixels, origin top-left of the webview); the
//! params are the JSON bodies of the former `/rk/*` routes:
//!   health  click{x,y,button?,count?,modifiers?}  move{x,y,modifiers?}
//!   drag{x1,y1,x2,y2,steps?,button?,modifiers?}
//!   pointer{phase:"down"|"drag"|"up"|"move",x,y,button?,modifiers?}
//!   wheel{x,y,dx,dy}  key{key:"cmd+k"}  type{text}  eval{js}  dom{selector}
//!   wait{js,timeoutMs?,intervalMs?} -> {matched,value|last,error?,elapsedMs}: one round trip that waits
//!   in the page (MutationObserver, events, a short timer, rAF when visible) until the function body `js`
//!   returns something truthy or `timeoutMs` (default 10000) passes. The predicate text is inlined into
//!   the evaluated script, never `new Function`, so page CSP cannot block it.
//!   batch{steps:[{method,params}]} -> {completed,results,stepMs} or {ok:false,failedStep,error,results}:
//!   N steps in one round trip, stopped at the first failure; every step is checked against the caller's
//!   allowlist and passes the effect gate / input lease on its own; `batch` cannot nest.
//!   Pacing: click, drag, key and type send their native events back to back. The optional `paceMs`
//!   (default 0, at most 5000) restores a gap between events; the former fixed pauses were about
//!   click 40-50, drag 16, key 20, type 8. `type` dispatches real key down/up events per character.
//!   set_viewport{width,height,settleMs?} -> {innerWidth,innerHeight,requestedWidth,requestedHeight,
//!   settled,clamped,placementTimedOut,reason?} (CSS pixels; no activation). innerWidth/innerHeight
//!   are READ BACK after two consecutive non-empty readings agree, 500 ms apart (bound
//!   `settleMs`, default 2000 or `Control::viewport_settle`). A single target match is insufficient.
//!   `ok:false` reports unsettled readback or placement timeout. `clamped` means confirmed OS limits.
//!   Hidden macOS windows retain their screen-constraint exemption through deferred layout; mismatches
//!   are corrected within this request's bound. Showing them restores normal constraints. No activation.
//!   ax  screenshot{mask?:[selector],inline?:bool} -> {path,bytes,masked} (PNG in `<service dir>/shots/`,
//!   owner-only) or, with `inline:true`, {bytes,masked,encoding:"base64",data} and no file;
//!   password/one-time-code/data-rk-secret elements and `mask` selectors are
//!   hidden during capture so secrets never reach the image (macOS hides, captures and restores in one
//!   main-thread operation)
//!   command{name,args}  typed app commands registered with `Control::command`
//!   lease_acquire{reconcile?:"delivered"|"lost"}  lease_release{leaseEpoch}
//!   lease_reconcile{leaseEpoch,decision}  lease_status
//!
//! EFF-001: every effectful method (input, eval, screenshot, command) passes
//! the plugin's [`EffectGate`] (`Control::gate`; default allow-all) before any
//! side effect and is settled with lifecycle events. PTY-002: when
//! `Control::input_lease` is set, input methods additionally require
//! `leaseEpoch` + `inputSequence` and are fenced/sequenced by the lease before
//! they reach the admission hook.

use rightkit_automation::control::admission::{
    CancelToken, EffectGate, EffectKind, EffectRequest, ExecError, SettleOutcome,
};
use rightkit_automation::control::lease::{
    InputError, InputLease, InputOutcome, UnacknowledgedInputDecision,
};
use rightkit_automation::json::esc;
use rightkit_automation::keys::appkit as keys;
use rightkit_daemon::{
    host, Allowlist, CallContext, Emit, Handler, ServiceConfig, ServiceError, Suite,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Listener, Manager, Runtime, WebviewWindow};

#[cfg(target_os = "macos")]
#[path = "mac_view.rs"]
mod native;
#[cfg(windows)]
#[path = "win_view.rs"]
mod native;
mod viewport;
pub use viewport::{
    set_viewport, PlacementGate, ViewportCfg, ViewportOutcome, ViewportWindow, PLACEMENT_EVENT,
};

#[cfg(not(any(target_os = "macos", windows)))]
mod native {
    use super::{Button, Mouse};
    use tauri::{Runtime, WebviewWindow};
    const NO: &str = "unsupported platform";
    pub fn mouse<R: Runtime>(
        _: &WebviewWindow<R>,
        _: Mouse,
        _: Button,
        _: f64,
        _: f64,
        _: i64,
        _: u64,
    ) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn focus_window<R: Runtime>(_: &WebviewWindow<R>) -> Result<String, String> {
        Err(NO.into())
    }
    pub fn mouse_batch<R: Runtime>(
        _: &WebviewWindow<R>,
        _: &[super::MouseEv],
    ) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn keys<R: Runtime>(_: &WebviewWindow<R>, _: &[super::KeyEv]) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn wait_slice<R: Runtime>(
        _: &WebviewWindow<R>,
        _: &str,
        _: std::time::Duration,
    ) -> Result<String, String> {
        Err(NO.into())
    }
    pub fn screenshot_masked<R: Runtime>(
        _: &WebviewWindow<R>,
        _: &str,
        _: &str,
    ) -> Result<(Vec<u8>, u64), String> {
        Err(NO.into())
    }
    pub fn wheel<R: Runtime>(
        _: &WebviewWindow<R>,
        _: f64,
        _: f64,
        _: f64,
        _: f64,
    ) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn eval<R: Runtime>(_: &WebviewWindow<R>, _: &str) -> Result<String, String> {
        Err(NO.into())
    }
    pub fn apply_viewport<R: Runtime>(_: &WebviewWindow<R>, _: u32, _: u32) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn read_viewport<R: Runtime>(
        _: &WebviewWindow<R>,
        _: std::time::Duration,
    ) -> Result<(f64, f64), String> {
        Err(NO.into())
    }
}
/// One native keyboard step. Windows inserts characters with no virtual key through
/// `Input.insertText` (`Text`); macOS only produces `Key`.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyEv {
    Key {
        down: bool,
        code: u16,
        chars: String,
        flags: u64,
    },
    #[cfg_attr(not(windows), allow(dead_code))]
    Text(String),
}

/// One native pointer step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseEv {
    pub kind: Mouse,
    pub button: Button,
    pub x: f64,
    pub y: f64,
    pub clicks: i64,
    pub flags: u64,
}

/// The key down/up pair (or one text insertion) that types `c`.
fn type_events(c: char) -> Vec<KeyEv> {
    let s = c.to_string();
    #[cfg(windows)]
    if !c.is_ascii() {
        return vec![KeyEv::Text(s)];
    }
    let code = keys::chord(&s.to_lowercase())
        .map(|(k, _)| k)
        .unwrap_or(0xFF);
    let flags = if c.is_uppercase() {
        keys::FLAG_SHIFT
    } else {
        0
    };
    vec![
        KeyEv::Key {
            down: true,
            code,
            chars: s.clone(),
            flags,
        },
        KeyEv::Key {
            down: false,
            code,
            chars: s,
            flags,
        },
    ]
}

/// Dispatch pointer events back to back (one native hop), or with `pace` ms between them.
fn mouse_seq<R: Runtime>(win: &WebviewWindow<R>, evs: &[MouseEv], pace: u64) -> Result<(), String> {
    if pace == 0 {
        return native::mouse_batch(win, evs);
    }
    for (i, e) in evs.iter().enumerate() {
        if i > 0 {
            pause(pace);
        }
        mouse(win, e.kind, e.button, e.x, e.y, e.clicks, e.flags)?;
    }
    Ok(())
}

/// Dispatch key events back to back (one native hop), or `pace` ms apart.
fn key_seq<R: Runtime>(win: &WebviewWindow<R>, evs: &[KeyEv], pace: u64) -> Result<(), String> {
    if pace == 0 {
        return native::keys(win, evs);
    }
    for (i, e) in evs.iter().enumerate() {
        if i > 0 {
            pause(pace);
        }
        native::keys(win, std::slice::from_ref(e))?;
    }
    Ok(())
}

/// Run `dispatch` (native events sent back to back) and return only once the page has handled
/// them. WKWebView queues pointer and key events and handles each in a web-process round trip, so
/// when they have all been dispatched they are not yet applied: a read right after would see the
/// old page. On macOS this installs a counting capture listener for `event` (the DOM event the LAST
/// native event produces), dispatches, and waits in the page until `expected` were counted; the queue
/// is FIFO, so that proves the earlier ones were handled. The wait is bounded and silent when the
/// page never counts them. Elsewhere the native call itself completes when the event is handled.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn settled<R: Runtime>(
    win: &WebviewWindow<R>,
    event: &str,
    expected: usize,
    dispatch: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    // A pointer move to where the pointer already is may produce no DOM event: do not wait long for it.
    let base_bound = if event == "mousemove" { 300 } else { 2_000 };
    #[cfg(target_os = "macos")]
    {
        eval(
            win,
            &format!("(function(){{window.__rkEvN=0;if(window.__rkEvH)removeEventListener(window.__rkEvT,window.__rkEvH,true);window.__rkEvT='{event}';window.__rkEvH=function(){{window.__rkEvN++}};addEventListener('{event}',window.__rkEvH,true);return '1'}})()"),
        )?;
        let sent = dispatch();
        if sent.is_ok() {
            let bound = base_bound + 20 * expected as u64;
            let _ = wait_route(
                win,
                &format!("return window.__rkEvN>={expected};"),
                bound,
                5,
            );
        }
        let _ = eval(
            win,
            "(function(){if(window.__rkEvH){removeEventListener(window.__rkEvT,window.__rkEvH,true);window.__rkEvH=null}return '1'})()",
        );
        sent
    }
    #[cfg(not(target_os = "macos"))]
    dispatch()
}

/// Pointer events settled (see [`settled`]); with `pace` they are sent one at a time, spaced.
fn mouse_settled<R: Runtime>(
    win: &WebviewWindow<R>,
    evs: &[MouseEv],
    pace: u64,
) -> Result<(), String> {
    let Some(last) = evs.last() else {
        return Ok(());
    };
    if pace > 0 {
        return mouse_seq(win, evs, pace);
    }
    let event = match last.kind {
        Mouse::Down => "mousedown",
        Mouse::Up => "mouseup",
        Mouse::Move | Mouse::Drag => "mousemove",
    };
    let expected = evs
        .iter()
        .filter(|e| {
            e.kind == last.kind
                || (event == "mousemove" && matches!(e.kind, Mouse::Move | Mouse::Drag))
        })
        .count();
    settled(win, event, expected, || mouse_seq(win, evs, 0))
}

/// Dispatch key events and return only once the page has handled them. With `pace` the events are
/// sent one at a time with that gap (the former behaviour); otherwise back to back in chunks, then
/// settled on one `keyup` per key sent (see [`settled`]).
fn keys_settled<R: Runtime>(
    win: &WebviewWindow<R>,
    evs: &[KeyEv],
    pace: u64,
) -> Result<(), String> {
    if pace > 0 {
        return key_seq(win, evs, pace);
    }
    let ups = evs
        .iter()
        .filter(|e| matches!(e, KeyEv::Key { down: false, .. }))
        .count();
    settled(win, "keyup", ups, || {
        for chunk in evs.chunks(TYPE_CHUNK_EVENTS) {
            key_seq(win, chunk, 0)?;
        }
        Ok(())
    })
}

/// Optional per-request gap between native events; default 0.
fn pace_ms(params: &Value) -> Result<u64, String> {
    match params.get("paceMs") {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= MAX_PACE_MS)
            .ok_or_else(|| format!("'paceMs' must be an integer in 0..={MAX_PACE_MS}")),
    }
}
const MAX_PACE_MS: u64 = 5000;

use native::{apply_viewport, eval, mouse, read_viewport, wheel};

/// The real window behind the viewport core.
struct NativeWindow<'a, R: Runtime>(&'a WebviewWindow<R>);
impl<R: Runtime> ViewportWindow for NativeWindow<'_, R> {
    fn apply(&self, width: u32, height: u32) -> Result<(), String> {
        apply_viewport(self.0, width, height)
    }
    fn read(&self, timeout: Duration) -> Result<(f64, f64), String> {
        read_viewport(self.0, timeout)
    }
    fn requires_exact_size(&self) -> Result<bool, String> {
        #[cfg(target_os = "macos")]
        {
            native::requires_exact_size(self.0)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(false)
        }
    }
}

/// What `set_viewport` needs besides the window.
#[derive(Clone)]
struct ViewportCtx {
    placement: Arc<PlacementGate>,
    cfg: ViewportCfg,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mouse {
    Down,
    Up,
    Move,
    Drag,
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    Left,
    Right,
}

type CommandFn<R> = Box<dyn Fn(&AppHandle<R>, &str) -> Result<String, String> + Send + Sync>;

/// Elements never captured in a screenshot: password and one-time-code inputs,
/// credential autocomplete hints, and anything the app marks `data-rk-secret`.
pub const SECRET_SELECTORS: &str = "input[type=password],input[autocomplete*=password],input[autocomplete=one-time-code],[data-rk-secret]";
static MASKED_LAST: AtomicU64 = AtomicU64::new(0);

/// Bytes that appear in every binary that compiles this module. `right-release` scans Mach-O and PE files
/// for exactly this string and refuses to seal or upload a release or update tier build that has it.
/// Keep it identical to `QA_CONTROL_MARKER` in `packages/release/qa-control-scan.mjs`.
pub const QA_CONTROL_MARKER: &str = "RIGHTKIT-QA-CONTROL-SERVER-PRESENT";

/// The marker as a retained static. `#[used]` keeps the object in the compiler output and (via
/// `llvm.used`) out of ld64 `-dead_strip` and link.exe `/OPT:REF`; `keep_marker()` below also READS it from
/// `Control::build`, so the linker, LTO and the optimiser must keep it for as long as the server code that
/// could serve a request is kept. No `link_section` is used: it would only add platform-specific naming.
#[used]
static QA_CONTROL_MARKER_BYTES: [u8; QA_CONTROL_MARKER.len()] = {
    let src = QA_CONTROL_MARKER.as_bytes();
    let mut out = [0u8; QA_CONTROL_MARKER.len()];
    let mut i = 0;
    while i < src.len() {
        out[i] = src[i];
        i += 1;
    }
    out
};

/// A volatile read of the marker's first byte: makes the static reachable from live server code so that
/// no optimisation level or LTO pass can treat it as dead.
#[inline(never)]
fn keep_marker() -> u8 {
    // SAFETY: the pointer is the start of a live, initialised static array of non-zero length.
    unsafe { std::ptr::read_volatile(QA_CONTROL_MARKER_BYTES.as_ptr()) }
}

/// Launcher-set environment variable that names the service and enables control.
pub const ENV_SERVICE: &str = "RIGHTKIT_CONTROL_SERVICE";

/// Every method the server understands (the default allowlist for QA/CLI apps).
pub const METHODS: &[&str] = &[
    "health",
    "click",
    "move",
    "drag",
    "pointer",
    "wheel",
    "key",
    "type",
    "eval",
    "dom",
    "ax",
    "screenshot",
    "set_viewport",
    "focus",
    "command",
    "wait",
    "batch",
    "lease_acquire",
    "lease_release",
    "lease_reconcile",
    "lease_status",
];

/// Effect kind of a control method; `None` for reads and lease management.
pub fn effect_kind(method: &str) -> Option<EffectKind> {
    match method {
        "click" | "move" | "drag" | "pointer" | "wheel" | "key" | "type" => Some(EffectKind::Input),
        "eval" | "wait" => Some(EffectKind::Script),
        "screenshot" => Some(EffectKind::Capture),
        "command" | "set_viewport" | "focus" => Some(EffectKind::Command),
        _ => None,
    }
}

/// Builder for the control plugin. `command` registers typed app verbs that
/// agents call through the `command` method (the closure gets the raw JSON
/// params and returns a JSON value or an error string).
pub struct Control<R: Runtime> {
    label: String,
    commands: HashMap<String, CommandFn<R>>,
    allow: Allowlist,
    gate: Arc<EffectGate>,
    lease: Option<Arc<InputLease>>,
    viewport: ViewportCfg,
}

impl<R: Runtime> Default for Control<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Runtime> Control<R> {
    /// Default allowlist: the QA harness (`right-qa`) and the CLI (`right-ctl`)
    /// may call every method. Other apps get nothing until `allow_app`.
    pub fn new() -> Self {
        let allow = Allowlist::new()
            .allow("right-qa", METHODS.iter().copied())
            .allow("right-ctl", METHODS.iter().copied());
        Self {
            label: "main".into(),
            commands: HashMap::new(),
            allow,
            gate: Arc::new(EffectGate::allow_all()),
            lease: None,
            viewport: ViewportCfg::default(),
        }
    }
    /// Default bound (2 s) for `set_viewport` to read back a settled size; a request can
    /// override it with `settleMs`.
    pub fn viewport_settle(mut self, bound: Duration) -> Self {
        self.viewport.settle = bound;
        self
    }
    /// Bound (default 5 s) for `set_viewport` to wait for the shell's initial window
    /// placement before it applies the request.
    pub fn viewport_placement_wait(mut self, bound: Duration) -> Self {
        self.viewport.placement_wait = bound;
        self
    }
    /// EFF-001 admission authority for every effectful method (default
    /// [`EffectGate::allow_all`]). Install hook and event sink on the gate.
    pub fn gate(mut self, gate: Arc<EffectGate>) -> Self {
        self.gate = gate;
        self
    }
    /// PTY-002: require `leaseEpoch`/`inputSequence` on input methods.
    pub fn input_lease(mut self, lease: Arc<InputLease>) -> Self {
        self.lease = Some(lease);
        self
    }
    pub fn window(mut self, label: &str) -> Self {
        self.label = label.into();
        self
    }
    pub fn command(
        mut self,
        name: &str,
        f: impl Fn(&AppHandle<R>, &str) -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        self.commands.insert(name.into(), Box::new(f));
        self
    }
    /// Let another app call only `methods` (per-app request allowlist).
    pub fn allow_app<I: IntoIterator<Item = S>, S: Into<String>>(
        mut self,
        app: &str,
        methods: I,
    ) -> Self {
        self.allow = self.allow.allow(app, methods);
        self
    }
    /// True only when the launcher asked for control.
    pub fn enabled() -> bool {
        service_from_env().is_some()
    }
    /// `Some(plugin)` when enabled by env, else `None` (normal launches).
    pub fn build_if_enabled(self) -> Option<TauriPlugin<R>> {
        Self::enabled().then(|| self.build())
    }
    pub fn build(self) -> TauriPlugin<R> {
        // Retain the marker with the server: see `keep_marker`.
        std::hint::black_box(keep_marker());
        let Control {
            label,
            commands,
            allow,
            gate,
            lease,
            viewport,
        } = self;
        let commands = Arc::new(commands);
        // Listen from plugin setup: the shell announces placement when the webview is
        // ready, which is after every plugin's setup.
        let placement = Arc::new(PlacementGate::default());
        let vp = ViewportCtx {
            placement: placement.clone(),
            cfg: viewport,
        };
        let builder = tauri::plugin::Builder::new("rightkit-control");
        // RIGHTKIT_CONTROL_TRACE: log WebView2 events from webview creation on (Windows only).
        #[cfg(windows)]
        let builder = builder.on_webview_ready(|webview| native::install_trace(&webview));
        // Hidden launches must never activate the app: re-assert the policy as each webview is
        // ready (an app's own setup may have reset it after plugin setup).
        #[cfg(target_os = "macos")]
        let builder = builder.on_webview_ready(|webview| {
            enforce_hidden_activation(webview.app_handle());
        });
        // QA IPC fixture seam: only while the runtime QA control service is active (this whole
        // plugin exists only in a `control` build, which release builds never carry).
        let builder = match ipc_fixture_script(service_from_env().as_deref()) {
            Some(script) => builder.js_init_script(script),
            None => builder,
        };
        builder
            .setup(move |app, _| {
                // Before any window exists (plugin setup precedes the app's own setup).
                enforce_hidden_activation(app);
                if let Some(service) = service_from_env() {
                    // One loud line, always, before the socket exists: a QA control server must never
                    // run unnoticed. stderr, not the app log, so it cannot be filtered or redirected away.
                    eprintln!(
                        "RIGHTKIT QA CONTROL SERVER ACTIVE: service={service} (control build, {ENV_SERVICE} set); \
                         this process accepts same-user input, DOM/eval and screenshot control. Never ship this build."
                    );
                    let gate_events = placement.clone();
                    app.listen(PLACEMENT_EVENT, move |event| {
                        gate_events.on_event(event.payload());
                    });
                    serve(
                        app.clone(),
                        label.clone(),
                        service,
                        commands.clone(),
                        allow.clone(),
                        Guards {
                            gate: gate.clone(),
                            lease: lease.clone(),
                        },
                        vp.clone(),
                    )?;
                }
                Ok(())
            })
            .build()
    }
}

/// Env flag the QA launcher sets for a hidden launch (`rightkit-qa` `Mode::Hidden`).
#[cfg(any(target_os = "macos", test))]
const ENV_HIDDEN: &str = "RIGHTKIT_QA_HIDDEN";

/// Pure decision: a hidden QA launch (`RIGHTKIT_QA_HIDDEN=1`) must run non-activating. Visible and
/// background launches are left to the app.
#[cfg(any(target_os = "macos", test))]
fn hidden_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

/// macOS: during a hidden QA launch make the app an Accessory (never frontmost, no Dock icon,
/// no menu bar) so a later `show()`, reopen or AppKit launch activation cannot bring it to the
/// front. Idempotent; a no-op for every other launch and on other platforms. This cannot undo an
/// activation the app already requested before plugin setup (tao's
/// `activate_ignoring_other_apps`, which an app must switch off in hidden runs).
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn enforce_hidden_activation<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(target_os = "macos")]
    if hidden_requested(std::env::var_os(ENV_HIDDEN).as_deref()) {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
}

/// QA IPC fixture page script (`window.__RK_QA_IPC__`, a `window.fetch` wrapper; see `ipc_fixture.js`).
const IPC_FIXTURE_JS: &str = include_str!("ipc_fixture.js");

/// The fixture init script, only when the runtime QA control service is active (`service` is the
/// `RIGHTKIT_CONTROL_SERVICE` value). `None` when unset or blank, so a normal launch of a control
/// build injects nothing.
fn ipc_fixture_script(service: Option<&str>) -> Option<&'static str> {
    service
        .filter(|s| !s.trim().is_empty())
        .map(|_| IPC_FIXTURE_JS)
}

fn service_from_env() -> Option<String> {
    std::env::var(ENV_SERVICE)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

struct ControlHandler<R: Runtime> {
    app: AppHandle<R>,
    label: String,
    commands: Arc<HashMap<String, CommandFn<R>>>,
    shots: std::path::PathBuf,
    shot_seq: AtomicU64,
    guards: Guards,
    viewport: ViewportCtx,
    allow: Allowlist,
}

#[derive(Clone)]
struct Guards {
    gate: Arc<EffectGate>,
    lease: Option<Arc<InputLease>>,
}

fn serve<R: Runtime>(
    app: AppHandle<R>,
    label: String,
    service: String,
    commands: Arc<HashMap<String, CommandFn<R>>>,
    allow: Allowlist,
    guards: Guards,
    viewport: ViewportCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    // Fail closed: no absolute suite root means no server, never a cwd-relative one.
    let suite = Suite::resolve()?;
    let shots = suite.private_service_subdir(&service, "shots")?;
    let cfg = ServiceConfig::new(
        &service,
        &format!("control:{label}"),
        0,
        env!("CARGO_PKG_VERSION"),
        allow.clone(),
    );
    let handler = Arc::new(ControlHandler {
        app,
        label,
        commands,
        shots,
        shot_seq: AtomicU64::new(0),
        guards,
        viewport,
        allow,
    });
    let hosted = host(&suite, cfg, handler)?;
    // Hosted releases on drop; the plugin keeps it for the life of the process.
    std::mem::forget(hosted);
    Ok(())
}

impl<R: Runtime> Handler for ControlHandler<R> {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        emit: &Emit,
    ) -> Result<Value, ServiceError> {
        if method == "health" {
            return Ok(json!({"ok": true, "service": "rightkit-control"}));
        }
        if method == "batch" {
            return self.batch(cx, params, emit);
        }
        if let Some(r) = self.lease_method(method, params) {
            return r;
        }
        if method == "set_viewport" {
            viewport_size(params).map_err(|e| ServiceError::new("bad_request", e))?;
            #[cfg(not(any(target_os = "macos", windows)))]
            return Err(ServiceError::new(
                "unsupported",
                "native viewport resizing unavailable on this platform",
            ));
        }
        // Precondition (no side effect), kept retryable as before the gate.
        if self.app.get_webview_window(&self.label).is_none() {
            return Err(ServiceError::retryable(
                "window_not_found",
                "webview window not created yet",
            ));
        }
        let Some(kind) = effect_kind(method) else {
            return self.execute(method, params);
        };
        let request = EffectRequest::new(kind, method, &self.label).with_params(params.to_string());
        let cancel = CancelToken::new();
        let exec = |_: &CancelToken| {
            self.execute(method, params).map_err(|e| {
                ExecError::Failed(if e.code == "control_error" {
                    e.message
                } else {
                    format!("{}: {}", e.code, e.message)
                })
            })
        };
        if let (EffectKind::Input, Some(lease)) = (kind, &self.guards.lease) {
            let epoch = u64_param(params, "leaseEpoch")?;
            let seq = u64_param(params, "inputSequence")?;
            let session = rightkit_automation::control::lease::ControlSession::new(
                lease.clone(),
                self.guards.gate.clone(),
            );
            return match session.input(epoch, seq, request, &cancel, exec) {
                Ok(InputOutcome::Applied { ack, value }) => {
                    Ok(with_ack(value, ack.epoch, ack.sequence, false))
                }
                Ok(InputOutcome::Duplicate { ack }) => {
                    Ok(with_ack(json!({"ok": true}), ack.epoch, ack.sequence, true))
                }
                Err(InputError::Lease(e)) => Err(ServiceError::new(
                    format!("lease_{}", e.label()),
                    e.to_string(),
                )),
                Err(InputError::NotDelivered { reason }) => Err(ServiceError::new(
                    if reason.starts_with("denied") {
                        "denied"
                    } else {
                        "not_delivered"
                    },
                    reason,
                )),
                Err(e @ InputError::Ambiguous { .. }) => {
                    Err(ServiceError::new("input_unacknowledged", e.to_string()))
                }
            };
        }
        let effect = self.guards.gate.admit(request, &cancel, exec);
        let st = &effect.settlement;
        match st.outcome {
            SettleOutcome::Ok => Ok(effect.value.unwrap_or(Value::Null)),
            SettleOutcome::Denied => Err(ServiceError::new(
                "denied",
                st.reason.clone().unwrap_or_default(),
            )),
            SettleOutcome::Cancelled => Err(ServiceError::new(
                "cancelled",
                st.reason.clone().unwrap_or_default(),
            )),
            SettleOutcome::Failed => Err(ServiceError::new(
                "control_error",
                st.reason.clone().unwrap_or_default(),
            )),
        }
    }
}

/// Most steps one `batch` may carry.
pub const MAX_BATCH_STEPS: usize = 256;

fn u64_param(params: &Value, key: &str) -> Result<u64, ServiceError> {
    params.get(key).and_then(Value::as_u64).ok_or_else(|| {
        ServiceError::new("lease_required", format!("input requires integer '{key}'"))
    })
}

fn with_ack(mut value: Value, epoch: u64, sequence: u64, duplicate: bool) -> Value {
    if let Value::Object(map) = &mut value {
        map.insert("leaseEpoch".into(), json!(epoch));
        map.insert("ackedInputSequence".into(), json!(sequence));
        map.insert("duplicate".into(), json!(duplicate));
    }
    value
}

fn snapshot_json(s: rightkit_automation::control::lease::LeaseSnapshot) -> Value {
    json!({
        "ok": true,
        "leaseEpoch": s.epoch,
        "attached": s.attached,
        "nextInputSequence": s.next_input_sequence,
        "ackedInputSequence": s.acked_input_sequence,
        "unacknowledgedInput": s.unacknowledged_input.map(|(a, b)| json!([a, b])),
    })
}

impl<R: Runtime> ControlHandler<R> {
    /// `batch{steps:[{method,params}]}`: run the steps in order, in one round trip, and stop at
    /// the first failure. Each step is checked against the caller's allowlist and goes through
    /// [`Handler::call`], so the effect gate and the input lease apply to it exactly as to a
    /// single request. A failed step is reported in the (successful) reply with the results so far.
    fn batch(&self, cx: &CallContext, params: &Value, emit: &Emit) -> Result<Value, ServiceError> {
        let steps = params
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| ServiceError::new("bad_request", "batch needs a 'steps' array"))?;
        if steps.len() > MAX_BATCH_STEPS {
            return Err(ServiceError::new(
                "bad_request",
                format!("batch has {} steps; at most {MAX_BATCH_STEPS}", steps.len()),
            ));
        }
        let mut plan = Vec::with_capacity(steps.len());
        for (i, step) in steps.iter().enumerate() {
            let m = step.get("method").and_then(Value::as_str).ok_or_else(|| {
                ServiceError::new("bad_request", format!("step {i}: missing 'method'"))
            })?;
            if m == "batch" {
                return Err(ServiceError::new(
                    "bad_request",
                    format!("step {i}: batch cannot nest"),
                ));
            }
            if !self.allow.permits(cx.app(), m) {
                return Err(ServiceError::new(
                    "request_not_allowed",
                    format!("step {i}: app '{}' may not call {m}", cx.app()),
                ));
            }
            let mut p = step.get("params").cloned().unwrap_or_else(|| json!({}));
            if m == "screenshot" && p.get("inline").is_none() {
                p["inline"] = json!(true);
            }
            plan.push((m.to_string(), p));
        }
        let (mut results, mut step_ms) = (
            Vec::with_capacity(plan.len()),
            Vec::with_capacity(plan.len()),
        );
        for (i, (m, p)) in plan.iter().enumerate() {
            let t = std::time::Instant::now();
            match self.call(cx, m, p, emit) {
                Ok(v) => {
                    results.push(v);
                    step_ms.push(t.elapsed().as_millis() as u64);
                }
                Err(e) => {
                    return Ok(json!({
                        "ok": false,
                        "completed": i,
                        "failedStep": i,
                        "failedMethod": m,
                        "errorCode": e.code,
                        "error": format!("step {i} ({m}): {}: {}", e.code, e.message),
                        "results": results,
                        "stepMs": step_ms,
                    }));
                }
            }
        }
        Ok(json!({"ok": true, "completed": results.len(), "results": results, "stepMs": step_ms}))
    }

    /// `lease_*` methods; `None` when `method` is not one of them.
    fn lease_method(&self, method: &str, params: &Value) -> Option<Result<Value, ServiceError>> {
        if !method.starts_with("lease_") {
            return None;
        }
        let Some(lease) = &self.guards.lease else {
            return Some(Err(ServiceError::new(
                "lease_disabled",
                "no input lease configured",
            )));
        };
        let decision = |k: &str| match params.get(k).and_then(Value::as_str) {
            Some("delivered") => Ok(Some(UnacknowledgedInputDecision::ReconcileAsDelivered)),
            Some("lost") => Ok(Some(UnacknowledgedInputDecision::ReconcileAsLost)),
            None => Ok(None),
            Some(other) => Err(ServiceError::new(
                "bad_request",
                format!("unknown decision '{other}'"),
            )),
        };
        let lease_err = |e: rightkit_automation::control::lease::LeaseError| {
            ServiceError::new(format!("lease_{}", e.label()), e.to_string())
        };
        Some((|| match method {
            "lease_status" => Ok(snapshot_json(lease.snapshot())),
            "lease_acquire" => {
                let grant = match decision("reconcile")? {
                    Some(d) => lease.try_acquire_reconciling(d).map_err(lease_err)?,
                    None => lease.acquire().map_err(lease_err)?,
                };
                Ok(
                    json!({"ok": true, "leaseEpoch": grant.epoch, "nextInputSequence": grant.next_input_sequence, "fencedEpoch": grant.fenced_epoch}),
                )
            }
            "lease_release" => lease
                .release(u64_param(params, "leaseEpoch")?)
                .map(snapshot_json)
                .map_err(lease_err),
            "lease_reconcile" => {
                let d = decision("decision")?
                    .ok_or_else(|| ServiceError::new("bad_request", "missing 'decision'"))?;
                lease
                    .reconcile(u64_param(params, "leaseEpoch")?, d)
                    .map(snapshot_json)
                    .map_err(lease_err)
            }
            _ => Err(ServiceError::new(
                "bad_request",
                format!("unknown method {method}"),
            )),
        })())
    }

    /// Execute stage: the only place control side effects happen.
    fn execute(&self, method: &str, params: &Value) -> Result<Value, ServiceError> {
        let Some(win) = self.app.get_webview_window(&self.label) else {
            return Err(ServiceError::retryable(
                "window_not_found",
                "webview window not created yet",
            ));
        };
        match route(
            method,
            params,
            &win,
            &self.app,
            &self.commands,
            &self.label,
            &self.viewport,
        ) {
            Ok(Out::Json(j)) => Ok(serde_json::from_str(&j).unwrap_or(Value::Null)),
            Ok(Out::Png(p)) if params.get("inline").and_then(Value::as_bool) == Some(true) => Ok(
                json!({"ok": true, "bytes": p.len(), "masked": MASKED_LAST.load(Ordering::SeqCst), "encoding": "base64", "data": rightkit_automation::json::base64_encode(&p)}),
            ),
            Ok(Out::Png(p)) => {
                let n = self.shot_seq.fetch_add(1, Ordering::SeqCst);
                let path = self.shots.join(format!("shot-{n}.png"));
                std::fs::write(&path, &p).map_err(|e| ServiceError::new("io", e.to_string()))?;
                Ok(
                    json!({"ok": true, "path": path.to_string_lossy(), "bytes": p.len(), "masked": MASKED_LAST.load(Ordering::SeqCst)}),
                )
            }
            Err(e) => Err(ServiceError::new("control_error", e)),
        }
    }
}

enum Out {
    Json(String),
    Png(Vec<u8>),
}
fn ok() -> Result<Out, String> {
    Ok(Out::Json(r#"{"ok":true}"#.into()))
}
fn pause(ms: u64) {
    if ms == 0 {
        return;
    }
    std::thread::sleep(Duration::from_millis(ms));
}
fn js_json<R: Runtime>(win: &WebviewWindow<R>, body: &str) -> Result<String, String> {
    eval(win, &format!("JSON.stringify((function(){{{body}}})())"))
}

fn pointer_button(params: &Value) -> Result<Button, String> {
    match params.get("button").and_then(Value::as_str) {
        None if params.get("button").is_none() => Ok(Button::Left),
        Some("left") => Ok(Button::Left),
        Some("right") => Ok(Button::Right),
        _ => Err("button must be 'left' or 'right'".into()),
    }
}

fn pointer_flags(params: &Value) -> Result<u64, String> {
    let Some(value) = params.get("modifiers") else {
        return Ok(0);
    };
    let modifiers = value.as_array().ok_or("modifiers must be an array")?;
    let mut flags = 0;
    for modifier in modifiers {
        let modifier = modifier.as_str().ok_or("modifier must be a string")?;
        flags |= match modifier.to_ascii_lowercase().as_str() {
            "shift" => keys::FLAG_SHIFT,
            "cmd" | "command" | "meta" => keys::FLAG_CMD,
            "alt" | "option" => keys::FLAG_ALT,
            "ctrl" | "control" => keys::FLAG_CTRL,
            _ => return Err(format!("unknown modifier '{modifier}'")),
        };
    }
    Ok(flags)
}

fn pointer_phase(params: &Value) -> Result<Mouse, String> {
    match params.get("phase").and_then(Value::as_str) {
        Some("down") => Ok(Mouse::Down),
        Some("drag") => Ok(Mouse::Drag),
        Some("up") => Ok(Mouse::Up),
        Some("move") => Ok(Mouse::Move),
        _ => Err("phase must be 'down', 'drag', 'up' or 'move'".into()),
    }
}

fn viewport_size(params: &Value) -> Result<(u32, u32), String> {
    let dimension = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| (1..=16384).contains(n))
            .map(|n| n as u32)
            .ok_or_else(|| format!("'{key}' must be an integer in 1..=16384 CSS pixels"))
    };
    Ok((dimension("width")?, dimension("height")?))
}

fn route<R: Runtime>(
    method: &str,
    params: &Value,
    win: &WebviewWindow<R>,
    app: &AppHandle<R>,
    commands: &HashMap<String, CommandFn<R>>,
    label: &str,
    vp: &ViewportCtx,
) -> Result<Out, String> {
    // Typed field access on the parsed params: text containing `"x":` or quotes can
    // never be mistaken for another field (the old flat string scan could).
    let str_field = |_: &Value, k: &str| params.get(k).and_then(Value::as_str).map(str::to_string);
    let num_field = |_: &Value, k: &str| params.get(k).and_then(Value::as_f64);
    let b = params;
    let n = |k: &str| num_field(b, k).ok_or_else(|| format!("missing number '{k}'"));
    match method {
        "set_viewport" => {
            let (width, height) = viewport_size(params)?;
            let mut cfg = vp.cfg;
            if let Some(ms) = params.get("settleMs") {
                let ms = ms
                    .as_u64()
                    .filter(|n| (1..=viewport::MAX_SETTLE.as_millis() as u64).contains(n))
                    .ok_or("'settleMs' must be an integer in 1..=10000")?;
                cfg.settle = Duration::from_millis(ms);
            }
            let outcome = viewport::set_viewport(
                &NativeWindow(win),
                &vp.placement,
                label,
                width,
                height,
                &cfg,
            )?;
            Ok(Out::Json(outcome.to_json().to_string()))
        }
        "focus" => {
            let how = native::focus_window(win)?;
            Ok(Out::Json(
                serde_json::json!({"ok": true, "focused": true, "how": how}).to_string(),
            ))
        }
        "move" => {
            let flags = pointer_flags(params)?;
            let ev = MouseEv {
                kind: Mouse::Move,
                button: Button::Left,
                x: n("x")?,
                y: n("y")?,
                clicks: 0,
                flags,
            };
            mouse_settled(win, &[ev], pace_ms(params)?)?;
            ok()
        }
        "pointer" => {
            let phase = pointer_phase(params)?;
            let button = pointer_button(params)?;
            let flags = pointer_flags(params)?;
            let ev = MouseEv {
                kind: phase,
                button,
                x: n("x")?,
                y: n("y")?,
                clicks: if phase == Mouse::Move { 0 } else { 1 },
                flags,
            };
            mouse_settled(win, &[ev], pace_ms(params)?)?;
            ok()
        }
        "click" => {
            let (x, y) = (n("x")?, n("y")?);
            let btn = pointer_button(params)?;
            let count = num_field(b, "count").unwrap_or(1.0).max(1.0) as i64;
            // Optional held modifiers, e.g. {"modifiers":["shift"]}: carried on every
            // native mouse event, so the page sees e.shiftKey/metaKey/altKey/ctrlKey.
            let flags = pointer_flags(params)?;
            let ev = |kind, button, clicks| MouseEv {
                kind,
                button,
                x,
                y,
                clicks,
                flags,
            };
            let mut evs = vec![ev(Mouse::Move, Button::Left, 0)];
            for c in 1..=count {
                evs.push(ev(Mouse::Down, btn, c));
                evs.push(ev(Mouse::Up, btn, c));
            }
            mouse_settled(win, &evs, pace_ms(params)?)?;
            ok()
        }
        "drag" => {
            let (x1, y1, x2, y2) = (n("x1")?, n("y1")?, n("x2")?, n("y2")?);
            let steps = num_field(b, "steps").unwrap_or(12.0).clamp(1.0, 200.0) as u32;
            let btn = pointer_button(params)?;
            let flags = pointer_flags(params)?;
            let ev = |kind, button, x, y| MouseEv {
                kind,
                button,
                x,
                y,
                clicks: if kind == Mouse::Move { 0 } else { 1 },
                flags,
            };
            let mut evs = vec![
                ev(Mouse::Move, Button::Left, x1, y1),
                ev(Mouse::Down, btn, x1, y1),
            ];
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                evs.push(ev(Mouse::Drag, btn, x1 + (x2 - x1) * t, y1 + (y2 - y1) * t));
            }
            evs.push(ev(Mouse::Up, btn, x2, y2));
            mouse_settled(win, &evs, pace_ms(params)?)?;
            ok()
        }
        "wheel" => {
            let (x, y) = (n("x")?, n("y")?);
            let (dx, dy) = (
                num_field(b, "dx").unwrap_or(0.0),
                num_field(b, "dy").unwrap_or(0.0),
            );
            if dx == 0.0 && dy == 0.0 {
                wheel(win, x, y, dx, dy)?;
            } else {
                settled(win, "wheel", 1, || wheel(win, x, y, dx, dy))?;
            }
            ok()
        }
        "key" => {
            let spec = str_field(b, "key").ok_or("missing 'key'")?;
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let (code, mut flags) =
                keys::chord(&spec).ok_or_else(|| format!("unknown key '{spec}'"))?;
            let last = match spec.rsplit('+').next().unwrap_or("") {
                "" => "+",
                l => l,
            };
            let mut ch = if flags & (keys::FLAG_CMD | keys::FLAG_CTRL) == 0 {
                key_char(last)
            } else {
                String::new()
            };
            if ch.is_empty() {
                if let Some(c) = keys::named_char(last) {
                    ch = c.to_string();
                } else if last.chars().count() == 1 {
                    ch = last.to_string();
                }
            }
            // macOS: navigation and F-keys are AppKit function characters plus the
            // Function (and, for arrows, NumericPad) flag, with or without modifiers.
            #[cfg(target_os = "macos")]
            if let Some((c, extra)) = keys::function_char(last) {
                ch = c.to_string();
                flags |= extra;
            }
            let evs = [
                KeyEv::Key {
                    down: true,
                    code,
                    chars: ch.clone(),
                    flags,
                },
                KeyEv::Key {
                    down: false,
                    code,
                    chars: ch,
                    flags,
                },
            ];
            keys_settled(win, &evs, pace_ms(params)?)?;
            ok()
        }
        "type" => {
            let text = str_field(b, "text").ok_or("missing 'text'")?;
            let pace = pace_ms(params)?;
            // Real key down/up events per character (keydown/keypress/keyup listeners fire), sent
            // back to back: one native hop per chunk, not two round trips per character.
            let evs: Vec<KeyEv> = text.chars().flat_map(type_events).collect();
            keys_settled(win, &evs, pace)?;
            ok()
        }
        "wait" => {
            let js = str_field(b, "js").ok_or("missing 'js'")?;
            let timeout = bounded_ms(params, "timeoutMs", 10_000, 1, MAX_WAIT_MS)?;
            let interval = bounded_ms(params, "intervalMs", 20, 1, 5_000)?;
            wait_route(win, &js, timeout, interval)
        }
        "eval" => {
            let js = str_field(b, "js").ok_or("missing 'js'")?;
            let v = js_json(win, &js)?;
            Ok(Out::Json(format!(
                "{{\"ok\":true,\"value\":{}}}",
                if v.is_empty() { "null".into() } else { v }
            )))
        }
        "dom" => {
            let sel = str_field(b, "selector").ok_or("missing 'selector'")?;
            let js = format!(
                "const q=document.querySelectorAll(\"{}\");return Array.from(q).slice(0,200).map(e=>{{const r=e.getBoundingClientRect();return {{tag:e.tagName.toLowerCase(),id:e.id,cls:String(e.className),text:(e.innerText||e.value||'').slice(0,200),rect:[r.x,r.y,r.width,r.height],hover:e.matches(':hover'),focus:e===document.activeElement,disabled:!!e.disabled}}}});",
                esc(&sel)
            );
            let v = js_json(win, &js)?;
            Ok(Out::Json(format!("{{\"ok\":true,\"elements\":{v}}}")))
        }
        "ax" => {
            let js = r#"const sel='a,button,input,select,textarea,summary,[role],[tabindex],h1,h2,h3,h4,label';return Array.from(document.querySelectorAll(sel)).slice(0,500).map(e=>{const r=e.getBoundingClientRect();const role=e.getAttribute('role')||({A:'link',BUTTON:'button',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox',H1:'heading',H2:'heading',H3:'heading',H4:'heading',LABEL:'label',SUMMARY:'button'})[e.tagName]||e.tagName.toLowerCase();return {role,name:(e.getAttribute('aria-label')||e.innerText||e.value||e.placeholder||'').trim().slice(0,120),rect:[r.x,r.y,r.width,r.height],disabled:!!e.disabled,checked:e.checked===undefined?null:e.checked}});"#;
            let v = js_json(win, js)?;
            Ok(Out::Json(format!("{{\"ok\":true,\"nodes\":{v}}}")))
        }
        "screenshot" => {
            // Redact before persistence: credential-bearing elements are hidden in the page
            // while the pixels are captured, so the secret never reaches the PNG. Caller
            // `mask` selectors extend the built-in set; masking failure discards the capture.
            let extra: Vec<String> = params
                .get("mask")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let selectors = [SECRET_SELECTORS.to_string()]
                .into_iter()
                .chain(extra)
                .collect::<Vec<_>>()
                .join(",");
            let sel_json = serde_json::to_string(&selectors).map_err(|e| e.to_string())?;
            // Hide, capture and restore as one operation (one main-thread operation on macOS).
            // Masking failure discards the capture; a restore failure is an error.
            let hide = format!(
                "try{{if(document.visibilityState==='hidden')for(const a of document.getAnimations()){{try{{a.finish()}}catch(e){{}}}}}}catch(e){{}}const els=Array.from(document.querySelectorAll({sel_json}));window.__rkMasked=els.map(e=>[e,e.style.getPropertyValue('visibility'),e.style.getPropertyPriority('visibility')]);for(const e of els)e.style.setProperty('visibility','hidden','important');return String(els.length);"
            );
            let (png, masked) = native::screenshot_masked(
                win,
                &format!("JSON.stringify((function(){{{hide}}})())"),
                SCREENSHOT_RESTORE,
            )?;
            MASKED_LAST.store(masked, Ordering::SeqCst);
            Ok(Out::Png(png))
        }
        "command" => {
            let name = str_field(b, "name").ok_or("missing 'name'")?;
            let f = commands
                .get(&name)
                .ok_or_else(|| format!("unknown command '{name}'"))?;
            // The client sends `{name, args}` with `args` as a string (`Control::command`): the
            // command gets that args text verbatim, not the whole request object (ViewRight #106).
            let args = match params.get("args") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            let v = f(app, &args)?;
            Ok(Out::Json(format!(
                "{{\"ok\":true,\"result\":{}}}",
                if v.is_empty() { "null".into() } else { v }
            )))
        }
        _ => Err(format!("unknown method {method}")),
    }
}

/// Key events (down and up count separately) sent per native hop by a paceless `type`.
const TYPE_CHUNK_EVENTS: usize = 128;
/// Largest `timeoutMs` for `wait`.
const MAX_WAIT_MS: u64 = 600_000;
/// One in-page wait slice; the route loops slices up to the total timeout.
const WAIT_SLICE_MS: u64 = 5_000;
/// Restores what the screenshot hide script changed; idempotent.
const SCREENSHOT_RESTORE: &str = "JSON.stringify((function(){for(const [e,v,p] of (window.__rkMasked||[])){if(v)e.style.setProperty('visibility',v,p);else e.style.removeProperty('visibility');}window.__rkMasked=[];return true;})())";

fn bounded_ms(params: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| format!("'{key}' must be an integer in {min}..={max}")),
    }
}

/// The in-page waiter for one slice: a Promise that resolves with a JSON string
/// `{"done":true,"value":..,"ms":..}` as soon as the predicate is truthy, or
/// `{"done":false,"last":..,"err":..,"ms":..}` at `slice_ms`. The predicate body is inlined here,
/// never built with `new Function`, so a strict page CSP cannot block it. Re-evaluated on DOM
/// mutations, common UI events, a short interval and (when visible) every frame; a hidden page
/// runs no rAF and may throttle timers, so mutations and events carry it there.
fn wait_script(body: &str, slice_ms: u64, interval_ms: u64) -> String {
    const TEMPLATE: &str = r#"(function(){return new Promise(function(__res){
var __t0=performance.now(),__done=false,__last=null,__err=null,__obs=null,__iv=0,__to=0,__raf=0;
var __evs=['input','change','click','keyup','keydown','transitionend','animationend','load','focusin','focusout','scroll','pointerup'];
var __pred=function(){__BODY__
};
function __ok(v){return v!==null&&v!==undefined&&v!==false&&v!==0&&v!==''&&!(typeof v==='number'&&isNaN(v));}
function __fin(done){if(__done)return;__done=true;try{if(__obs)__obs.disconnect();}catch(e){}clearInterval(__iv);clearTimeout(__to);if(__raf)cancelAnimationFrame(__raf);for(var i=0;i<__evs.length;i++)document.removeEventListener(__evs[i],__chk,true);var key=done?'value':'last';var o={done:done,ms:performance.now()-__t0,err:__err};o[key]=__last;var out;try{out=JSON.stringify(o);}catch(e){o[key]=String(__last);out=JSON.stringify(o);}__res(out);}
function __chk(){if(__done)return;try{var v=__pred();if(__ok(v)){__last=v;__fin(true);return;}__last=v===undefined?null:v;__err=null;}catch(e){__err=String(e&&e.message||e);}}
__chk();if(__done)return;
try{__obs=new MutationObserver(__chk);__obs.observe(document,{subtree:true,childList:true,attributes:true,characterData:true});}catch(e){}
for(var i=0;i<__evs.length;i++)document.addEventListener(__evs[i],__chk,true);
__iv=setInterval(__chk,__INTERVAL__);
if(document.visibilityState==='visible'){var __f=function(){if(__done)return;__chk();__raf=requestAnimationFrame(__f);};__raf=requestAnimationFrame(__f);}
__to=setTimeout(function(){__chk();__fin(false);},__SLICE__);
});})()"#;
    TEMPLATE
        .replace("__INTERVAL__", &interval_ms.to_string())
        .replace("__SLICE__", &slice_ms.to_string())
        .replace("__BODY__", body)
}

/// Wait in the page for `js` to return something truthy; slices of at most [`WAIT_SLICE_MS`]
/// until `timeout_ms`. Reports what it saw; a timeout is a result (`matched:false`), not an error.
fn wait_route<R: Runtime>(
    win: &WebviewWindow<R>,
    js: &str,
    timeout_ms: u64,
    interval_ms: u64,
) -> Result<Out, String> {
    let start = std::time::Instant::now();
    let mut last = Value::Null;
    let mut err;
    loop {
        let remaining = timeout_ms.saturating_sub(start.elapsed().as_millis() as u64);
        let slice = remaining.clamp(1, WAIT_SLICE_MS);
        let script = wait_script(js, slice, interval_ms);
        match native::wait_slice(win, &script, Duration::from_millis(slice + 3_000)) {
            Ok(raw) => {
                let v: Value =
                    serde_json::from_str(&raw).map_err(|e| format!("wait reply: {e}"))?;
                if v["done"] == json!(true) {
                    return Ok(Out::Json(
                        json!({"ok": true, "matched": true, "value": v["value"], "elapsedMs": start.elapsed().as_millis() as u64}).to_string(),
                    ));
                }
                last = v["last"].clone();
                err = v["err"].clone();
            }
            // A slice the page never completed (script error, throttled timers): report it.
            Err(e) => err = json!(e),
        }
        if start.elapsed().as_millis() as u64 >= timeout_ms {
            return Ok(Out::Json(
                json!({"ok": true, "matched": false, "last": last, "error": err, "elapsedMs": start.elapsed().as_millis() as u64}).to_string(),
            ));
        }
    }
}

/// Character a bare named key produces (so `Enter` inserts a newline).
fn key_char(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => "\r".into(),
        "tab" => "\t".into(),
        "space" => " ".into(),
        "escape" | "esc" => "\u{1b}".into(),
        "delete" | "backspace" => "\u{7f}".into(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_activation_only_for_the_hidden_flag() {
        use std::ffi::OsStr;
        assert!(hidden_requested(Some(OsStr::new("1"))));
        assert!(!hidden_requested(Some(OsStr::new("0"))));
        assert!(!hidden_requested(Some(OsStr::new(""))));
        assert!(!hidden_requested(None));
        assert_eq!(ENV_HIDDEN, "RIGHTKIT_QA_HIDDEN");
    }
    #[test]
    fn ipc_fixture_script_only_when_the_service_env_is_set() {
        assert!(ipc_fixture_script(None).is_none());
        assert!(ipc_fixture_script(Some("")).is_none());
        assert!(ipc_fixture_script(Some("  ")).is_none());
        let script = ipc_fixture_script(Some("svc")).expect("script when the service is active");
        assert!(script.contains("__RK_QA_IPC__"));
        assert!(script.contains("window.fetch"));
    }
    #[test]
    fn ipc_fixture_script_never_assigns_invoke_ipc_or_post_message() {
        let script = ipc_fixture_script(Some("svc")).unwrap();
        let code: String = script
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for name in ["invoke", "ipc", "postMessage"] {
            for assign in [
                format!(".{name} ="),
                format!(".{name}="),
                format!("['{name}']"),
                format!("[\"{name}\"]"),
            ] {
                assert!(
                    !code.contains(&assign),
                    "fixture script must not assign {name}: {assign}"
                );
            }
        }
        assert!(!code.contains("defineProperty"));
        assert!(!code.contains("__TAURI_INTERNALS__.invoke"));
    }
    #[test]
    fn qa_marker_is_the_fixed_scanned_string() {
        // `packages/release/qa-control-scan.mjs` scans for exactly these bytes.
        assert_eq!(QA_CONTROL_MARKER, "RIGHTKIT-QA-CONTROL-SERVER-PRESENT");
        assert_eq!(&QA_CONTROL_MARKER_BYTES[..], QA_CONTROL_MARKER.as_bytes());
        assert_eq!(keep_marker(), b'R');
    }
    #[test]
    fn key_char_names() {
        assert_eq!(key_char("Enter"), "\r");
        assert!(METHODS.contains(&"screenshot"));
        assert!(METHODS.contains(&"set_viewport"));
    }
    #[test]
    fn effectful_methods_are_classified() {
        for m in ["click", "move", "drag", "pointer", "wheel", "key", "type"] {
            assert_eq!(effect_kind(m), Some(EffectKind::Input), "{m}");
        }
        assert_eq!(effect_kind("eval"), Some(EffectKind::Script));
        assert_eq!(effect_kind("screenshot"), Some(EffectKind::Capture));
        assert_eq!(effect_kind("set_viewport"), Some(EffectKind::Command));
        assert!(METHODS.contains(&"focus"));
        assert_eq!(effect_kind("focus"), Some(EffectKind::Command));
        assert_eq!(effect_kind("command"), Some(EffectKind::Command));
        for m in ["health", "dom", "ax", "lease_status", "lease_acquire"] {
            assert_eq!(effect_kind(m), None, "{m}");
        }
    }
    #[test]
    fn pace_defaults_to_zero_and_is_bounded() {
        assert_eq!(pace_ms(&json!({})).unwrap(), 0);
        assert_eq!(pace_ms(&json!({"paceMs": null})).unwrap(), 0);
        assert_eq!(pace_ms(&json!({"paceMs": 40})).unwrap(), 40);
        for bad in [json!(-1), json!(5001), json!("8"), json!(1.5)] {
            assert!(pace_ms(&json!({"paceMs": bad})).is_err(), "{bad}");
        }
    }
    #[test]
    fn new_methods_are_listed_and_classified() {
        assert!(METHODS.contains(&"wait") && METHODS.contains(&"batch"));
        // `wait` runs page script: gated like eval. `batch` gates each step itself.
        assert_eq!(effect_kind("wait"), Some(EffectKind::Script));
        assert_eq!(effect_kind("batch"), None);
    }
    #[test]
    fn wait_script_inlines_the_predicate_and_never_uses_new_function() {
        let s = wait_script("return window.__x === 1; // note", 4000, 15);
        assert!(s.contains("return window.__x === 1; // note\n}"), "{s}");
        assert!(s.contains("setInterval(__chk,15)") && s.contains("},4000);"));
        assert!(!s.contains("new Function") && !s.contains("eval("));
        assert!(!s.contains("__BODY__") && !s.contains("__SLICE__") && !s.contains("__INTERVAL__"));
    }
    #[test]
    fn type_events_are_a_key_down_up_pair() {
        let evs = type_events('a');
        assert_eq!(evs.len(), 2);
        assert!(matches!(&evs[0], KeyEv::Key { down: true, chars, .. } if chars == "a"));
        assert!(matches!(&evs[1], KeyEv::Key { down: false, .. }));
    }
    #[test]
    fn secret_selectors_cover_credentials() {
        for s in ["input[type=password]", "one-time-code", "data-rk-secret"] {
            assert!(SECRET_SELECTORS.contains(s), "{s}");
        }
        // The restore script undoes exactly what the hide script records, and is idempotent.
        assert!(
            SCREENSHOT_RESTORE.contains("__rkMasked")
                && SCREENSHOT_RESTORE.contains("removeProperty('visibility')")
        );
    }
    #[test]
    fn viewport_dimensions_are_bounded_integers() {
        assert_eq!(
            viewport_size(&json!({"width":1440,"height":1000})).unwrap(),
            (1440, 1000)
        );
        for value in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!(16385),
            json!("1440"),
            Value::Null,
        ] {
            assert!(viewport_size(&json!({"width":value,"height":1000})).is_err());
            assert!(viewport_size(&json!({"width":1440,"height":value})).is_err());
        }
        assert!(viewport_size(&json!({"width":1440})).is_err());
    }
    #[test]
    fn pointer_fields_validate_before_dispatch() {
        assert!(METHODS.contains(&"pointer"));
        for (phase, expected) in [
            ("down", Mouse::Down),
            ("drag", Mouse::Drag),
            ("up", Mouse::Up),
            ("move", Mouse::Move),
        ] {
            assert_eq!(pointer_phase(&json!({"phase": phase})).unwrap(), expected);
        }
        assert!(pointer_phase(&json!({"phase": "click"})).is_err());
        assert_eq!(pointer_button(&json!({})).unwrap(), Button::Left);
        assert_eq!(
            pointer_button(&json!({"button": "right"})).unwrap(),
            Button::Right
        );
        assert!(pointer_button(&json!({"button": "middle"})).is_err());
        assert_eq!(
            pointer_flags(&json!({"modifiers": ["SHIFT", "meta", "option", "control"]})).unwrap(),
            keys::FLAG_SHIFT | keys::FLAG_CMD | keys::FLAG_ALT | keys::FLAG_CTRL
        );
        assert!(pointer_flags(&json!({"modifiers": "shift"})).is_err());
        assert!(pointer_flags(&json!({"modifiers": [false]})).is_err());
        assert!(pointer_flags(&json!({"modifiers": ["unknown"]})).is_err());
    }
}
