//! `screen.observe` and `screen.act`: a remote caller sees and drives the windows of any app on
//! this computer except the blocked ones, through RightKit's `rightkit-automation` (macOS Accessibility, Windows
//! UI Automation). the host app does no capture and no input of its own. Design ruling: D3 (every
//! family stays gateway-reachable; reachability is never activation).
//!
//! **`screen.observe`** (tier 2, `remoteScreenObserve`, off) needs `remoteTools`, the user's
//! disclosure grant (`remoteScreenDisclosure`). There is no allowlist of windows: the scope is
//! every app except the owner's blocklist (`remoteBlockedApps`, empty by default) and a hard floor
//! that is never configurable (the host app itself, System Settings, security and permission prompts;
//! `settings::hard_floor`). the owner, 2026-10-17: "we should have a block list, the opposite of an
//! allow list". With no `window` it lists the on-screen windows outside that floor and the
//! blocklist; with a `window` it returns that window's accessibility tree (ids that stay stable
//! while the structure does) and, with `screenshot`, a PNG of it. A blocked or unknown window is
//! `window_not_approved`, never read. Secure fields, fields labelled
//! as credentials, text that looks like a key and every common token are withheld or masked in the
//! tree, and the capture is redacted by the crate (secure and credential fields painted black).
//! The OS permission is only checked by a remote call, never asked for: a missing Accessibility
//! (or, for a screenshot, Screen Recording) grant is `permission_missing`. The grants belong to
//! the process that acts, the resident engine (`pulse-engine`, a LaunchAgent), not to owner app.
//! The engine asks for them itself, and only for the owner: `permissions_request` calls
//! `AXIsProcessTrustedWithOptions` with the prompt option and `CGRequestScreenCaptureAccess`
//! (Apple documents both as the way for the calling process to ask), and `permissions_status`
//! reads them back for the hub's Permissions view. Windows UI Automation needs no OS grant.
//!
//! **`screen.act`** (tier 3, `remoteScreenAct`, off, its own switch) needs the same grants and
//! acts on a snapshot `screen.observe` returned to the same door. Approvals depend on the door
//! (the owner, 2026-10-17): the local agent and the owner (`Origin::Local`) are asked only for what
//! sends, buys or publishes (rule 3 below) and for a control labelled as a delete; ordinary
//! clicks, typing and menu presses on a non-blocked app run without a question. The ChatGPT
//! gateway (`Origin::Gateway`, a remote door) keeps the stricter rule: every action needs a local
//! approval bound to the target (app id, process id and start time, window and its title, element
//! id, role and name) and the action (kind and full payload); the first call answers
//! `approval_required`, the user approves in the host app, and the caller repeats the call with
//! `approval`. A target that changed since the snapshot (the process was replaced, the window is
//! gone, retitled, or blocked, the structure moved) is `target_changed`, and an approval
//! given for another target or action is `approval_invalid`. Actions run with the crate's
//! allowlist, kill switch, secure-field and credential gates, freshness guard (`Guard::Structure`)
//! and strict focus guard, and never above the targeted tier: foreground input (which would take
//! the user's focus and pointer) is not allowed, and launching, terminating or delivering text to
//! another app is refused. A label that reads as delete, send, purchase or close is named in the
//! approval summary and is confirmed by that approval. For the gateway every action is asked, with
//! one exception (below).
//!
//! **Form fill** (Orthic-Labs/an upstream issue, defaults the RK3 Mac lead set; the owner can change them).
//! Three rules, checked on every `set_value`, `type_text` and `keys` call whatever the switches say
//! and for every door (rules 1 and 3 are the approvals that stay for the local agent too):
//! (1) the agent never types a password, card number, bank detail or government ID itself: a value
//! that is one (`sensitive::sensitive_value`: Luhn, IBAN, SSN, routing number and other national
//! IDs), a field labelled as one (`sensitive_label`, plus the crate's credential labels and secure
//! fields) is refused with `sensitive_input`, and the user types it or lets their password manager
//! or the system's autofill do it; (2) with `remoteFormFill` on, the user's own non-sensitive
//! details (`remoteFormFillProfile`: name, email, phone, address, typed by the user in settings and
//! never read from anywhere else) may be typed (`action.profile`, or `text` equal to a profile
//! value) after a standing grant for that app: the gateway's grant is keyed on the app id, is
//! listed in `remoteFormFillStanding` (an app id, or `app::title fragment`) or given once in the host app
//! (family `fill:<app>`, ended after 30 days without use), and replaces only the per-action
//! question for that typing; the local agent needs no grant; (3) pressing, clicking
//! or choosing a control that sends, buys, pays, publishes or confirms (`submit_label`), or typing
//! a newline or Return, is never covered by a grant: it asks every time, and the approval says so.
//!
//! **Spot takeover** (Orthic-Labs/an upstream issue, docs/agent-credentials-payments.md "Additions"): for a
//! step the agent must not do (the pay button, a sign-in, a CAPTCHA) it calls `screen.act` with
//! `{type: "handover"}` and an element. The call is asked every time, for every door, through the
//! same approval as a submit; once approved, the owner's owner app shows only that element's region,
//! dims the rest of the screen and offers Done and Cancel (`screen.spot.current`,
//! `screen.spot.end`, owner-only). The call waits for the answer and returns `handed_back`. While
//! it waits every screen tool answers `user_in_control`: nothing is observed, captured or acted on.
//!
//! Receipts: each call is one line in the Activity log (`audit`): the tool, the action kind, the
//! tier that ran, the outcome and counts; never an app, a window title, element text or a typed
//! value.
//!
//! Threading (macOS): the crate reads the app list through NSWorkspace, which publishes through
//! the main run loop. A host without a running main loop may see a stale app list; that is
//! untested here (see docs/chatgpt-gateway.md).

use super::settings::{Settings, hard_floor};
use super::{Origin, Refusal, Result};
use serde_json::{Value, json};

const TEXT_MAX: usize = 4096;
const PATH_ITEMS_MAX: usize = 8;
const DEFAULT_NODES: u64 = 300;
const HARD_NODES: u64 = 600;
const DEFAULT_DEPTH: u64 = 12;
const HARD_DEPTH: u64 = 20;
/// Longest element value or name returned, in characters.
const VALUE_MAX: usize = 500;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

/// `screen.observe`. `args`: `window` (a window id from the list), `screenshot` (bool),
/// `maxNodes` (1 to 600, default 300), `maxDepth` (1 to 20, default 12).
pub fn observe(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    settings.require_screen_observe()?;
    user_in_control()?;
    imp::observe(settings, origin, args)
}

/// `screen.act`. `args`: `snapshot` (from `screen.observe`), `element` (an id in it), `action`
/// (`{type, ...}`: press, set_value, type_text, keys, click, scroll, focus, menu_press; `profile`
/// in place of `text` for set_value and type_text; `handover` with an `element` and optional
/// `seconds`, 10 to 600, default 180, gives that region to the user: see `Spot`), `approval`.
pub fn act(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    settings.require_screen_act()?;
    user_in_control()?;
    // `wait` (Zenon's `computer` wait, tools-desktop.ts:384) touches nothing: it only pauses, up to
    // 10 seconds, between two actions, so it needs no snapshot and no approval.
    if args["action"]["type"].as_str() == Some("wait") {
        let ms = count_arg(&args["action"], "ms", 1000, 10_000)?;
        std::thread::sleep(std::time::Duration::from_millis(ms));
        return Ok(json!({"done": true, "result": "waited", "waitedMs": ms}));
    }
    imp::act(settings, origin, args)
}

/// The clipboard's text and whether the clipboard holds nothing else (`clipboard.read`), through
/// the crate's platform clipboard snapshot. `None` text: no text on it.
pub fn clipboard_text() -> Result<(Option<String>, bool)> {
    imp::clipboard_text()
}

/// The OS grants of the process that runs the screen tools, as the Permissions view shows them:
/// `{ platform, process, grants: [{ id, title, status, why }] }`, with `status` one of `granted`,
/// `needsApproval` or `notNeeded` (Windows). Reading never prompts.
pub fn permissions_status() -> Value {
    imp::permissions_status()
}

/// Asks the OS for one grant of this process (`grant`: `accessibility` or `screenRecording`),
/// which shows the system prompt, and answers the status as `permissions_status` does. Owner-only
/// (the engine's `screen.permissions.request`); a remote call never reaches it.
pub fn permissions_request(grant: &str) -> Result<Value> {
    imp::permissions_request(grant)
}

/// A spot takeover in progress (Orthic-Labs/an upstream issue): the agent handed one region of one window
/// to the user (`screen.act` with `{type: "handover"}`, approved like any other act). While it
/// runs the owner's owner app shows only that region, dims the rest and offers Done and Cancel; the
/// agent's call waits here for the answer.
///
/// The screen tools run in more than one process on this computer (the engine for the local door,
/// `the gateway` for ChatGPT) while owner app reads the state from the engine, so the takeover
/// lives in one owner-only file, `screen-spot.json` in the host app's state folder, not in process memory.
/// The process that starts the handover creates it (exclusively) and polls it; `screen.spot.end`
/// (engine, owner-only) writes the answer into it; `user_in_control` and `screen.spot.current`
/// read it in every process. Format (JSON, one object): `v` (1), `id`, `label`, `app`, `rect`
/// (x, y, width, height in the screen coordinates the crate reports: points with the origin at the
/// top left of the main display on macOS, physical pixels of the virtual desktop on Windows),
/// `pid` and `pidStart` (the starting process and its start time, seconds), `expires` (unix
/// seconds), `end` (`done` or `cancelled` once the user answered, else null).
///
/// A takeover counts as in progress while its file exists, `expires` has not passed and the owning
/// process (pid and start time) is still running; a dead or expired owner's file is stale and
/// counts as none. Failure choice (fail closed while it can be bounded): a file that cannot be
/// read or parsed counts as a takeover in progress until its modification time plus the longest
/// takeover and a minute has passed, then as none; a file whose modification time cannot be read
/// either counts as a takeover. A missing file, or no state folder, is no takeover.
struct Spot {
    id: String,
    label: String,
    app: String,
    rect: [f64; 4],
    /// The target window (its id in this crate's words) and the program that owns it: the handover
    /// is bound to them, and the watch in `spot_run` ends it when that window moves or goes. Empty
    /// in a file an older build wrote.
    window: String,
    target_pid: u32,
    pid: u32,
    pid_start: u64,
    expires: u64,
    /// `done` or `cancelled` once the user answered; `None` while waiting.
    end: Option<&'static str>,
}

/// What the state file says right now.
enum Slot {
    /// No takeover (no file, or a stale one).
    Free,
    /// A takeover; `None` when the file cannot be read or parsed but is not yet old enough to ignore.
    Taken(Option<Spot>),
}

/// The longest takeover (`seconds` is at most 600) plus a minute: how long an unreadable file is
/// trusted to mean a takeover.
const SPOT_UNREADABLE_SECS: u64 = 660;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn spot_path() -> Option<std::path::PathBuf> {
    rightkit_app_host::state_root().map(|root| root.join("screen-spot.json"))
}

/// The start time (seconds) of process `pid`, or `None` when it is not running.
fn process_start(pid: u32) -> Option<u64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::everything(),
    );
    system.process(pid).map(|p| p.start_time())
}

fn spot_parse(text: &str) -> Option<Spot> {
    let v: Value = serde_json::from_str(text).ok()?;
    let rect = v["rect"].as_array().filter(|r| r.len() == 4)?;
    let mut r = [0.0; 4];
    for (slot, value) in r.iter_mut().zip(rect) {
        *slot = value.as_f64()?;
    }
    Some(Spot {
        id: v["id"].as_str()?.to_string(),
        label: v["label"].as_str()?.to_string(),
        app: v["app"].as_str()?.to_string(),
        rect: r,
        window: v["window"].as_str().unwrap_or_default().to_string(),
        target_pid: v["targetPid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .unwrap_or(0),
        pid: u32::try_from(v["pid"].as_u64()?).ok()?,
        pid_start: v["pidStart"].as_u64()?,
        expires: v["expires"].as_u64()?,
        end: match v["end"].as_str() {
            None => None,
            Some("done") => Some("done"),
            Some("cancelled") => Some("cancelled"),
            Some(_) => return None,
        },
    })
}

fn spot_json(s: &Spot) -> String {
    json!({
        "v": 1, "id": s.id, "label": s.label, "app": s.app, "rect": s.rect,
        "window": s.window, "targetPid": s.target_pid, "pid": s.pid, "pidStart": s.pid_start, "expires": s.expires, "end": s.end,
    })
    .to_string()
}

/// Reads the state file (see `Spot` for what counts as a takeover).
fn read_slot() -> Slot {
    use std::io::Read;
    let Some(path) = spot_path() else {
        return Slot::Free;
    };
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Slot::Free,
        Err(_) => return unreadable_slot(&path),
    };
    let mut text = String::new();
    if file.take(16 * 1024).read_to_string(&mut text).is_err() {
        return unreadable_slot(&path);
    }
    let Some(spot) = spot_parse(&text) else {
        return unreadable_slot(&path);
    };
    if unix_now() >= spot.expires || process_start(spot.pid) != Some(spot.pid_start) {
        return Slot::Free;
    }
    Slot::Taken(Some(spot))
}

fn unreadable_slot(path: &std::path::Path) -> Slot {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    match modified {
        Some(at) if unix_now() >= at.saturating_add(SPOT_UNREADABLE_SECS) => Slot::Free,
        // Not old enough to ignore, or its age cannot be told: fail closed.
        _ => Slot::Taken(None),
    }
}

fn spot_write(path: &std::path::Path, spot: &Spot) -> std::io::Result<()> {
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, spot_json(spot).as_bytes()),
    )
    .map_err(std::io::Error::from)
}

/// While the user is in control the agent sees nothing and does nothing: no screen tool runs, so
/// nothing the user types or sees in the region is read, captured or acted on (the design's rule
/// "no screenshots during takeover"). The check reads the shared state file, so it holds in every
/// process that runs a screen tool, and for `clipboard.read`.
pub(super) fn user_in_control() -> Result<()> {
    if matches!(read_slot(), Slot::Taken(_)) {
        return Err(Refusal::new(
            "user_in_control",
            "the user has taken over part of the screen; wait for the hand back, then call screen.observe again",
        ));
    }
    Ok(())
}

/// The takeover on screen now, for owner app: `{active: false}` or `{active: true, id, label,
/// app, x, y, w, h, secondsLeft}`. Owner-only (`screen.spot.current`).
pub fn spot_current() -> Value {
    match read_slot() {
        Slot::Taken(Some(s)) if s.end.is_none() => json!({
            "active": true,
            "id": s.id,
            "label": s.label,
            "app": s.app,
            "x": s.rect[0],
            "y": s.rect[1],
            "w": s.rect[2],
            "h": s.rect[3],
            "secondsLeft": s.expires.saturating_sub(unix_now()),
            // The shape the surfaces read (`{active, id, label, region, targetWindow, expires}`);
            // the flat fields above stay for owner app that already reads them.
            "region": {"x": s.rect[0], "y": s.rect[1], "w": s.rect[2], "h": s.rect[3]},
            "targetWindow": {"app": s.app, "window": s.window, "pid": s.target_pid},
            "expires": s.expires,
        }),
        _ => json!({"active": false}),
    }
}

/// The user's answer on the takeover `id`: `outcome` is `done` or `cancelled`. Owner-only
/// (`screen.spot.end`). The agent's waiting call returns on its next check.
pub fn spot_end(id: &str, outcome: &str) -> Result<Value> {
    let outcome = match outcome {
        "done" => "done",
        "cancelled" => "cancelled",
        _ => return Err(invalid("outcome is done or cancelled")),
    };
    match (read_slot(), spot_path()) {
        (Slot::Taken(Some(mut s)), Some(path)) if s.id == id && s.end.is_none() => {
            s.end = Some(outcome);
            spot_write(&path, &s).map_err(|_| {
                Refusal::new("write_failed", "the answer could not be saved; try again")
            })?;
            Ok(json!({"ended": true, "outcome": outcome}))
        }
        _ => Err(Refusal::new(
            "no_takeover",
            "there is no such takeover waiting for an answer",
        )),
    }
}

/// Starts a takeover and waits for the user's answer or `secs`; always clears it before returning.
/// Returns `done`, `cancelled` or `timed_out`.
#[cfg(any(target_os = "macos", windows))]
fn spot_run(
    label: String,
    app: String,
    rect: [f64; 4],
    target: (String, u32),
    secs: u64,
    moved: &dyn Fn() -> bool,
) -> Result<&'static str> {
    use std::io::Write;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let unavailable = || Refusal::new("read_failed", "the app's state folder is not available");
    let path = spot_path().ok_or_else(unavailable)?;
    if let Some(dir) = path.parent() {
        let _ = rightkit_fs::ensure_private_dir(dir);
    }
    let in_control = || Refusal::new("user_in_control", "a takeover is already on screen");
    match read_slot() {
        Slot::Taken(_) => return Err(in_control()),
        // A stale file (dead owner, expired) is replaced. Two starters that both find the same
        // stale file can still remove each other's new file; the window is that narrow.
        Slot::Free => {
            let _ = std::fs::remove_file(&path);
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let spot = Spot {
        id: format!("{nanos:x}"),
        label,
        app,
        rect,
        window: target.0,
        target_pid: target.1,
        pid,
        pid_start: process_start(pid).unwrap_or(0),
        expires: unix_now() + secs,
        end: None,
    };
    // Created exclusively and owner-only, so a second starter cannot overwrite a live takeover; a
    // reader that catches it half written finds it unreadable, which counts as a takeover.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            in_control()
        } else {
            Refusal::new("write_failed", "the takeover could not be started")
        }
    })?;
    if file.write_all(spot_json(&spot).as_bytes()).is_err() {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(Refusal::new(
            "write_failed",
            "the takeover could not be started",
        ));
    }
    drop(file);
    let mut ticks = 0u32;
    let outcome = loop {
        // About once a second: the target window must still be there, in place, with its title.
        // A takeover over a window that moved or went would show the wrong part of the screen.
        ticks += 1;
        if ticks.is_multiple_of(5) && moved() {
            break "target_moved";
        }
        match std::fs::read_to_string(&path) {
            // Only this process removes the file: gone means someone cleared it, which hands back.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break "cancelled",
            Ok(text) => match spot_parse(&text) {
                Some(s) if s.id != spot.id => break "cancelled",
                Some(Spot { end: Some(end), .. }) => break end,
                _ => {}
            },
            Err(_) => {}
        }
        if unix_now() >= spot.expires {
            break "timed_out";
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Cleared only if it is still ours.
    if std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| spot_parse(&t))
        .is_none_or(|s| s.id == spot.id)
    {
        let _ = std::fs::remove_file(&path);
    }
    Ok(outcome)
}

/// One `remoteFormFillStanding` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    app: String,
    title: Option<String>,
}

/// `app` or `app::title fragment`, lower case; an entry with no app is dropped.
fn parse_scope(entries: &[String]) -> Vec<Scope> {
    entries
        .iter()
        .filter_map(|entry| {
            let (app, title) = match entry.split_once("::") {
                Some((app, title)) => (app, Some(title)),
                None => (entry.as_str(), None),
            };
            let app = app.trim().to_lowercase();
            if app.is_empty() {
                return None;
            }
            let title = title
                .map(|t| t.trim().to_lowercase())
                .filter(|t| !t.is_empty());
            Some(Scope { app, title })
        })
        .collect()
}

fn in_scope(scope: &[Scope], app_id: &str, title: &str) -> bool {
    let app = app_id.to_lowercase();
    let title = title.to_lowercase();
    scope.iter().any(|s| {
        s.app == app
            && s.title
                .as_ref()
                .is_none_or(|fragment| title.contains(fragment.as_str()))
    })
}

fn text_arg<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(text) if text.len() <= TEXT_MAX && !text.contains('\0') => {
            Ok(Some(text.as_str()))
        }
        Value::String(_) => Err(invalid(format!(
            "{key} is at most {TEXT_MAX} bytes and has no NUL"
        ))),
        _ => Err(invalid(format!("{key} must be a string"))),
    }
}

fn count_arg(args: &Value, key: &str, default: u64, hard: u64) -> Result<u64> {
    match &args[key] {
        Value::Null => Ok(default),
        value => match value.as_u64() {
            Some(n) if (1..=hard).contains(&n) => Ok(n),
            _ => Err(invalid(format!("{key} is 1 to {hard}"))),
        },
    }
}

fn bool_arg(args: &Value, key: &str) -> Result<bool> {
    match &args[key] {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(*b),
        _ => Err(invalid(format!("{key} must be true or false"))),
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::*;

    fn unsupported() -> Refusal {
        Refusal::new(
            "unsupported_platform",
            "the screen tools run on macOS and Windows only",
        )
    }

    pub fn observe(_: &Settings, _: Origin, _: &Value) -> Result<Value> {
        Err(unsupported())
    }

    pub fn act(_: &Settings, _: Origin, _: &Value) -> Result<Value> {
        Err(unsupported())
    }

    pub fn clipboard_text() -> Result<(Option<String>, bool)> {
        Err(unsupported())
    }

    pub fn permissions_status() -> Value {
        json!({ "platform": "other", "process": crate::config::config().engine_executable, "grants": [] })
    }

    pub fn permissions_request(_: &str) -> Result<Value> {
        Err(unsupported())
    }
}

#[cfg(any(target_os = "macos", windows))]
mod imp {
    use super::*;
    use crate::approvals;
    use crate::sensitive;
    use rightkit_automation::{
        ActOptions, Action, Admission, Admit, Automation, Button, EffectKind, EffectRequest,
        Error as AutoError, Guard, Limits, NativePlatform, Node, Platform, Policy, Target, Tier,
        classify_risk, credential_label, looks_secret, native, redact as redact_payload,
    };
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::{Arc, Mutex, OnceLock};

    type Auto = Automation<NativePlatform>;

    /// Snapshots kept for `screen.act`; the crate keeps the same number.
    const ISSUED_MAX: usize = 8;

    /// What a snapshot was issued for: the identity `screen.act` re-checks.
    struct Issued {
        snapshot: String,
        origin: Origin,
        pid: i32,
        app_id: String,
        app_name: String,
        started: Option<u64>,
        window: u64,
        title: String,
        /// Element id to (role, redacted name).
        nodes: HashMap<String, (String, String)>,
        /// The element that had focus in the snapshot: the target of `type_text` and `keys` when no
        /// element is named.
        focused: Option<String>,
        /// Every element with a frame, to find the one under a click point.
        rects: Vec<NodeRect>,
    }

    /// An element's id, depth in the tree and frame (screen points).
    struct NodeRect {
        id: String,
        depth: usize,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    /// What a press, click or key lands on, for the submit, buy, send and delete check.
    enum Landing<'a> {
        /// An element with its role and (redacted) name.
        Known(&'a str, &'a str),
        /// Nothing could be resolved, or an unlabelled control: the owner is asked.
        Unknown,
    }

    /// Roles that carry text or hold other controls: pressing one with no name is not a button
    /// press, so an unnamed one is not "unknown". Matched against the lower-cased role.
    fn inert_role(role: &str) -> bool {
        let role = role.to_lowercase();
        [
            "text", "static", "group", "scroll", "window", "web", "list", "table", "row", "cell",
            "outline", "split", "tab",
        ]
        .iter()
        .any(|r| role.contains(r))
    }

    /// The element a press or click lands on, from its role and name: unlabelled controls
    /// (icon-only buttons) are `Unknown`.
    fn landing_of(node: Option<&(String, String)>) -> Landing<'_> {
        match node {
            None => Landing::Unknown,
            Some((role, name)) if name.is_empty() && !inert_role(role) => Landing::Unknown,
            Some((role, name)) => Landing::Known(role, name),
        }
    }

    /// The deepest element whose frame holds the point, smallest on a tie.
    fn element_at(issued: &Issued, x: f64, y: f64) -> Option<&(String, String)> {
        issued
            .rects
            .iter()
            .filter(|r| x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h)
            .max_by(|a, b| {
                a.depth
                    .cmp(&b.depth)
                    .then((b.w * b.h).total_cmp(&(a.w * a.h)))
            })
            .and_then(|r| issued.nodes.get(&r.id))
    }

    /// Text typed so far into one element, across calls: a card number typed a digit at a time is
    /// still a card number. Keyed by client, process, window and element.
    struct Typed {
        key: String,
        text: String,
        at: std::time::Instant,
    }

    /// Most characters of one element's typed text kept (the newest), and how long it is kept.
    const TYPED_KEEP: usize = 512;
    const TYPED_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    const TYPED_MAX_ELEMENTS: usize = 32;

    static TYPED: Mutex<Vec<Typed>> = Mutex::new(Vec::new());

    /// Adds `piece` to the text typed into `key` (`replace` starts the text again, as `set_value`
    /// does) and returns the text typed so far.
    fn typed_so_far(key: &str, piece: &str, replace: bool) -> String {
        let mut all = TYPED.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|t| t.at.elapsed() < TYPED_TTL);
        let at = all.iter().position(|t| t.key == key).unwrap_or_else(|| {
            if all.len() >= TYPED_MAX_ELEMENTS {
                all.remove(0);
            }
            all.push(Typed {
                key: key.to_string(),
                text: String::new(),
                at: std::time::Instant::now(),
            });
            all.len() - 1
        });
        let entry = &mut all[at];
        if replace {
            entry.text.clear();
        }
        entry.text.push_str(piece);
        let extra = entry.text.chars().count().saturating_sub(TYPED_KEEP);
        if extra > 0 {
            entry.text = entry.text.chars().skip(extra).collect();
        }
        entry.at = std::time::Instant::now();
        entry.text.clone()
    }

    /// The text typed so far into `key`, without adding to it.
    fn typed_peek(key: &str) -> String {
        let all = TYPED.lock().unwrap_or_else(|e| e.into_inner());
        all.iter()
            .find(|t| t.key == key && t.at.elapsed() < TYPED_TTL)
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    struct State {
        auto: Auto,
        issued: VecDeque<Issued>,
    }

    /// The admission hook every effect passes after the crate's own gates: no launch, no
    /// terminate, no text delivery to another app, and input never above the targeted tier.
    struct Gate;

    impl Admission for Gate {
        fn approve(&self, req: &EffectRequest) -> Admit {
            match req.kind {
                EffectKind::Launch | EffectKind::Terminate | EffectKind::Deliver => {
                    Admit::Deny("not offered to remote callers".into())
                }
                EffectKind::Act => Admit::AllowUpTo(Tier::Targeted),
                EffectKind::Observe | EffectKind::Capture => Admit::Allow,
            }
        }
    }

    fn state() -> &'static Mutex<State> {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        STATE.get_or_init(|| {
            let auto = native(Policy::new(Vec::<String>::new()));
            auto.policy().protect_pid(std::process::id() as i32);
            auto.set_admission(Arc::new(Gate));
            Mutex::new(State {
                auto,
                issued: VecDeque::new(),
            })
        })
    }

    fn lock() -> std::sync::MutexGuard<'static, State> {
        state().lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True when no remote tool may touch this app: the hard floor or the owner's blocklist, by
    /// app id or, for a path entry, by the program's path.
    fn blocked(settings: &Settings, app: &rightkit_automation::AppInfo) -> bool {
        hard_floor(&app.app_id)
            || settings.app_blocked(&app.app_id)
            || app
                .path
                .as_deref()
                .is_some_and(|p| hard_floor(p) || settings.app_blocked(p))
    }

    /// The crate's policy is an allowlist only, so the allowlist is set again on every call to
    /// every running app that is not blocked (read from the settings owner app last published):
    /// a change to the blocklist, or an app that was just opened, applies at once. The crate's own
    /// never-drivable list and the protected engine process still win over it.
    fn sync_policy(auto: &Auto, settings: &Settings) {
        let want: HashSet<String> = auto
            .apps()
            .iter()
            .filter(|a| !blocked(settings, a))
            .map(|a| a.app_id.to_lowercase())
            .collect();
        let policy = auto.policy();
        for app in policy.allowed() {
            if !want.contains(&app) {
                policy.revoke(&app);
            }
        }
        for app in &want {
            policy.allow(app);
        }
    }

    pub fn clipboard_text() -> Result<(Option<String>, bool)> {
        match lock().auto.platform().clip_snapshot() {
            Some(snapshot) => Ok((snapshot.text, snapshot.lossless)),
            None => Err(Refusal::new(
                "read_failed",
                "the clipboard could not be read",
            )),
        }
    }

    fn grant_row(id: &str, title: &str, status: &str, why: &str) -> Value {
        json!({ "id": id, "title": title, "status": status, "why": why })
    }

    /// The grants of this process. Windows UI Automation and capture need no OS grant.
    pub fn permissions_status() -> Value {
        let (platform, grants) = if cfg!(target_os = "macos") {
            let status = lock().auto.status();
            let word = |on: bool| if on { "granted" } else { "needsApproval" };
            (
                "macos",
                vec![
                    grant_row(
                        "accessibility",
                        "Accessibility (always-on agent)",
                        word(status.accessibility),
                        "Lets the always-on agent read and drive app windows (except the apps you block). macOS grants it to the engine, not to the notch.",
                    ),
                    grant_row(
                        "screenRecording",
                        "Screen Recording (always-on agent)",
                        word(status.screen_capture),
                        "Lets the always-on agent take a screenshot of an app window. Granted to the engine, not to the notch.",
                    ),
                    grant_row(
                        "automation",
                        "Automation (always-on agent)",
                        word(apple_events::granted()),
                        "Lets the always-on agent script System Events and Finder (press keys and menu items in them). macOS asks once per app, for the engine, not the notch.",
                    ),
                ],
            )
        } else {
            (
                "windows",
                vec![grant_row(
                    "accessibility",
                    "UI Automation",
                    "notNeeded",
                    "Windows needs no OS permission for the screen tools; your block list and your approvals for sending, buying and publishing are the limit.",
                )],
            )
        };
        json!({ "platform": platform, "process": crate::config::config().engine_executable, "grants": grants })
    }

    /// Shows the OS prompt for one grant of this process, then reads the status back. The prompt
    /// is asynchronous: the answer may still read `needsApproval` until the user has chosen.
    pub fn permissions_request(grant: &str) -> Result<Value> {
        if !matches!(grant, "accessibility" | "screenRecording" | "automation") {
            return Err(invalid(
                "grant must be accessibility, screenRecording or automation",
            ));
        }
        #[cfg(target_os = "macos")]
        {
            match grant {
                "accessibility" => request::accessibility(),
                "automation" => apple_events::request(),
                _ => request::screen_recording(),
            }
        }
        Ok(permissions_status())
    }

    #[cfg(target_os = "macos")]
    mod request {
        use std::ffi::c_void;

        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
            static kAXTrustedCheckOptionPrompt: *const c_void;
        }
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGRequestScreenCaptureAccess() -> bool;
        }
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            static kCFBooleanTrue: *const c_void;
            static kCFTypeDictionaryKeyCallBacks: u8;
            static kCFTypeDictionaryValueCallBacks: u8;
            fn CFDictionaryCreate(
                allocator: *const c_void,
                keys: *const *const c_void,
                values: *const *const c_void,
                count: isize,
                key_callbacks: *const u8,
                value_callbacks: *const u8,
            ) -> *const c_void;
            fn CFRelease(object: *const c_void);
        }

        /// `AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true})`.
        pub fn accessibility() {
            // SAFETY: the keys and values are CoreFoundation constants that outlive the call, the
            // dictionary is created and released here, and the callbacks are the system's own.
            unsafe {
                let keys = [kAXTrustedCheckOptionPrompt];
                let values = [kCFBooleanTrue];
                let options = CFDictionaryCreate(
                    std::ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    1,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                );
                AXIsProcessTrustedWithOptions(options);
                if !options.is_null() {
                    CFRelease(options);
                }
            }
        }

        pub fn screen_recording() {
            // SAFETY: a plain system call with no arguments.
            unsafe {
                CGRequestScreenCaptureAccess();
            }
        }
    }

    /// The Apple Events (Automation) grants of this process: the always-on agent scripts System
    /// Events and Finder. macOS keeps them per process, so the engine's grant is the one that counts.
    /// Windows has no such grant.
    mod apple_events {
        /// Target apps the agent scripts; both are always running on macOS.
        #[cfg(target_os = "macos")]
        const TARGETS: [&str; 2] = ["com.apple.systemevents", "com.apple.finder"];

        #[cfg(target_os = "macos")]
        mod ffi {
            use std::ffi::c_void;

            #[repr(C)]
            pub struct AEDesc {
                pub descriptor_type: u32,
                pub data_handle: *mut c_void,
            }

            #[link(name = "CoreServices", kind = "framework")]
            unsafe extern "C" {
                pub fn AECreateDesc(
                    type_code: u32,
                    data: *const c_void,
                    size: isize,
                    result: *mut AEDesc,
                ) -> i16;
                pub fn AEDisposeDesc(desc: *mut AEDesc) -> i16;
                pub fn AEDeterminePermissionToAutomateTarget(
                    target: *const AEDesc,
                    event_class: u32,
                    event_id: u32,
                    ask_user: u8,
                ) -> i32;
            }
        }

        /// The OSStatus for one target: 0 when granted. With `ask` false it never prompts, and an
        /// undetermined answer is -1744 (errAEEventWouldRequireUserConsent).
        #[cfg(target_os = "macos")]
        fn determine(bundle: &str, ask: bool) -> i32 {
            const TYPE_BUNDLE_ID: u32 = 0x6275_6E64; // 'bund'
            const TYPE_WILDCARD: u32 = 0x2A2A_2A2A; // '****'
            let mut desc = ffi::AEDesc {
                descriptor_type: 0,
                data_handle: std::ptr::null_mut(),
            };
            // SAFETY: the bundle id bytes outlive the call, the descriptor is created and disposed
            // here, and both calls are the system's own Apple Events functions.
            unsafe {
                if ffi::AECreateDesc(
                    TYPE_BUNDLE_ID,
                    bundle.as_ptr().cast(),
                    bundle.len() as isize,
                    &raw mut desc,
                ) != 0
                {
                    return -1;
                }
                let status = ffi::AEDeterminePermissionToAutomateTarget(
                    &raw const desc,
                    TYPE_WILDCARD,
                    TYPE_WILDCARD,
                    u8::from(ask),
                );
                ffi::AEDisposeDesc(&raw mut desc);
                status
            }
        }

        /// Granted only when every target answers noErr. Never prompts.
        #[cfg(target_os = "macos")]
        pub fn granted() -> bool {
            TARGETS.iter().all(|bundle| determine(bundle, false) == 0)
        }

        #[cfg(not(target_os = "macos"))]
        pub fn granted() -> bool {
            true
        }

        /// Asks for each target in turn on its own thread: the macOS prompt waits for the owner's
        /// answer, so the request returns at once and the status is read back later.
        #[cfg(target_os = "macos")]
        pub fn request() {
            std::thread::spawn(|| {
                for bundle in TARGETS {
                    determine(bundle, true);
                }
            });
        }
    }

    /// The OS grants, checked and never requested. Screen Recording only when a capture is asked;
    /// Automation only when the call sends Apple Events (`screen.act`).
    fn permission(auto: &Auto, capture: bool, apple: bool) -> Result<()> {
        let status = auto.status();
        let mut missing = Vec::new();
        if !status.accessibility {
            missing.push("Accessibility");
        }
        if capture && !status.screen_capture {
            missing.push("Screen Recording");
        }
        if apple && !apple_events::granted() {
            missing.push("Automation (System Events and Finder)");
        }
        if missing.is_empty() {
            return Ok(());
        }
        Err(Refusal::new(
            "permission_missing",
            format!(
                "The app does not have the {} permission on this computer; grant it in System Settings (Privacy & Security). A remote call never asks for it; the owner presses Allow in the app's Permissions.",
                missing.join(" and ")
            ),
        ))
    }

    struct Win {
        id: u64,
        pid: i32,
        app_id: String,
        app_name: String,
        started: Option<u64>,
        title: String,
        bounds: rightkit_automation::Rect,
    }

    /// The on-screen, ordinary windows of apps that are not blocked.
    fn approved_windows(auto: &Auto, settings: &Settings) -> Vec<Win> {
        let apps: HashMap<i32, rightkit_automation::AppInfo> =
            auto.apps().into_iter().map(|a| (a.pid, a)).collect();
        auto.windows(true)
            .into_iter()
            .filter(|w| w.layer == 0 && w.bounds.w > 50.0 && w.bounds.h > 50.0)
            .filter_map(|w| {
                let app = apps.get(&w.pid)?;
                (!blocked(settings, app)).then(|| Win {
                    id: w.window_id,
                    pid: w.pid,
                    app_id: app.app_id.clone(),
                    app_name: app.name.clone(),
                    started: app.started,
                    title: w.title,
                    bounds: w.bounds,
                })
            })
            .collect()
    }

    fn clip(text: &str) -> String {
        text.chars().take(VALUE_MAX).collect()
    }

    /// Text that may leave this computer: the common tokens and keys masked.
    fn masked(text: &str) -> String {
        clip(&super::super::redact::redact(text).0)
    }

    fn node_json(
        node: &Node,
        nodes: &mut HashMap<String, (String, String)>,
        rects: &mut Vec<NodeRect>,
        depth: usize,
        count: &mut u64,
    ) -> Value {
        *count += 1;
        if let Some(f) = node.frame {
            rects.push(NodeRect {
                id: node.id.clone(),
                depth,
                x: f.x,
                y: f.y,
                w: f.w,
                h: f.h,
            });
        }
        let label = node
            .name
            .as_deref()
            .or(node.description.as_deref())
            .or(node.identifier.as_deref())
            .unwrap_or("");
        let credential = node.secure
            || credential_label(label)
            || node.identifier.as_deref().is_some_and(credential_label)
            || node.description.as_deref().is_some_and(credential_label);
        let name = masked(label);
        nodes.insert(node.id.clone(), (node.role.clone(), name.clone()));
        let value = node.value.as_deref().map(|v| {
            if credential || looks_secret(v) {
                "<withheld>".to_string()
            } else {
                masked(v)
            }
        });
        let mut out = json!({"id": node.id, "role": node.role});
        let fields = out.as_object_mut().expect("an object");
        if !name.is_empty() {
            fields.insert("name".into(), json!(name));
        }
        if let Some(value) = value {
            fields.insert("value".into(), json!(value));
        }
        if node.secure {
            fields.insert("secure".into(), json!(true));
        }
        if node.enabled == Some(false) {
            fields.insert("enabled".into(), json!(false));
        }
        if let Some(f) = node.frame {
            fields.insert(
                "frame".into(),
                json!({"x": f.x, "y": f.y, "w": f.w, "h": f.h}),
            );
        }
        if !node.caps.is_empty() {
            fields.insert("caps".into(), json!(node.caps));
        }
        if !node.children.is_empty() {
            let children: Vec<Value> = node
                .children
                .iter()
                .map(|c| node_json(c, nodes, rects, depth + 1, count))
                .collect();
            fields.insert("children".into(), Value::Array(children));
        }
        out
    }

    pub fn observe(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
        let window = match &args["window"] {
            Value::Null => None,
            value => Some(
                value
                    .as_u64()
                    .filter(|w| *w >= 1)
                    .ok_or_else(|| invalid("window is a window id from the list"))?,
            ),
        };
        let screenshot = bool_arg(args, "screenshot")?;
        if screenshot && window.is_none() {
            return Err(invalid("screenshot needs a window"));
        }
        let max_nodes = count_arg(args, "maxNodes", DEFAULT_NODES, HARD_NODES)? as usize;
        let max_depth = count_arg(args, "maxDepth", DEFAULT_DEPTH, HARD_DEPTH)? as usize;

        let mut state = lock();
        sync_policy(&state.auto, settings);
        permission(&state.auto, screenshot, false)?;
        let windows = approved_windows(&state.auto, settings);

        let Some(window) = window else {
            let rows: Vec<Value> = windows
                .iter()
                .map(|w| {
                    json!({
                        "window": w.id, "app": w.app_id, "appName": masked(&w.app_name),
                        "title": masked(&w.title),
                        "bounds": {"x": w.bounds.x, "y": w.bounds.y, "w": w.bounds.w, "h": w.bounds.h},
                    })
                })
                .collect();
            return Ok(json!({"windows": rows, "returned": rows.len()}));
        };
        let win = windows.iter().find(|w| w.id == window).ok_or_else(|| {
            Refusal::new(
                "window_not_approved",
                "that window is not on screen, or its app is blocked",
            )
        })?;
        let snapshot = state
            .auto
            .snapshot(
                Target::window(win.pid, win.id),
                Limits {
                    max_depth,
                    max_nodes,
                },
            )
            .map_err(auto_refusal)?;
        let mut nodes = HashMap::new();
        let mut rects = Vec::new();
        let mut count = 0u64;
        let tree = node_json(&snapshot.root, &mut nodes, &mut rects, 0, &mut count);
        let mut answer = json!({
            "snapshot": snapshot.id,
            "window": win.id, "app": win.app_id, "title": masked(&win.title),
            "truncated": snapshot.truncated,
            "root": tree,
            "returned": count,
        });
        if let Some(focused) = &snapshot.focused {
            answer["focused"] = json!(focused);
        }
        state.issued.push_back(Issued {
            snapshot: snapshot.id.clone(),
            origin,
            pid: win.pid,
            app_id: win.app_id.clone(),
            app_name: masked(&win.app_name),
            started: win.started,
            window: win.id,
            title: win.title.clone(),
            nodes,
            focused: snapshot.focused.clone(),
            rects,
        });
        while state.issued.len() > ISSUED_MAX {
            state.issued.pop_front();
        }
        if screenshot {
            let (png, report) = capture(&state.auto, win, settings.image_max_bytes)?;
            answer["bytes"] = json!(png.len());
            answer["screenshot"] = json!({
                "mime": "image/png", "width": report.width, "height": report.height,
                "masked": report.masked, "base64": super::super::b64::encode(&png),
            });
        }
        Ok(answer)
    }

    /// A redacted PNG of one approved window, through a private temporary file that is always
    /// removed.
    fn capture(
        auto: &Auto,
        win: &Win,
        cap: u64,
    ) -> Result<(Vec<u8>, rightkit_automation::ShotReport)> {
        let dir = rightkit_app_host::state_directory()
            .map_err(|_| Refusal::new("read_failed", "the app's state folder is not available"))?;
        let _ = rightkit_fs::ensure_private_dir(&dir);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!("screen-capture-{}-{nanos}.png", std::process::id()));
        let report = auto.screenshot_report(&Target::window(win.pid, win.id), &path);
        let bytes = match &report {
            Ok(_) => read_capped(&path, cap),
            Err(_) => Ok(Vec::new()),
        };
        let _ = std::fs::remove_file(&path);
        let report = report.map_err(auto_refusal)?;
        Ok((bytes?, report))
    }

    fn read_capped(path: &std::path::Path, cap: u64) -> Result<Vec<u8>> {
        use std::io::Read;
        let failed = || Refusal::new("read_failed", "the capture could not be read back");
        let file = std::fs::File::open(path).map_err(|_| failed())?;
        let mut bytes = Vec::new();
        file.take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failed())?;
        if bytes.len() as u64 > cap {
            return Err(Refusal::new(
                "too_large",
                "the screenshot is larger than the image cap in the app's settings (remoteImageMaxBytes)",
            ));
        }
        Ok(bytes)
    }

    /// The action as the crate takes it, plus whether it needs an element.
    fn parse_action(spec: &Value) -> Result<(Action, bool)> {
        let kind = spec["type"]
            .as_str()
            .ok_or_else(|| invalid("action.type is needed"))?;
        let text = |key: &str| -> Result<String> {
            text_arg(spec, key)?
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .ok_or_else(|| invalid(format!("action.{key} is needed")))
        };
        let number = |key: &str| -> Result<Option<f64>> {
            match &spec[key] {
                Value::Null => Ok(None),
                value => value
                    .as_f64()
                    .filter(|n| n.is_finite() && n.abs() <= 1.0e6)
                    .map(Some)
                    .ok_or_else(|| invalid(format!("action.{key} must be a number"))),
            }
        };
        Ok(match kind {
            "press" => (Action::Press, true),
            "focus" => (Action::Focus, true),
            "set_value" => (Action::SetValue(text("text")?), true),
            "type_text" => (Action::TypeText(text("text")?), false),
            "keys" => (Action::Keys(text("keys")?), false),
            "scroll" => (
                Action::Scroll {
                    dx: number("dx")?.unwrap_or(0.0),
                    dy: number("dy")?.unwrap_or(0.0),
                },
                true,
            ),
            "click" => {
                let button = match spec["button"].as_str() {
                    None | Some("left") => Button::Left,
                    Some("right") => Button::Right,
                    Some(_) => return Err(invalid("action.button is left or right")),
                };
                let count = match &spec["count"] {
                    Value::Null => 1,
                    value => value
                        .as_u64()
                        .filter(|n| (1..=3).contains(n))
                        .ok_or_else(|| invalid("action.count is 1 to 3"))?
                        as u32,
                };
                let point = match (number("x")?, number("y")?) {
                    (Some(x), Some(y)) => Some((x, y)),
                    (None, None) => None,
                    _ => return Err(invalid("action.x and action.y go together")),
                };
                (
                    Action::Click {
                        point,
                        button,
                        count,
                    },
                    point.is_none(),
                )
            }
            "menu_press" => {
                let items = spec["path"]
                    .as_array()
                    .filter(|p| (1..=PATH_ITEMS_MAX).contains(&p.len()))
                    .ok_or_else(|| {
                        invalid(format!(
                            "action.path is a list of 1 to {PATH_ITEMS_MAX} menu names"
                        ))
                    })?;
                let path: Option<Vec<String>> = items
                    .iter()
                    .map(|i| {
                        i.as_str()
                            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.contains('\0'))
                            .map(str::to_string)
                    })
                    .collect();
                (
                    Action::MenuPress(path.ok_or_else(|| invalid("action.path holds menu names"))?),
                    false,
                )
            }
            _ => {
                return Err(invalid(
                    "action.type is press, set_value, type_text, keys, click, scroll, focus or menu_press",
                ));
            }
        })
    }

    /// What the user is asked to approve, in words: the action and its payload (masked when it
    /// looks secret), the element and the window.
    fn describe(action: &Action, element: Option<&(String, String)>) -> String {
        let payload = |text: &str| redact_payload(text, false, true);
        let what = match action {
            Action::Press => "press".to_string(),
            Action::Focus => "focus".to_string(),
            Action::SetValue(t) => format!("set the value to \"{}\"", payload(t)),
            Action::TypeText(t) => format!("type \"{}\"", payload(t)),
            Action::Keys(k) => format!("press keys \"{}\"", payload(k)),
            Action::Click { point, count, .. } => match point {
                Some((x, y)) => format!("click {count}x at {x:.0},{y:.0}"),
                None => format!("click {count}x"),
            },
            Action::Scroll { dx, dy } => format!("scroll {dx:.0},{dy:.0}"),
            Action::MenuPress(path) => format!("choose menu {}", path.join(" > ")),
        };
        match element {
            Some((role, name)) if name.is_empty() => format!("{what} on a {role}"),
            Some((role, name)) => format!("{what} on {role} \"{name}\""),
            None => what,
        }
    }

    /// `screen.act` with `{type: "handover"}`: gives the region of one element to the user (spot
    /// takeover, #355). It is checked as every act is (same snapshot, same process, window and
    /// title, app not blocked) and always asked, for every door, through the same approval: the
    /// approval names the app, the window and the control and says the rest of the screen is
    /// dimmed. The second call (with `approval`) shows the region and waits for Done, Cancel or
    /// the time limit; the answer is a hand back, never a claim that the page changed as hoped.
    fn handover(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
        let snapshot = text_arg(args, "snapshot")?
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("snapshot is needed"))?;
        let element = text_arg(args, "element")?
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("a handover needs the element to show the user"))?;
        let approval = text_arg(args, "approval")?;
        let secs = count_arg(&args["action"], "seconds", 180, 600)?.max(10);
        if !settings.spot_takeover {
            return Err(Refusal::new(
                "takeover_off",
                "handing the user part of the screen is turned off in the app's settings (remoteScreenSpotTakeover)",
            ));
        }

        let state = lock();
        sync_policy(&state.auto, settings);
        permission(&state.auto, false, false)?;
        let issued = state
            .issued
            .iter()
            .find(|i| i.snapshot == snapshot && i.origin == origin)
            .ok_or_else(|| {
                Refusal::new(
                    "unknown_snapshot",
                    "no such snapshot for this caller; call screen.observe again",
                )
            })?;
        let (role, name) = issued.nodes.get(element).cloned().ok_or_else(|| {
            Refusal::new("unknown_snapshot", "that element is not in the snapshot")
        })?;
        let changed = |why: &str| {
            Refusal::new(
                "target_changed",
                format!("{why}; nothing was done, call screen.observe again"),
            )
        };
        let app = state
            .auto
            .platform()
            .app(issued.pid)
            .filter(|a| a.app_id == issued.app_id && a.started == issued.started)
            .ok_or_else(|| changed("the app is no longer the one that was observed"))?;
        let window = state
            .auto
            .windows(false)
            .into_iter()
            .find(|w| w.window_id == issued.window && w.pid == issued.pid)
            .ok_or_else(|| changed("the window is gone"))?;
        if window.title != issued.title {
            return Err(changed("the window title changed"));
        }
        if blocked(settings, &app) {
            return Err(Refusal::new(
                "window_not_approved",
                "that app is blocked in the app's settings (remoteBlockedApps)",
            ));
        }
        // The region: the element's frame cut to its window. No frame, or nothing left on screen,
        // cannot be shown, so it is refused before the user is asked.
        let frame = issued
            .rects
            .iter()
            .find(|r| r.id == element)
            .ok_or_else(|| invalid("that element has no position on screen"))?;
        let b = window.bounds;
        let (x0, y0) = (frame.x.max(b.x), frame.y.max(b.y));
        let (x1, y1) = (
            (frame.x + frame.w).min(b.x + b.w),
            (frame.y + frame.h).min(b.y + b.h),
        );
        if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
            return Err(invalid("that element is not visible in its window"));
        }
        let rect = [x0, y0, x1 - x0, y1 - y0];

        let shown = if name.is_empty() {
            format!("a {role}")
        } else {
            format!("{role} \"{name}\"")
        };
        let summary = format!(
            "hand control to you: show only {shown} in {} window \"{}\" and dim the rest of the screen, for up to {} min. The app reads and captures nothing meanwhile; you act there, then press Done",
            issued.app_name,
            masked(&window.title),
            secs.div_ceil(60),
        );
        let started = issued.started.unwrap_or(0).to_be_bytes();
        let pid = issued.pid.to_be_bytes();
        let window_id = issued.window.to_be_bytes();
        let bind = approvals::bind_of(&[
            b"screen.act",
            origin.as_str().as_bytes(),
            issued.app_id.as_bytes(),
            &pid,
            &started,
            &window_id,
            issued.title.as_bytes(),
            issued.snapshot.as_bytes(),
            element.as_bytes(),
            role.as_bytes(),
            name.as_bytes(),
            b"Handover",
            &secs.to_be_bytes(),
        ]);
        match approval.filter(|a| !a.is_empty()) {
            None => {
                return Err(approvals::request_act_held(
                    origin,
                    "screen.act",
                    &bind,
                    &summary,
                ));
            }
            Some(id) => approvals::take_act_held(origin, "screen.act", &bind, id)?,
        }
        let app_name = issued.app_name.clone();
        // The target the takeover is bound to, kept to watch it while the user holds the screen.
        let (target_pid, target_window, target_title) =
            (issued.pid, issued.window, issued.title.clone());
        let target_bounds = (
            window.bounds.x,
            window.bounds.y,
            window.bounds.w,
            window.bounds.h,
        );
        // The wait can take minutes: other screen calls are refused while it runs (`spot_run`),
        // so the shared state is released first.
        drop(state);
        let moved = move || {
            let state = lock();
            match state
                .auto
                .windows(false)
                .into_iter()
                .find(|w| w.window_id == target_window && w.pid == target_pid)
            {
                None => true,
                Some(w) => {
                    (w.bounds.x, w.bounds.y, w.bounds.w, w.bounds.h) != target_bounds
                        || w.title != target_title
                }
            }
        };
        let outcome = spot_run(
            shown,
            app_name,
            rect,
            (target_window.to_string(), target_pid.max(0) as u32),
            secs,
            &moved,
        )?;
        Ok(json!({
            "done": outcome == "done",
            "result": "handed_back",
            "outcome": outcome,
            "note": if outcome == "target_moved" {
                "the window moved, resized, changed its title or closed, so the handover ended; nothing was read or captured meanwhile, call screen.observe again and ask again"
            } else {
                "the user acted in the region; nothing was read or captured meanwhile, call screen.observe again before relying on the page"
            },
        }))
    }

    pub fn act(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
        if args["action"]["type"].as_str() == Some("handover") {
            return handover(settings, origin, args);
        }
        let snapshot = text_arg(args, "snapshot")?
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("snapshot is needed"))?;
        let element = text_arg(args, "element")?.filter(|s| !s.is_empty());
        let approval = text_arg(args, "approval")?;
        let (spec, from_profile) = resolve_profile(settings, &args["action"])?;
        let (action, needs_element) = parse_action(&spec)?;
        if needs_element && element.is_none() {
            return Err(invalid("this action needs an element from the snapshot"));
        }

        let state = lock();
        sync_policy(&state.auto, settings);
        permission(&state.auto, false, true)?;

        let issued = state
            .issued
            .iter()
            .find(|i| i.snapshot == snapshot && i.origin == origin)
            .ok_or_else(|| {
                Refusal::new(
                    "unknown_snapshot",
                    "no such snapshot for this caller; call screen.observe again",
                )
            })?;
        let node = match element {
            Some(id) => Some(issued.nodes.get(id).ok_or_else(|| {
                Refusal::new("unknown_snapshot", "that element is not in the snapshot")
            })?),
            None => None,
        };

        // The target must still be the thing the snapshot saw: same process (id and start
        // time), same window with the same title, and its app still not blocked.
        let changed = |why: &str| {
            Refusal::new(
                "target_changed",
                format!("{why}; nothing was done, call screen.observe again"),
            )
        };
        let app = state
            .auto
            .platform()
            .app(issued.pid)
            .filter(|a| a.app_id == issued.app_id && a.started == issued.started)
            .ok_or_else(|| changed("the app is no longer the one that was observed"))?;
        let window = state
            .auto
            .windows(false)
            .into_iter()
            .find(|w| w.window_id == issued.window && w.pid == issued.pid)
            .ok_or_else(|| changed("the window is gone"))?;
        if window.title != issued.title {
            return Err(changed("the window title changed"));
        }
        if blocked(settings, &app) {
            return Err(Refusal::new(
                "window_not_approved",
                "that app is blocked in the app's settings (remoteBlockedApps)",
            ));
        }
        if let Action::Click {
            point: Some((x, y)),
            ..
        } = &action
            && !window.bounds.contains(*x, *y)
        {
            return Err(invalid("the click point is outside the window"));
        }

        // The element that typing and keys land in: the one the call names, else the one that had
        // focus in the snapshot. (`set_value` always names its element.)
        let focus_node = issued.focused.as_ref().and_then(|id| issued.nodes.get(id));
        let typing_node = node.or(focus_node);

        // Keys that move (Tab, arrows, Home, End) and type in one call can assemble digits out of
        // order; navigation and typing go in separate calls.
        if let Action::Keys(k) = &action
            && sensitive::keys_navigate_and_type(k)
        {
            return Err(invalid(
                "keys that move the caret or focus (Tab, arrows, Home, End) and keys that type must be sent in separate calls",
            ));
        }

        // Rule 1: never type a password, card number, bank detail or government ID. The field
        // label is checked on the element the text goes into, and the value on everything typed
        // into that element so far (a card number typed a digit at a time across calls is one
        // card number), with separators, non-ASCII digits and multi-character key chords folded.
        let target_label: Option<&str> = match &action {
            Action::SetValue(_) => node.map(|(_, name)| name.as_str()),
            Action::TypeText(_) | Action::Keys(_) => typing_node.map(|(_, name)| name.as_str()),
            _ => None,
        };
        let typed: Option<String> = match &action {
            Action::SetValue(t) | Action::TypeText(t) => Some(t.clone()),
            Action::Keys(k) => Some(sensitive::keys_typed(k)),
            _ => None,
        };
        if let Some(text) = &typed {
            let label_hit =
                target_label.is_some_and(|l| sensitive::sensitive_label(l) || credential_label(l));
            let replace = matches!(action, Action::SetValue(_));
            let target_key = format!(
                "{}|{}|{}|{}|{}",
                approvals::current_client(),
                issued.pid,
                issued.started.unwrap_or(0),
                issued.window,
                element.or(issued.focused.as_deref()).unwrap_or("focus"),
            );
            let so_far = if replace {
                text.clone()
            } else {
                format!("{}{}", typed_peek(&target_key), text)
            };
            if label_hit
                || sensitive::sensitive_value(text).is_some()
                || sensitive::sensitive_value(&so_far).is_some()
            {
                return Err(sensitive_input());
            }
            typed_so_far(&target_key, text, replace);
        }

        // What a press, click or key lands on, for the risk and submit checks. A click at a point
        // lands on the deepest element under it; keys land on the focused or named element. An
        // unlabelled control, or an element that cannot be resolved, is `Unknown` and asks.
        let landing: Option<Landing> = match &action {
            Action::Press => Some(landing_of(node)),
            Action::Click {
                point: Some((x, y)),
                ..
            } => Some(landing_of(element_at(issued, *x, *y))),
            Action::Click { point: None, .. } => Some(landing_of(node)),
            // Space presses a focused button, as does Return (asked anyway below). With the focus
            // unknown it asks; on a text field it is ordinary typing.
            Action::Keys(k) if sensitive::keys_space(k) => Some(landing_of(typing_node)),
            // Typed text with a space in it presses a focused button the same way; it asks only
            // when the focus is a known button that sends, buys or deletes.
            Action::TypeText(t) if t.contains(' ') => {
                typing_node.map(|(r, n)| Landing::Known(r, n))
            }
            _ => None,
        };
        let landed_name: Option<&str> = match &landing {
            Some(Landing::Known(_, name)) => Some(*name),
            _ => None,
        };
        // A label that reads as a risky act is named to the user and confirmed by the approval.
        let risk = match (&action, landed_name, node) {
            (Action::MenuPress(path), _, _) => path.last().and_then(|l| classify_risk(l)),
            (_, Some(name), _) => classify_risk(name),
            (_, None, Some((_, name))) => classify_risk(name),
            _ => None,
        };
        // Rule 3: a control that sends, buys, pays or publishes (or a typed newline or Return)
        // always asks; no grant covers it. So does a press, click or Space whose target is an
        // unlabelled control or cannot be identified.
        let unidentified = matches!(landing, Some(Landing::Unknown));
        let button_like = |role: &str| {
            let role = role.to_lowercase();
            ["button", "menuitem", "link", "checkbox", "radio", "switch"]
                .iter()
                .any(|r| role.contains(r))
        };
        let submits = match (&action, &landing) {
            (Action::MenuPress(path), _) => path.last().is_some_and(|l| sensitive::submit_label(l)),
            (Action::Keys(k), _) if sensitive::keys_submit(k) => true,
            (Action::TypeText(t), _) if t.contains(['\n', '\r']) => true,
            // Space or text with a space on a focused control that is a button of that kind.
            (Action::Keys(_) | Action::TypeText(_), Some(Landing::Known(role, name))) => {
                button_like(role)
                    && (sensitive::submit_label(name) || classify_risk(name).is_some())
            }
            (_, Some(Landing::Known(_, name))) => sensitive::submit_label(name),
            (_, Some(Landing::Unknown)) => true,
            _ => false,
        };
        let mut summary = format!(
            "{} in {} window \"{}\"",
            describe(&action, node),
            issued.app_name,
            masked(&window.title)
        );
        if let Some(class) = risk {
            summary.push_str(&format!(" (risky: {})", class.label()));
        }
        if unidentified {
            summary.push_str(
                " (submits: The app could not tell what this control is, so it may send, buy or delete)",
            );
        } else if submits {
            summary.push_str(" (submits: this may send, buy or publish)");
        }

        // Rule 2: the user's own details. The gateway needs a standing grant for the app before
        // those are typed without a per-action question; the local agent and the owner are never
        // asked per action, so for them the profile is typed like any other non-sensitive text.
        // Everything else the gateway does, and every submit, is asked below.
        let local = origin == Origin::Local;
        let fills = matches!(action, Action::SetValue(_) | Action::TypeText(_));
        let mut standing = false;
        if fills && !submits && from_profile && settings.form_fill && !local {
            let listed = parse_scope(&settings.form_fill_standing)
                .iter()
                .any(|s| in_scope(std::slice::from_ref(s), &app.app_id, &window.title));
            if listed {
                standing = true;
            } else {
                let family = fill_family(&app.app_id.to_lowercase());
                if approvals::session_active_held(origin, &family)? {
                    standing = true;
                } else {
                    return Err(approvals::request_session_held(
                        origin,
                        &family,
                        &format!(
                            "allow {} to type your name, email, phone and address into forms in {} windows, now and whenever it asks, until you stop using it for 30 days. It never types passwords, card numbers, bank details or government IDs, and sending, buying or publishing still asks every time",
                            origin.as_str(),
                            issued.app_name,
                        ),
                    ));
                }
            }
        }
        // The local agent and the owner are asked only for what submits, and for a delete.
        let deletes = matches!(risk, Some(rightkit_automation::RiskClass::Delete));
        let unasked = standing || (local && !submits && !deletes);

        let origin_name = origin.as_str();
        let started = issued.started.unwrap_or(0).to_be_bytes();
        let pid = issued.pid.to_be_bytes();
        let window_id = issued.window.to_be_bytes();
        let (role, name) = node.cloned().unwrap_or_default();
        let canonical = format!("{action:?}");
        let bind = approvals::bind_of(&[
            b"screen.act",
            origin_name.as_bytes(),
            issued.app_id.as_bytes(),
            &pid,
            &started,
            &window_id,
            issued.title.as_bytes(),
            issued.snapshot.as_bytes(),
            element.unwrap_or("").as_bytes(),
            role.as_bytes(),
            name.as_bytes(),
            canonical.as_bytes(),
        ]);
        match approval.filter(|a| !a.is_empty()) {
            _ if unasked => {}
            None => {
                return Err(approvals::request_act_held(
                    origin,
                    "screen.act",
                    &bind,
                    &summary,
                ));
            }
            Some(id) => approvals::take_act_held(origin, "screen.act", &bind, id)?,
        }

        let opts = ActOptions {
            min_tier: Tier::Semantic,
            max_tier: Tier::Targeted,
            guard: Guard::Structure,
            confirm: risk.into_iter().collect(),
            strict_focus: true,
            log_values: false,
            settle_ms: 150,
            fence: None,
        };
        // Digits typed into a field can land in an order the pieces do not show (Home or an arrow
        // between them), so after the act the field's own value is read back and tested.
        // BackSpace and Delete can leave digits that now read as one, so they are read back too.
        let readback = typed.as_deref().is_some_and(sensitive::has_digit)
            || matches!(&action, Action::Keys(k) if sensitive::keys_delete(k));
        let outcome = state
            .auto
            .act(snapshot, element, action, &opts)
            .map_err(auto_refusal)?;
        if outcome.fresh.is_some() {
            return Err(changed("the window's structure changed since the snapshot"));
        }
        let done = outcome.done();
        if done
            && readback
            && let Some(refusal) = sensitive_after(&state.auto, issued)
        {
            return Err(refusal);
        }
        let receipt = outcome.receipt;
        let tier = receipt.tier_used.map(|t| t.label());
        Ok(json!({
            "done": done,
            "result": receipt.result.label(),
            "tier": tier,
            "focusMoved": receipt.focus_moved,
            "attempts": receipt.attempts.iter()
                .map(|a| json!({"tier": a.tier.label(), "ok": a.ok}))
                .collect::<Vec<_>>(),
        }))
    }

    fn find_node<'n>(node: &'n Node, id: &str) -> Option<&'n Node> {
        if node.id == id {
            return Some(node);
        }
        node.children.iter().find_map(|c| find_node(c, id))
    }

    /// Reads the focused field of the window back after digits were typed. When its value is now a
    /// card number, bank detail or government ID, clears the field (`set_value` empty) and returns
    /// Rule 1's refusal. An unreadable window or field returns `None` (the pre-act checks stand).
    fn sensitive_after(auto: &Auto, issued: &Issued) -> Option<Refusal> {
        let after = auto
            .snapshot(
                Target::window(issued.pid, issued.window),
                Limits {
                    max_depth: DEFAULT_DEPTH as usize,
                    max_nodes: DEFAULT_NODES as usize,
                },
            )
            .ok()?;
        let focused = after.focused.clone()?;
        let node = find_node(&after.root, &focused)?;
        let value = node.value.clone().unwrap_or_default();
        sensitive::sensitive_value(&value)?;
        let clear = ActOptions {
            min_tier: Tier::Semantic,
            max_tier: Tier::Targeted,
            guard: Guard::Structure,
            confirm: Vec::new(),
            strict_focus: true,
            log_values: false,
            settle_ms: 50,
            fence: None,
        };
        // Best effort: the refusal stands whether or not the field could be cleared.
        let _ = auto.act(
            &after.id,
            Some(focused.as_str()),
            Action::SetValue(String::new()),
            &clear,
        );
        Some(sensitive_input())
    }

    /// Rule 1's refusal.
    fn sensitive_input() -> Refusal {
        Refusal::new(
            "sensitive_input",
            "The app does not type a password, card number, bank detail or government ID for an agent; the user types it, or uses their password manager or the system's autofill and confirms each one",
        )
    }

    /// The session family of a form-fill grant. A key too long for a grant family is hashed.
    fn fill_family(key: &str) -> String {
        if key.len() <= 120 {
            format!("fill:{key}")
        } else {
            format!("fill:{}", approvals::bind_of(&[key.as_bytes()]))
        }
    }

    /// `action.profile` (`name`, `email`, `phone`, `address`) replaces `text` with that detail from
    /// the profile the user typed in settings. Returns the action spec to parse and whether its
    /// text is a profile value (given as `profile`, or `text` equal to one).
    fn resolve_profile(settings: &Settings, spec: &Value) -> Result<(Value, bool)> {
        let kind = spec["type"].as_str().unwrap_or("");
        let fill = matches!(kind, "set_value" | "type_text");
        let is_profile_text = |text: &str| {
            ["name", "email", "phone", "address"]
                .iter()
                .any(|f| settings.profile_value(f) == Some(text))
        };
        match &spec["profile"] {
            Value::Null => {
                let from = fill
                    && spec["text"]
                        .as_str()
                        .is_some_and(|t| settings.form_fill && is_profile_text(t));
                Ok((spec.clone(), from))
            }
            Value::String(field) if fill => {
                if !settings.form_fill {
                    return Err(Refusal::new(
                        "form_fill_off",
                        "form fill is turned off in the app's settings (remoteFormFill)",
                    ));
                }
                if !spec["text"].is_null() {
                    return Err(invalid("give action.profile or action.text, not both"));
                }
                let value = ["name", "email", "phone", "address"]
                    .contains(&field.as_str())
                    .then(|| settings.profile_value(field))
                    .flatten()
                    .ok_or_else(|| {
                        Refusal::new(
                            "profile_missing",
                            "that detail is not in the profile in the app's settings (remoteFormFillProfile)",
                        )
                    })?;
                let mut copy = spec.clone();
                copy["text"] = json!(value);
                if let Some(fields) = copy.as_object_mut() {
                    fields.remove("profile");
                }
                Ok((copy, true))
            }
            _ => Err(invalid(
                "action.profile is name, email, phone or address, for set_value and type_text",
            )),
        }
    }

    /// A crate error as a stable refusal. Messages are the crate's own and carry no content.
    fn auto_refusal(error: AutoError) -> Refusal {
        match error {
            AutoError::PermissionDenied(_) => Refusal::new(
                "permission_missing",
                "the OS permission for this was withdrawn; grant it in System Settings",
            ),
            AutoError::NotFound(_) => Refusal::new("target_changed", error.to_string()),
            AutoError::Blocked(_) | AutoError::NeedsConfirmation(_) | AutoError::Denied(_) => {
                Refusal::new("refused", error.to_string())
            }
            AutoError::NoTier(_) => Refusal::new(
                "no_tier",
                "no input path that leaves the user's focus alone can do this; foreground input is not allowed",
            ),
            AutoError::FocusMoved { .. } => Refusal::new(
                "focus_moved",
                "the action moved the user's focus, which is refused",
            ),
            AutoError::Invalid(_) | AutoError::UnknownSnapshot(_) => {
                Refusal::new("invalid_request", error.to_string())
            }
            AutoError::Unsupported(_) => Refusal::new("unsupported_platform", error.to_string()),
            _ => Refusal::new("act_failed", error.to_string()),
        }
    }
}
