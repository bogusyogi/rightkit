//! In-app control server (Tauri plugin, feature `tauri-plugin`).
//!
//! Started only when the launcher sets `RIGHTKIT_CONTROL_SERVICE=<name>`
//! (a lowercase service name such as `control-genright`); normal user launches
//! never set it, so nothing listens. The server is a `rightkit-service` host:
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
//!   health  click{x,y,button?,count?}  move{x,y}  drag{x1,y1,x2,y2,steps?}
//!   wheel{x,y,dx,dy}  key{key:"cmd+k"}  type{text}  eval{js}  dom{selector}
//!   ax  screenshot{mask?:[selector]} -> {path,bytes,masked} (PNG in `<service dir>/shots/`,
//!   owner-only; password/one-time-code/data-rk-secret elements and `mask` selectors are
//!   hidden during capture so secrets never reach the file)
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

use crate::admission::{
    CancelToken, EffectGate, EffectKind, EffectRequest, ExecError, SettleOutcome,
};
use crate::json::esc;
use crate::keys;
use crate::lease::{InputError, InputLease, InputOutcome, UnacknowledgedInputDecision};
use rightkit_service::{
    host, Allowlist, CallContext, Emit, Handler, ServiceConfig, ServiceError, Suite,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

#[cfg(target_os = "macos")]
#[path = "mac_view.rs"]
mod native;
#[cfg(windows)]
#[path = "win_view.rs"]
mod native;
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
    ) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn key<R: Runtime>(
        _: &WebviewWindow<R>,
        _: bool,
        _: u16,
        _: &str,
        _: u64,
    ) -> Result<(), String> {
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
    pub fn screenshot<R: Runtime>(_: &WebviewWindow<R>) -> Result<Vec<u8>, String> {
        Err(NO.into())
    }
}
/// Windows inserts characters with no virtual key through `Input.insertText`.
#[cfg(windows)]
fn type_char<R: Runtime>(win: &WebviewWindow<R>, c: char) -> Result<(), String> {
    let s = c.to_string();
    if !c.is_ascii() {
        return native::insert_text(win, &s);
    }
    let code = keys::chord(&s.to_lowercase())
        .map(|(k, _)| k)
        .unwrap_or(0xFF);
    let fl = if c.is_uppercase() {
        keys::FLAG_SHIFT
    } else {
        0
    };
    key(win, true, code, &s, fl)?;
    key(win, false, code, &s, fl)
}
#[cfg(not(windows))]
fn type_char<R: Runtime>(win: &WebviewWindow<R>, c: char) -> Result<(), String> {
    let s = c.to_string();
    let code = keys::chord(&s.to_lowercase())
        .map(|(k, _)| k)
        .unwrap_or(0xFF);
    let fl = if c.is_uppercase() {
        keys::FLAG_SHIFT
    } else {
        0
    };
    key(win, true, code, &s, fl)?;
    key(win, false, code, &s, fl)
}
use native::{eval, key, mouse, screenshot, wheel};

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

/// Launcher-set environment variable that names the service and enables control.
pub const ENV_SERVICE: &str = "RIGHTKIT_CONTROL_SERVICE";

/// Every method the server understands (the default allowlist for QA/CLI apps).
pub const METHODS: &[&str] = &[
    "health",
    "click",
    "move",
    "drag",
    "wheel",
    "key",
    "type",
    "eval",
    "dom",
    "ax",
    "screenshot",
    "command",
    "lease_acquire",
    "lease_release",
    "lease_reconcile",
    "lease_status",
];

/// Effect kind of a control method; `None` for reads and lease management.
pub fn effect_kind(method: &str) -> Option<EffectKind> {
    match method {
        "click" | "move" | "drag" | "wheel" | "key" | "type" => Some(EffectKind::Input),
        "eval" => Some(EffectKind::Script),
        "screenshot" => Some(EffectKind::Capture),
        "command" => Some(EffectKind::Command),
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
        }
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
        let Control {
            label,
            commands,
            allow,
            gate,
            lease,
        } = self;
        let commands = Arc::new(commands);
        tauri::plugin::Builder::new("rightkit-control")
            .setup(move |app, _| {
                if let Some(service) = service_from_env() {
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
                    )?;
                }
                Ok(())
            })
            .build()
    }
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
) -> Result<(), Box<dyn std::error::Error>> {
    // Fail closed: no absolute suite root means no server, never a cwd-relative one.
    let suite = Suite::resolve()?;
    let shots = suite.private_service_subdir(&service, "shots")?;
    let cfg = ServiceConfig::new(
        &service,
        &format!("control:{label}"),
        0,
        env!("CARGO_PKG_VERSION"),
        allow,
    );
    let handler = Arc::new(ControlHandler {
        app,
        label,
        commands,
        shots,
        shot_seq: AtomicU64::new(0),
        guards,
    });
    let hosted = host(&suite, cfg, handler)?;
    // Hosted releases on drop; the plugin keeps it for the life of the process.
    std::mem::forget(hosted);
    Ok(())
}

impl<R: Runtime> Handler for ControlHandler<R> {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        if method == "health" {
            return Ok(json!({"ok": true, "service": "rightkit-control"}));
        }
        if let Some(r) = self.lease_method(method, params) {
            return r;
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
            let session =
                crate::lease::ControlSession::new(lease.clone(), self.guards.gate.clone());
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

fn snapshot_json(s: crate::lease::LeaseSnapshot) -> Value {
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
        let lease_err = |e: crate::lease::LeaseError| {
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
        match route(method, params, &win, &self.app, &self.commands) {
            Ok(Out::Json(j)) => Ok(serde_json::from_str(&j).unwrap_or(Value::Null)),
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
    std::thread::sleep(Duration::from_millis(ms));
}
fn js_json<R: Runtime>(win: &WebviewWindow<R>, body: &str) -> Result<String, String> {
    eval(win, &format!("JSON.stringify((function(){{{body}}})())"))
}

fn route<R: Runtime>(
    method: &str,
    params: &Value,
    win: &WebviewWindow<R>,
    app: &AppHandle<R>,
    commands: &HashMap<String, CommandFn<R>>,
) -> Result<Out, String> {
    // Typed field access on the parsed params: text containing `"x":` or quotes can
    // never be mistaken for another field (the old flat string scan could).
    let str_field = |_: &Value, k: &str| params.get(k).and_then(Value::as_str).map(str::to_string);
    let num_field = |_: &Value, k: &str| params.get(k).and_then(Value::as_f64);
    let b = params;
    let n = |k: &str| num_field(b, k).ok_or_else(|| format!("missing number '{k}'"));
    match method {
        "move" => {
            mouse(win, Mouse::Move, Button::Left, n("x")?, n("y")?, 0)?;
            ok()
        }
        "click" => {
            let (x, y) = (n("x")?, n("y")?);
            let btn = if str_field(b, "button").as_deref() == Some("right") {
                Button::Right
            } else {
                Button::Left
            };
            let count = num_field(b, "count").unwrap_or(1.0).max(1.0) as i64;
            mouse(win, Mouse::Move, Button::Left, x, y, 0)?;
            pause(50);
            for c in 1..=count {
                mouse(win, Mouse::Down, btn, x, y, c)?;
                pause(40);
                mouse(win, Mouse::Up, btn, x, y, c)?;
                pause(40);
            }
            ok()
        }
        "drag" => {
            let (x1, y1, x2, y2) = (n("x1")?, n("y1")?, n("x2")?, n("y2")?);
            let steps = num_field(b, "steps").unwrap_or(12.0).clamp(1.0, 200.0) as u32;
            mouse(win, Mouse::Move, Button::Left, x1, y1, 0)?;
            pause(50);
            mouse(win, Mouse::Down, Button::Left, x1, y1, 1)?;
            pause(50);
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                mouse(
                    win,
                    Mouse::Drag,
                    Button::Left,
                    x1 + (x2 - x1) * t,
                    y1 + (y2 - y1) * t,
                    1,
                )?;
                pause(16);
            }
            mouse(win, Mouse::Up, Button::Left, x2, y2, 1)?;
            ok()
        }
        "wheel" => {
            wheel(
                win,
                n("x")?,
                n("y")?,
                num_field(b, "dx").unwrap_or(0.0),
                num_field(b, "dy").unwrap_or(0.0),
            )?;
            ok()
        }
        "key" => {
            let spec = str_field(b, "key").ok_or("missing 'key'")?;
            let (code, flags) =
                keys::chord(&spec).ok_or_else(|| format!("unknown key '{spec}'"))?;
            let last = spec.rsplit('+').next().unwrap_or("");
            let mut ch = if flags & (keys::FLAG_CMD | keys::FLAG_CTRL) == 0 {
                key_char(last)
            } else {
                String::new()
            };
            if ch.is_empty() && last.chars().count() == 1 {
                ch = last.to_string();
            }
            key(win, true, code, &ch, flags)?;
            pause(20);
            key(win, false, code, &ch, flags)?;
            ok()
        }
        "type" => {
            let text = str_field(b, "text").ok_or("missing 'text'")?;
            for c in text.chars() {
                type_char(win, c)?;
                pause(8);
            }
            ok()
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
            let hide = format!(
                "const els=Array.from(document.querySelectorAll({sel_json}));window.__rkMasked=els.map(e=>[e,e.style.getPropertyValue('visibility'),e.style.getPropertyPriority('visibility')]);for(const e of els)e.style.setProperty('visibility','hidden','important');return els.length;"
            );
            let masked = js_json(win, &hide)
                .map_err(|e| format!("screenshot masking failed ({e}); capture refused"))?;
            let shot = screenshot(win);
            let restore = "for(const [e,v,p] of (window.__rkMasked||[])){if(v)e.style.setProperty('visibility',v,p);else e.style.removeProperty('visibility');}window.__rkMasked=[];return true;";
            let restored = js_json(win, restore);
            let png = shot?;
            restored.map_err(|e| format!("could not restore masked elements: {e}"))?;
            MASKED_LAST.store(masked.trim().parse().unwrap_or(0), Ordering::SeqCst);
            Ok(Out::Png(png))
        }
        "command" => {
            let name = str_field(b, "name").ok_or("missing 'name'")?;
            let f = commands
                .get(&name)
                .ok_or_else(|| format!("unknown command '{name}'"))?;
            let v = f(app, &params.to_string())?;
            Ok(Out::Json(format!(
                "{{\"ok\":true,\"result\":{}}}",
                if v.is_empty() { "null".into() } else { v }
            )))
        }
        _ => Err(format!("unknown method {method}")),
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
    fn key_char_names() {
        assert_eq!(key_char("Enter"), "\r");
        assert!(METHODS.contains(&"screenshot"));
    }
    #[test]
    fn effectful_methods_are_classified() {
        for m in ["click", "move", "drag", "wheel", "key", "type"] {
            assert_eq!(effect_kind(m), Some(EffectKind::Input), "{m}");
        }
        assert_eq!(effect_kind("eval"), Some(EffectKind::Script));
        assert_eq!(effect_kind("screenshot"), Some(EffectKind::Capture));
        assert_eq!(effect_kind("command"), Some(EffectKind::Command));
        for m in ["health", "dom", "ax", "lease_status", "lease_acquire"] {
            assert_eq!(effect_kind(m), None, "{m}");
        }
    }
}
