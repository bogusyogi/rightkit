//! Windows in-process input via WebView2 `CallDevToolsProtocolMethod`
//! (`Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` / `Runtime.evaluate`
//! / `Page.captureScreenshot`). No `SendInput`, no window activation.
//!
//! Verified on Windows by Pulse's hub journey, which drives a hidden WebView2 through this
//! module on a `windows-2025` GitHub runner (rightkit-control 0.1.11 with `wv2-0_39`; the
//! passing run is Pulse CI 38084463429). That run covers move, down/up, click, eval and
//! screenshot; it does not cover every method here (for example wheel and key paths are
//! exercised only where the journey uses them).

use super::{Button, Mouse};
use crate::json::str_field;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::{Runtime, WebviewWindow};
#[cfg(not(any(feature = "wv2-0_38", feature = "wv2-0_39")))]
compile_error!("rightkit-control's Windows plugin needs one of `wv2-0_38` (Tauri 2.11) or `wv2-0_39` (Tauri 2.12+); neither is a default feature");
use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
use windows::core::HSTRING;
#[cfg(all(feature = "wv2-0_38", not(feature = "wv2-0_39")))]
use ::{webview2_com_038 as webview2_com, windows_061 as windows};
#[cfg(feature = "wv2-0_39")]
use ::{webview2_com_039 as webview2_com, windows_062 as windows};

const CDP_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Opt-in tracing: `RIGHTKIT_CONTROL_TRACE=1` writes to stderr, any other non-empty value is
/// a file path to append to. Off by default; nothing is formatted when off.
fn trace_sink() -> &'static Option<String> {
    static SINK: OnceLock<Option<String>> = OnceLock::new();
    SINK.get_or_init(|| {
        std::env::var("RIGHTKIT_CONTROL_TRACE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty() && v != "0")
    })
}

fn trace_enabled() -> bool {
    trace_sink().is_some()
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
}

fn trace(line: impl FnOnce() -> String) {
    let Some(sink) = trace_sink() else { return };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!(
        "[rk-control-trace {ms} tid={}] {}\n",
        unsafe { GetCurrentThreadId() },
        line()
    );
    if sink == "1" {
        eprint!("{line}");
    } else if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sink)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

/// How far a devtools call got: 0 queued for the UI thread, 1 issued to WebView2, 2 completed.
const STAGE_QUEUED: u8 = 0;
const STAGE_ISSUED: u8 = 1;
const STAGE_DONE: u8 = 2;

fn cdp_once<R: Runtime>(
    win: &WebviewWindow<R>,
    id: u64,
    method: &str,
    params: String,
    timeout: Duration,
    stage: Arc<AtomicU8>,
) -> Result<String, String> {
    let (tx, rx) = channel::<Result<String, String>>();
    let method_owned = method.to_string();
    let queued = Instant::now();
    trace(|| format!("devtools#{id} enqueue {method}"));
    let stage_ui = stage.clone();
    win.with_webview(move |pv| unsafe {
        stage_ui.store(STAGE_ISSUED, Ordering::SeqCst);
        trace(|| {
            format!(
                "devtools#{id} dispatch {method_owned} after {}ms on the UI thread",
                queued.elapsed().as_millis()
            )
        });
        let core = match pv.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        };
        let tx2 = tx.clone();
        let stage_done = stage_ui.clone();
        let name = method_owned.clone();
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |res, json| {
                stage_done.store(STAGE_DONE, Ordering::SeqCst);
                trace(|| {
                    format!(
                        "devtools#{id} complete {name} after {}ms ok={}",
                        queued.elapsed().as_millis(),
                        res.is_ok()
                    )
                });
                let _ = tx2.send(res.map(|_| json).map_err(|e| e.to_string()));
                Ok(())
            }));
        if let Err(e) = core.CallDevToolsProtocolMethod(
            &HSTRING::from(method_owned),
            &HSTRING::from(params),
            &handler,
        ) {
            let _ = tx.send(Err(e.to_string()));
        }
    })
    .map_err(|e| e.to_string())?;
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err("devtools call timed out".to_string()),
    }
}

fn cdp<R: Runtime>(win: &WebviewWindow<R>, method: &str, params: String) -> Result<String, String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let id = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let stage = Arc::new(AtomicU8::new(STAGE_QUEUED));
    match cdp_once(win, id, method, params, CDP_TIMEOUT, stage.clone()) {
        Err(e) if e == "devtools call timed out" => {
            let why = match stage.load(Ordering::SeqCst) {
                STAGE_QUEUED => format!(
                    "{method}: the UI thread never ran the call within {}s (the main thread is blocked or not dispatching)",
                    CDP_TIMEOUT.as_secs()
                ),
                _ => {
                    // The call reached WebView2. A browser-level call that is answered by the
                    // browser process separates "renderer unresponsive" from "channel dead".
                    let probe_stage = Arc::new(AtomicU8::new(STAGE_QUEUED));
                    let probe = cdp_once(
                        win,
                        id,
                        "Browser.getVersion",
                        "{}".into(),
                        PROBE_TIMEOUT,
                        probe_stage,
                    );
                    let mut why = if probe.as_ref().err().map(|e| e.as_str())
                        == Some("devtools call timed out")
                    {
                        format!(
                            "{method} was issued but neither it nor a browser-level probe completed (devtools channel or browser process unresponsive)"
                        )
                    } else {
                        format!(
                            "{method} was issued and the browser process still answers, but the page did not complete it within {}s (renderer main thread busy or blocked, a modal script dialog, or a paused debugger)",
                            CDP_TIMEOUT.as_secs()
                        )
                    };
                    if capture_enabled() {
                        why.push_str("; page stacks: ");
                        why.push_str(&capture_stacks(win, id));
                    }
                    why
                }
            };
            trace(|| format!("devtools#{id} TIMEOUT {why}"));
            Err(format!("devtools call timed out: {why}"))
        }
        other => other,
    }
}

/// Stack capture on a devtools timeout is on with `RIGHTKIT_CONTROL_TRACE` or
/// `RIGHTKIT_CONTROL_STACK_ON_TIMEOUT=1`. It enables the V8 debugger at webview creation
/// (`Debugger.enable` is not an interrupting command, so it would queue behind a busy script if
/// sent only after the hang) and later interrupts the running script with `Debugger.pause`.
fn capture_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        trace_enabled()
            || std::env::var("RIGHTKIT_CONTROL_STACK_ON_TIMEOUT")
                .map(|v| v.trim() == "1")
                .unwrap_or(false)
    })
}

/// Receives the next `Debugger.paused` payload while a capture is waiting for it.
static PAUSE_TX: std::sync::Mutex<Option<std::sync::mpsc::Sender<String>>> =
    std::sync::Mutex::new(None);

/// `fn url:line:col` for the top `limit` frames of a `Debugger.paused` payload.
fn frames_summary(paused: &str, limit: usize) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(paused) else {
        return format!(
            "unparsable Debugger.paused payload: {}",
            &paused[..paused.len().min(200)]
        );
    };
    let reason = v["reason"].as_str().unwrap_or("?");
    let frames: Vec<String> = v["callFrames"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(limit)
                .map(|f| {
                    let name = f["functionName"]
                        .as_str()
                        .filter(|n| !n.is_empty())
                        .unwrap_or("(anonymous)");
                    let url = f["url"].as_str().filter(|u| !u.is_empty()).unwrap_or("?");
                    format!(
                        "{name} {url}:{}:{}",
                        f["location"]["lineNumber"].as_i64().unwrap_or(-1) + 1,
                        f["location"]["columnNumber"].as_i64().unwrap_or(-1) + 1
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    format!("[reason={reason}] {}", frames.join(" <- "))
}

/// Interrupt the page's main thread up to three times and report where it was. Out of band: the
/// stuck `Runtime.evaluate` is not involved. If no pause arrives within 2 s the renderer is in
/// native code or gone.
fn capture_stacks<R: Runtime>(win: &WebviewWindow<R>, id: u64) -> String {
    let quiet = || Arc::new(AtomicU8::new(STAGE_QUEUED));
    // Normally a no-op (armed at webview creation); re-sent in case a navigation reset the agent.
    let _ = cdp_once(
        win,
        id,
        "Debugger.enable",
        "{}".into(),
        Duration::from_millis(500),
        quiet(),
    );
    let mut samples = Vec::new();
    for n in 1..=3 {
        let (tx, rx) = channel::<String>();
        *PAUSE_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let sent = cdp_once(
            win,
            id,
            "Debugger.pause",
            "{}".into(),
            Duration::from_secs(2),
            quiet(),
        );
        let paused = rx.recv_timeout(Duration::from_secs(2));
        *PAUSE_TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
        match paused {
            Ok(p) => {
                let line = frames_summary(&p, 15);
                trace(|| format!("devtools#{id} STACK sample {n}: {line}"));
                samples.push(format!("sample {n}: {line}"));
                let _ = cdp_once(
                    win,
                    id,
                    "Debugger.resume",
                    "{}".into(),
                    Duration::from_secs(2),
                    quiet(),
                );
            }
            Err(_) => {
                let line = format!(
                    "sample {n}: no Debugger.paused within 2s (Debugger.pause {}); the renderer is in native code, not running script, or gone",
                    match &sent { Ok(_) => "was accepted".to_string(), Err(e) => format!("failed: {e}") }
                );
                trace(|| format!("devtools#{id} STACK {line}"));
                samples.push(line);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    samples.join(" | ")
}

/// Enable the V8 debugger at webview creation and route `Debugger.paused` events: to a waiting
/// capture, or (unrequested pauses, for example a `debugger;` statement) logged and resumed at
/// once so the page behaves as it would without the harness.
fn arm_debugger<R: Runtime>(webview: &tauri::Webview<R>) {
    use webview2_com::{
        DevToolsProtocolEventReceivedEventHandler, NavigationCompletedEventHandler,
    };
    let label = webview.label().to_string();
    let _ = webview.with_webview(move |pv| unsafe {
        let Ok(core) = pv.controller().CoreWebView2() else {
            return;
        };
        let mut token: i64 = 0;
        if let Ok(receiver) =
            core.GetDevToolsProtocolEventReceiver(&HSTRING::from("Debugger.paused"))
        {
            let l = label.clone();
            let h =
                DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |sender, args| {
                    let Some(a) = args else { return Ok(()) };
                    let mut p = windows::core::PWSTR::null();
                    let _ = a.ParameterObjectAsJson(&mut p);
                    let payload = pwstr_string(p);
                    let waiting = PAUSE_TX
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                        .map(|tx| tx.send(payload.clone()).is_ok())
                        .unwrap_or(false);
                    if !waiting {
                        trace(|| {
                            format!(
                                "[{l}] unrequested Debugger.paused {} (resuming)",
                                frames_summary(&payload, 5)
                            )
                        });
                        if let Some(core) = sender {
                            let noop = CallDevToolsProtocolMethodCompletedHandler::create(
                                Box::new(|_, _| Ok(())),
                            );
                            let _ = core.CallDevToolsProtocolMethod(
                                &HSTRING::from("Debugger.resume"),
                                &HSTRING::from("{}"),
                                &noop,
                            );
                        }
                    }
                    Ok(())
                }));
            let _ = receiver.add_DevToolsProtocolEventReceived(&h, &mut token);
        }
        let send_enable = |core: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2| {
            let h = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
            let _ = core.CallDevToolsProtocolMethod(
                &HSTRING::from("Debugger.enable"),
                &HSTRING::from("{}"),
                &h,
            );
        };
        send_enable(&core);
        // A navigation can reset the agent; re-enable after each one.
        let nav = NavigationCompletedEventHandler::create(Box::new(move |sender, _| {
            if let Some(core) = sender {
                send_enable(&core);
            }
            Ok(())
        }));
        let _ = core.add_NavigationCompleted(&nav, &mut token);
        trace(|| format!("[{label}] debugger armed for stack capture"));
    });
}

fn pwstr_string(p: windows::core::PWSTR) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { p.to_string() }.unwrap_or_default()
    }
}

/// Opt-in (`RIGHTKIT_CONTROL_TRACE`): log WebView2 navigation, dialog, new-window and process
/// failure events for this webview so a stuck devtools call can be matched to what the page did.
pub fn install_trace<R: Runtime>(webview: &tauri::Webview<R>) {
    use webview2_com::{
        NavigationCompletedEventHandler, NavigationStartingEventHandler,
        NewWindowRequestedEventHandler, ProcessFailedEventHandler, ScriptDialogOpeningEventHandler,
    };
    if capture_enabled() {
        arm_debugger(webview);
    }
    if !trace_enabled() {
        return;
    }
    let label = webview.label().to_string();
    let _ = webview.with_webview(move |pv| unsafe {
        let core = match pv.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                trace(|| format!("[{label}] install_trace: no CoreWebView2: {e}"));
                return;
            }
        };
        let mut token: i64 = 0;
        let l = label.clone();
        let h = NavigationStartingEventHandler::create(Box::new(move |_s, args| {
            if let Some(a) = args {
                let (mut uri, mut id, mut user) = (
                    windows::core::PWSTR::null(),
                    0u64,
                    windows::core::BOOL::default(),
                );
                let _ = a.Uri(&mut uri);
                let _ = a.NavigationId(&mut id);
                let _ = a.IsUserInitiated(&mut user);
                trace(|| {
                    format!(
                        "[{l}] NavigationStarting id={id} user={} uri={}",
                        user.as_bool(),
                        pwstr_string(uri)
                    )
                });
            }
            Ok(())
        }));
        let _ = core.add_NavigationStarting(&h, &mut token);
        let l = label.clone();
        let h = NavigationCompletedEventHandler::create(Box::new(move |_s, args| {
            if let Some(a) = args {
                let (mut id, mut ok) = (0u64, windows::core::BOOL::default());
                let mut status = Default::default();
                let _ = a.NavigationId(&mut id);
                let _ = a.IsSuccess(&mut ok);
                let _ = a.WebErrorStatus(&mut status);
                trace(|| {
                    format!(
                        "[{l}] NavigationCompleted id={id} success={} webErrorStatus={}",
                        ok.as_bool(),
                        status.0
                    )
                });
            }
            Ok(())
        }));
        let _ = core.add_NavigationCompleted(&h, &mut token);
        let l = label.clone();
        let h = ScriptDialogOpeningEventHandler::create(Box::new(move |_s, args| {
            if let Some(a) = args {
                let (mut uri, mut msg) =
                    (windows::core::PWSTR::null(), windows::core::PWSTR::null());
                let mut kind = Default::default();
                let _ = a.Uri(&mut uri);
                let _ = a.Message(&mut msg);
                let _ = a.Kind(&mut kind);
                trace(|| {
                    format!(
                        "[{l}] ScriptDialogOpening kind={} uri={} message={}",
                        kind.0,
                        pwstr_string(uri),
                        pwstr_string(msg)
                    )
                });
            }
            Ok(())
        }));
        let _ = core.add_ScriptDialogOpening(&h, &mut token);
        let l = label.clone();
        let h = NewWindowRequestedEventHandler::create(Box::new(move |_s, args| {
            if let Some(a) = args {
                let mut uri = windows::core::PWSTR::null();
                let _ = a.Uri(&mut uri);
                trace(|| format!("[{l}] NewWindowRequested uri={}", pwstr_string(uri)));
            }
            Ok(())
        }));
        let _ = core.add_NewWindowRequested(&h, &mut token);
        let l = label.clone();
        let h = ProcessFailedEventHandler::create(Box::new(move |_s, args| {
            if let Some(a) = args {
                let mut kind = Default::default();
                let _ = a.ProcessFailedKind(&mut kind);
                trace(|| format!("[{l}] ProcessFailed kind={}", kind.0));
            }
            Ok(())
        }));
        let _ = core.add_ProcessFailed(&h, &mut token);
        trace(|| format!("[{label}] install_trace: handlers registered"));
    });
    // Every 2 s: controller visibility and bounds plus a browser-level round trip, so a
    // hidden or zero-sized controller and a stalled browser process are visible in the log.
    let probe_webview = webview.clone();
    let label = webview.label().to_string();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        let label = label.clone();
        let queued = Instant::now();
        let sent = probe_webview.with_webview(move |pv| unsafe {
            let controller = pv.controller();
            let mut visible = windows::core::BOOL::default();
            let mut bounds = windows::Win32::Foundation::RECT::default();
            let _ = controller.IsVisible(&mut visible);
            let _ = controller.Bounds(&mut bounds);
            let dispatched = queued.elapsed().as_millis();
            let Ok(core) = controller.CoreWebView2() else {
                return;
            };
            let sent = Instant::now();
            let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                move |res, _json| {
                    trace(|| {
                        format!(
                            "[{label}] probe isVisible={} bounds={}x{} at ({},{}) ui_dispatch={}ms Browser.getVersion={}ms ok={}",
                            visible.as_bool(),
                            bounds.right - bounds.left,
                            bounds.bottom - bounds.top,
                            bounds.left,
                            bounds.top,
                            dispatched,
                            sent.elapsed().as_millis(),
                            res.is_ok()
                        )
                    });
                    Ok(())
                },
            ));
            let _ = core.CallDevToolsProtocolMethod(
                &HSTRING::from("Browser.getVersion"),
                &HSTRING::from("{}"),
                &handler,
            );
        });
        if sent.is_err() {
            break;
        }
    });
}

fn cdp_mods(flags: u64) -> u32 {
    use crate::keys::*;
    (if flags & FLAG_ALT != 0 { 1 } else { 0 })
        | (if flags & FLAG_CTRL != 0 { 2 } else { 0 })
        | (if flags & FLAG_CMD != 0 { 4 } else { 0 })
        | (if flags & FLAG_SHIFT != 0 { 8 } else { 0 })
}

pub fn mouse<R: Runtime>(
    win: &WebviewWindow<R>,
    kind: Mouse,
    button: Button,
    x: f64,
    y: f64,
    clicks: i64,
    flags: u64,
) -> Result<(), String> {
    // CDP modifiers: Alt=1, Ctrl=2, Meta=4, Shift=8.
    let modifiers = [
        (crate::keys::FLAG_ALT, 1),
        (crate::keys::FLAG_CTRL, 2),
        (crate::keys::FLAG_CMD, 4),
        (crate::keys::FLAG_SHIFT, 8),
    ]
    .iter()
    .filter(|(f, _)| flags & f != 0)
    .map(|(_, m)| m)
    .sum::<u32>();
    let (b, mask) = match button {
        Button::Left => ("left", 1),
        Button::Right => ("right", 2),
    };
    let (ty, button, buttons) = match kind {
        Mouse::Down => ("mousePressed", b, mask),
        Mouse::Up => ("mouseReleased", b, 0),
        Mouse::Move => ("mouseMoved", "none", 0),
        Mouse::Drag => ("mouseMoved", b, mask),
    };
    cdp(win, "Input.dispatchMouseEvent", format!(r#"{{"type":"{ty}","x":{x},"y":{y},"button":"{button}","buttons":{buttons},"clickCount":{clicks},"modifiers":{modifiers}}}"#)).map(|_| ())
}

pub fn wheel<R: Runtime>(
    win: &WebviewWindow<R>,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> Result<(), String> {
    cdp(
        win,
        "Input.dispatchMouseEvent",
        format!(r#"{{"type":"mouseWheel","x":{x},"y":{y},"deltaX":{dx},"deltaY":{dy}}}"#),
    )
    .map(|_| ())
}

/// Windows VK for a US-layout printable character: letters and digits are their ASCII
/// uppercase value; punctuation uses the `VK_OEM_*` codes (the ASCII value would collide
/// with `VK_SNAPSHOT`, `VK_DELETE`, `VK_LWIN`, `VK_RIGHT`, ...). Shifted symbols map to
/// their base key. Anything else has no virtual key (0).
fn char_vk(c: char) -> u32 {
    match c {
        'a'..='z' | 'A'..='Z' | '0'..='9' => c.to_ascii_uppercase() as u32,
        ';' | ':' => 186,
        '=' | '+' => 187,
        ',' | '<' => 188,
        '-' | '_' => 189,
        '.' | '>' => 190,
        '/' | '?' => 191,
        '`' | '~' => 192,
        '[' | '{' => 219,
        '\\' | '|' => 220,
        ']' | '}' => 221,
        '\'' | '"' => 222,
        '!' => 49,
        '@' => 50,
        '#' => 51,
        '$' => 52,
        '%' => 53,
        '^' => 54,
        '&' => 55,
        '*' => 56,
        '(' => 57,
        ')' => 48,
        _ => 0,
    }
}

/// `code` is the macOS virtual keycode used across the crate; map it to a Windows VK.
fn vk(code: u16, chars: &str) -> u32 {
    match code {
        0x24 => 13,
        0x30 => 9,
        0x31 => 32,
        0x33 => 8,
        0x35 => 27,
        0x75 => 46,
        0x7B => 37,
        0x7E => 38,
        0x7C => 39,
        0x7D => 40,
        0x73 => 36,
        0x77 => 35,
        0x74 => 33,
        0x79 => 34,
        // F1..F12 (mac codes -> VK_F1 = 112 ..).
        0x7A => 112,
        0x78 => 113,
        0x63 => 114,
        0x76 => 115,
        0x60 => 116,
        0x61 => 117,
        0x62 => 118,
        0x64 => 119,
        0x65 => 120,
        0x6D => 121,
        0x67 => 122,
        0x6F => 123,
        _ => crate::keys::ansi_char(code)
            .map(char_vk)
            .filter(|v| *v != 0)
            .unwrap_or_else(|| chars.chars().next().map(char_vk).unwrap_or(0)),
    }
}

/// CDP `key` value for the named keys the crate maps by virtual code; otherwise the character itself.
fn key_value(vk: u32, chars: &str) -> String {
    match vk {
        13 => "Enter".into(),
        9 => "Tab".into(),
        8 => "Backspace".into(),
        27 => "Escape".into(),
        37 => "ArrowLeft".into(),
        38 => "ArrowUp".into(),
        39 => "ArrowRight".into(),
        40 => "ArrowDown".into(),
        36 => "Home".into(),
        35 => "End".into(),
        33 => "PageUp".into(),
        34 => "PageDown".into(),
        _ => chars.to_string(),
    }
}

/// The full `Input.dispatchKeyEvent` payload, serialized by serde_json so quotes,
/// backslashes, control characters and non-ASCII text can never break the JSON.
/// `text` is sent only on key-down of a character-producing key without Ctrl/Meta;
/// control characters other than Enter/Tab are never sent as text.
fn key_event_params(down: bool, code: u16, chars: &str, flags: u64) -> String {
    let vk = vk(code, chars);
    let mods = cdp_mods(flags);
    let producing = |c: char| !c.is_control() || c == '\r' || c == '\t';
    let text: String = if down && mods & 6 == 0 && chars.chars().all(producing) {
        chars.to_string()
    } else {
        String::new()
    };
    let ty = if !down {
        "keyUp"
    } else if text.is_empty() {
        "rawKeyDown"
    } else {
        "keyDown"
    };
    let mut v = serde_json::json!({
        "type": ty,
        "modifiers": mods,
        "windowsVirtualKeyCode": vk,
        "nativeVirtualKeyCode": vk,
        "key": key_value(vk, chars),
    });
    if !text.is_empty() {
        v["text"] = serde_json::Value::String(text.clone());
        v["unmodifiedText"] = serde_json::Value::String(text);
    }
    v.to_string()
}

pub fn key<R: Runtime>(
    win: &WebviewWindow<R>,
    down: bool,
    code: u16,
    chars: &str,
    flags: u64,
) -> Result<(), String> {
    cdp(
        win,
        "Input.dispatchKeyEvent",
        key_event_params(down, code, chars, flags),
    )
    .map(|_| ())
}

/// Insert text exactly as given (any Unicode, emoji, newlines) with `Input.insertText`;
/// per-character key events cannot express characters with no virtual key.
pub fn insert_text<R: Runtime>(win: &WebviewWindow<R>, text: &str) -> Result<(), String> {
    cdp(
        win,
        "Input.insertText",
        serde_json::json!({ "text": text }).to_string(),
    )
    .map(|_| ())
}

pub fn eval<R: Runtime>(win: &WebviewWindow<R>, js: &str) -> Result<String, String> {
    let r = cdp(
        win,
        "Runtime.evaluate",
        serde_json::json!({ "expression": js, "returnByValue": true }).to_string(),
    )?;
    Ok(str_field(&r, "value").unwrap_or_default())
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn GetClientRect(hwnd: *mut std::ffi::c_void, rect: *mut Rect) -> i32;
    fn GetWindowRect(hwnd: *mut std::ffi::c_void, rect: *mut Rect) -> i32;
    fn SetWindowPos(
        hwnd: *mut std::ffi::c_void,
        after: *mut std::ffi::c_void,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> i32;
}

/// The page's CSS viewport right now, for the settle loop in `viewport.rs`.
pub fn read_viewport<R: Runtime>(
    win: &WebviewWindow<R>,
    _: Duration,
) -> Result<(f64, f64), String> {
    let s = eval(win, "innerWidth + ',' + innerHeight")?;
    let (w, h) = s.split_once(',').ok_or("CSS viewport measurement failed")?;
    Ok((
        w.trim().parse().map_err(|_| "invalid CSS width")?,
        h.trim().parse().map_err(|_| "invalid CSS height")?,
    ))
}

/// SetWindowPos has no work-area clamp. Preserve visibility, z-order and focus.
/// Converges on the target when it can; a size the system will not give is not an
/// error here: the caller reads back and reports what the page actually has.
pub fn apply_viewport<R: Runtime>(
    win: &WebviewWindow<R>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let hwnd = win.hwnd().map_err(|e| e.to_string())?.0 as usize;
    let target = (width as f64, height as f64);
    let measure = || -> Result<(f64, f64), String> {
        let s = eval(win, "innerWidth + ',' + innerHeight")?;
        let (w, h) = s.split_once(',').ok_or("CSS viewport measurement failed")?;
        Ok((
            w.parse().map_err(|_| "invalid CSS width")?,
            h.parse().map_err(|_| "invalid CSS height")?,
        ))
    };
    let mut viewport = measure()?;
    for _ in 0..3 {
        if viewport.0 <= 0.0 || viewport.1 <= 0.0 {
            return Err("CSS viewport is empty".into());
        }
        let (tx, rx) = channel();
        win.run_on_main_thread(move || unsafe {
            let hwnd = hwnd as *mut std::ffi::c_void;
            let (mut client, mut frame) = (Rect::default(), Rect::default());
            let result = (|| {
                if GetClientRect(hwnd, &mut client) == 0 || GetWindowRect(hwnd, &mut frame) == 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                let scale = (client.right - client.left) as f64 / viewport.0;
                let w = (frame.right - frame.left) as f64 + (target.0 - viewport.0) * scale;
                let h = (frame.bottom - frame.top) as f64 + (target.1 - viewport.1) * scale;
                if SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    w.round() as i32,
                    h.round() as i32,
                    0x0002 | 0x0004 | 0x0010 | 0x0200,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                Ok(())
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "window resize timed out")??;
        for _ in 0..10 {
            viewport = measure()?;
            if viewport == target {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(())
}

pub fn screenshot<R: Runtime>(win: &WebviewWindow<R>) -> Result<Vec<u8>, String> {
    let r = cdp(win, "Page.captureScreenshot", r#"{"format":"png"}"#.into())?;
    let b64 = str_field(&r, "data").ok_or("no screenshot data")?;
    let mut out = Vec::with_capacity(b64.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in b64.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hostile inputs must round-trip through the CDP payload unchanged.
    #[test]
    fn key_payload_is_valid_json_for_hostile_text() {
        for t in [
            "\"",
            "\\",
            "a\"b\\c",
            "\n",
            "é",
            "日本",
            "\u{1f600}",
            "\u{7f}",
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&key_event_params(true, 0xFF, t, 0)).expect("valid JSON");
            let text = v.get("text").and_then(|x| x.as_str());
            if t.chars().all(|c| !c.is_control()) {
                assert_eq!(text, Some(t), "{t:?}");
            } else {
                assert_eq!(text, None, "control text is not inserted: {t:?}");
            }
        }
        let enter: serde_json::Value =
            serde_json::from_str(&key_event_params(true, 0x24, "\r", 0)).unwrap();
        assert_eq!(enter["key"], "Enter");
        assert_eq!(enter["text"], "\r");
    }
}

#[cfg(test)]
mod vk_tests {
    use super::*;
    use crate::keys::{chord, named};

    fn vk_of(spec: &str) -> u32 {
        let (code, _) = chord(spec).unwrap();
        let last = spec
            .rsplit('+')
            .next()
            .filter(|l| !l.is_empty())
            .unwrap_or("+");
        let ch = crate::keys::named_char(last)
            .map(String::from)
            .or_else(|| (last.chars().count() == 1).then(|| last.to_string()))
            .unwrap_or_default();
        vk(code, &ch)
    }

    #[test]
    fn named_keys_map_to_windows_vk() {
        for (n, v) in [
            ("enter", 13),
            ("tab", 9),
            ("space", 32),
            ("backspace", 8),
            ("esc", 27),
            ("left", 37),
            ("up", 38),
            ("right", 39),
            ("down", 40),
            ("home", 36),
            ("end", 35),
            ("pageup", 33),
            ("pagedown", 34),
            ("forwarddelete", 46),
        ] {
            assert_eq!(vk(named(n).unwrap(), ""), v, "{n}");
        }
    }

    #[test]
    fn function_keys_map_to_vk_f1_through_f12() {
        for n in 1..=12u32 {
            assert_eq!(vk_of(&format!("f{n}")), 111 + n, "f{n}");
        }
    }

    #[test]
    fn digits_and_letters_use_ascii_vk() {
        for d in '0'..='9' {
            assert_eq!(vk_of(&format!("cmd+{d}")), d as u32, "{d}");
        }
        for l in 'a'..='z' {
            assert_eq!(
                vk_of(&format!("ctrl+{l}")),
                l.to_ascii_uppercase() as u32,
                "{l}"
            );
        }
    }

    #[test]
    fn punctuation_uses_oem_vk_not_colliding_ascii() {
        for (spec, v) in [
            ("cmd+;", 186),
            ("cmd+=", 187),
            ("cmd+,", 188),
            ("cmd+-", 189),
            ("cmd+.", 190),
            ("cmd+/", 191),
            ("cmd+`", 192),
            ("cmd+[", 219),
            ("cmd+\\", 220),
            ("cmd+]", 221),
            ("cmd+'", 222),
            ("cmd+comma", 188),
            ("cmd+period", 190),
            ("cmd+slash", 191),
            ("cmd+minus", 189),
            ("cmd+equal", 187),
        ] {
            assert_eq!(vk_of(spec), v, "{spec}");
        }
    }

    #[test]
    fn cmd_plus_is_shifted_oem_plus() {
        let (code, flags) = chord("cmd++").unwrap();
        assert_eq!(flags, crate::keys::FLAG_CMD | crate::keys::FLAG_SHIFT);
        assert_eq!(vk(code, "+"), 187);
        assert_eq!(cdp_mods(flags), 4 | 8);
    }

    #[test]
    fn typed_symbols_never_collide_with_navigation_vk() {
        // Unmapped-code typed characters fall back to the character's own VK.
        assert_eq!(vk(0xFF, "!"), 49);
        assert_eq!(vk(0xFF, "%"), 53);
        assert_eq!(vk(0xFF, "'"), 222);
        assert_eq!(vk(0xFF, "("), 57);
        assert_eq!(vk(0xFF, "é"), 0);
    }

    #[test]
    fn modifier_flags_map_to_cdp_modifiers() {
        use crate::keys::*;
        assert_eq!(cdp_mods(FLAG_ALT), 1);
        assert_eq!(cdp_mods(FLAG_CTRL), 2);
        assert_eq!(cdp_mods(FLAG_CMD), 4);
        assert_eq!(cdp_mods(FLAG_SHIFT), 8);
        assert_eq!(cdp_mods(FLAG_ALT | FLAG_CTRL | FLAG_CMD | FLAG_SHIFT), 15);
        assert_eq!(cdp_mods(0), 0);
    }

    #[test]
    fn key_event_payload_is_valid_json_with_oem_vk() {
        let v: serde_json::Value =
            serde_json::from_str(&key_event_params(true, 0x2B, ",", crate::keys::FLAG_CMD))
                .unwrap();
        assert_eq!(v["windowsVirtualKeyCode"], 188);
        assert_eq!(v["modifiers"], 4);
        assert_eq!(v["key"], ",");
        assert!(v.get("text").is_none());
    }
}
