//! Bounded, pane-attributable WezTerm readback for Windows paste confirmation
//! (feature `desktop-context`, Windows only).
//!
//! WezTerm does not expose usable UIA text patterns.  Its CLI can provide
//! insertion evidence, but only after the caller has supplied an exact mux
//! binding.  This module deliberately has no discovery fallback: pane 0,
//! `WEZTERM_PANE`, or a default/global socket is unsafe for production use.
//!
//! The host supplies a [`WezTermPolicy`]: how the user profile directory is
//! resolved, the pin-resolution budget, the CLI output cap and an optional
//! event sink. The WezTerm identifiers (GUI exe, CLI exe, window class,
//! socket directory) belong to WezTerm itself and stay here.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use rightkit_child_process::{run_command_capture, CaptureOptions, OwnedCommand};

use super::uia_worker::TextProof;

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

/// Default cap on one `wezterm cli` stdout; override with
/// `WezTermPolicy::max_output_bytes`.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_PROBE_BUDGET: Duration = Duration::from_secs(60);
const MAX_PROBE_WORKERS: usize = 8;
static PROBE_WORKERS: AtomicUsize = AtomicUsize::new(0);

struct ProbeSlot;

impl Drop for ProbeSlot {
    fn drop(&mut self) {
        PROBE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn start_probe(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), WezTermError> {
    PROBE_WORKERS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < MAX_PROBE_WORKERS).then_some(active + 1)
        })
        .map_err(|_| WezTermError::Busy)?;
    let slot = ProbeSlot;
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _slot = slot;
            work();
        })
        .map(|_| ())
        .map_err(|_| WezTermError::CliUnavailable)
}

fn probe_deadline(budget: Duration) -> Result<Instant, WezTermError> {
    if budget > MAX_PROBE_BUDGET {
        return Err(WezTermError::Timeout);
    }
    Instant::now()
        .checked_add(budget)
        .ok_or(WezTermError::Timeout)
}

/// Host inputs for WezTerm pane resolution.
#[derive(Clone)]
pub struct WezTermPolicy {
    /// Resolves the current user's profile directory (the shell's known
    /// folder, never an environment variable). The process-specific WezTerm
    /// socket is looked up under `<profile>/.local/share/wezterm`.
    pub profile_dir: fn() -> Option<PathBuf>,
    /// Wall-clock budget of the background pane resolution plus baseline read.
    pub pin_resolve_budget: Duration,
    /// Output cap for one CLI call; larger output is `OutputTooLarge`.
    pub max_output_bytes: usize,
    /// Optional event sink `(event, detail)`. Events: `wezterm_pin_resolved`,
    /// `wezterm_pin_resolve_failed`.
    pub event_sink: Option<fn(&str, &str)>,
}

impl WezTermPolicy {
    pub fn new(profile_dir: fn() -> Option<PathBuf>) -> Self {
        Self {
            profile_dir,
            pin_resolve_budget: Duration::from_millis(3_000),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            event_sink: None,
        }
    }
}

/// Immutable identity captured while the intended WezTerm pane owns focus.
/// `window_id` is obtained by a trusted integration resolver; the CLI cannot
/// derive an HWND-to-mux mapping on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WezTermPaneBinding {
    pub hwnd: isize,
    pub pid: u32,
    pub window_id: u64,
    pub pane_id: u64,
    pub socket: PathBuf,
    pub cli_path: PathBuf,
    pub process_creation_time: u64,
    /// Output cap applied to every CLI call made for this binding.
    pub max_output_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WezTermReadback {
    pub text: String,
    pub window_id: u64,
    pub pane_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WezTermError {
    InvalidBinding,
    TargetChanged,
    AmbiguousPane,
    CliUnavailable,
    Busy,
    Timeout,
    OutputTooLarge,
    Io(String),
    InvalidUtf8,
}

/// Parse `wezterm cli list --format json`, requiring one active exact pane.
/// The caller must separately establish that `window_id` belongs to `hwnd/pid`.
pub fn parse_active_pane(json: &str, binding: &WezTermPaneBinding) -> Result<(), WezTermError> {
    let rows: serde_json::Value =
        serde_json::from_str(json).map_err(|_| WezTermError::AmbiguousPane)?;
    let rows = rows.as_array().ok_or(WezTermError::AmbiguousPane)?;
    let matches = rows
        .iter()
        .filter(|row| {
            row.get("window_id").and_then(serde_json::Value::as_u64) == Some(binding.window_id)
                && row.get("pane_id").and_then(serde_json::Value::as_u64) == Some(binding.pane_id)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(WezTermError::AmbiguousPane);
    }
    if matches[0]
        .get("is_active")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(WezTermError::TargetChanged);
    }
    Ok(())
}

/// Parse `wezterm cli list-clients --format json`, requiring exactly one
/// client for the captured GUI process and its focused pane to remain bound.
/// `list` alone is insufficient because WezTerm marks panes in multiple tabs
/// active within one window.
pub fn parse_focused_client(
    json: &str,
    pid: u32,
    expected_pane_id: Option<u64>,
) -> Result<u64, WezTermError> {
    let rows: serde_json::Value =
        serde_json::from_str(json).map_err(|_| WezTermError::AmbiguousPane)?;
    let rows = rows.as_array().ok_or(WezTermError::AmbiguousPane)?;
    let matches = rows
        .iter()
        .filter(|row| row.get("pid").and_then(serde_json::Value::as_u64) == Some(pid as u64))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(WezTermError::AmbiguousPane);
    }
    let focused = matches[0]
        .get("focused_pane_id")
        .and_then(serde_json::Value::as_u64)
        .ok_or(WezTermError::AmbiguousPane)?;
    if let Some(expected) = expected_pane_id {
        if expected != focused {
            return Err(WezTermError::TargetChanged);
        }
    }
    Ok(focused)
}

/// Resolve a target without guessing mux state. This accepts only one visible
/// top-level HWND for PID, a live process creation timestamp, the installed
/// sibling CLI, and a process-specific socket. The returned binding is safe
/// to capture at Stop and revalidate at delivery time.
#[cfg(windows)]
fn prepare_provisional(
    policy: &WezTermPolicy,
    hwnd: isize,
    pid: u32,
    expected_creation_time: u64,
) -> Result<WezTermPaneBinding, WezTermError> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LPARAM},
        System::Threading::{
            GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible,
        },
    };
    if hwnd == 0 || pid == 0 {
        return Err(WezTermError::InvalidBinding);
    }
    unsafe {
        if GetForegroundWindow() != hwnd as HWND {
            return Err(WezTermError::TargetChanged);
        }
        struct EnumContext {
            pid: u32,
            seen: Vec<isize>,
        }
        let mut context = EnumContext {
            pid,
            seen: Vec::new(),
        };
        extern "system" fn collect(window: HWND, data: LPARAM) -> i32 {
            unsafe {
                let context = &mut *(data as *mut EnumContext);
                let mut owner = 0;
                GetWindowThreadProcessId(window, &mut owner);
                if owner == context.pid && IsWindowVisible(window) != 0 {
                    // WezTerm also creates visible IME & OpenGL probe windows.
                    // Only actual terminal windows participate in mux binding.
                    let mut class = [0u16; 256];
                    let len = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
                    if len > 0
                        && String::from_utf16_lossy(&class[..len as usize])
                            == "org.wezfurlong.wezterm"
                    {
                        context.seen.push(window as isize);
                    }
                }
                1
            }
        }
        EnumWindows(Some(collect), &mut context as *mut _ as LPARAM);
        if context.seen.len() != 1 || context.seen[0] != hwnd {
            return Err(WezTermError::AmbiguousPane);
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err(WezTermError::TargetChanged);
        }
        let mut created = std::mem::zeroed();
        let mut exited = std::mem::zeroed();
        let mut kernel = std::mem::zeroed();
        let mut user = std::mem::zeroed();
        let times_ok =
            GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        let mut chars = 32_768u32;
        let mut path = vec![0u16; chars as usize];
        let path_ok = QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut chars) != 0;
        CloseHandle(handle);
        if !times_ok || !path_ok || filetime_u64(&created) != expected_creation_time {
            return Err(WezTermError::TargetChanged);
        }
        path.truncate(chars as usize);
        let exe = PathBuf::from(String::from_utf16_lossy(&path));
        if exe
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("wezterm-gui.exe"))
            != Some(true)
        {
            return Err(WezTermError::InvalidBinding);
        }
        let cli_path = exe
            .parent()
            .ok_or(WezTermError::InvalidBinding)?
            .join("wezterm.exe");
        if !cli_path.is_file() {
            return Err(WezTermError::CliUnavailable);
        }
        let profile = (policy.profile_dir)().ok_or(WezTermError::InvalidBinding)?;
        let socket = profile
            .join(".local/share/wezterm")
            .join(format!("gui-sock-{pid}"));
        if !socket.is_file() {
            return Err(WezTermError::CliUnavailable);
        }
        let provisional = WezTermPaneBinding {
            hwnd,
            pid,
            window_id: 0,
            pane_id: 0,
            socket: socket.clone(),
            cli_path: cli_path.clone(),
            process_creation_time: expected_creation_time,
            max_output_bytes: policy.max_output_bytes,
        };
        Ok(provisional)
    }
}

/// CLI half of the resolution: which pane of the (already Win32-validated)
/// GUI process is focused. `list-clients` and `list` run concurrently, so the
/// cost is one cold `wezterm cli` launch, not two.
#[cfg(windows)]
fn resolve_panes(
    provisional: &WezTermPaneBinding,
    deadline: Instant,
) -> Result<WezTermPaneBinding, WezTermError> {
    let (clients, list) = std::thread::scope(|scope| {
        let clients =
            scope.spawn(|| run_cli(provisional, &["list-clients", "--format", "json"], deadline));
        let list = scope.spawn(|| run_cli(provisional, &["list", "--format", "json"], deadline));
        (
            clients.join().unwrap_or(Err(WezTermError::CliUnavailable)),
            list.join().unwrap_or(Err(WezTermError::CliUnavailable)),
        )
    });
    let pane_id = parse_focused_client(&clients?, provisional.pid, None)?;
    let list = list?;
    let rows: serde_json::Value =
        serde_json::from_str(&list).map_err(|_| WezTermError::AmbiguousPane)?;
    let row = rows
        .as_array()
        .and_then(|rows| {
            let matches = rows
                .iter()
                .filter(|row| {
                    row.get("pane_id").and_then(serde_json::Value::as_u64) == Some(pane_id)
                })
                .collect::<Vec<_>>();
            (matches.len() == 1).then(|| matches[0])
        })
        .ok_or(WezTermError::AmbiguousPane)?;
    let window_id = row
        .get("window_id")
        .and_then(serde_json::Value::as_u64)
        .ok_or(WezTermError::AmbiguousPane)?;
    if row.get("is_active").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(WezTermError::TargetChanged);
    }
    let binding = WezTermPaneBinding {
        window_id,
        pane_id,
        ..provisional.clone()
    };
    validate_target(&binding)?;
    if Instant::now() >= deadline {
        return Err(WezTermError::Timeout);
    }
    Ok(binding)
}

/// Synchronous resolution (both halves). Kept for callers that need a complete
/// binding immediately; stop capture uses [`WezTermPin::begin`] instead.
#[cfg(windows)]
#[allow(dead_code)]
pub fn resolve_singleton(
    policy: &WezTermPolicy,
    hwnd: isize,
    pid: u32,
    expected_creation_time: u64,
    timeout: Duration,
) -> Result<WezTermPaneBinding, WezTermError> {
    let deadline = probe_deadline(timeout)?;
    let provisional = prepare_provisional(policy, hwnd, pid, expected_creation_time)?;
    resolve_panes(&provisional, deadline)
}

/// Stop-time pin. Construction is Win32/file work only (foreground HWND,
/// process identity, singleton window, CLI + socket paths: single-digit ms).
/// The mux resolution (focused pane id) and a baseline `get-text` run on a
/// background thread during transcription and are reused at delivery, so
/// neither a cold `wezterm cli` launch nor a baseline read sits on the Stop
/// critical path or in front of Ctrl+V.
#[cfg(windows)]
#[derive(Clone)]
pub struct WezTermPin {
    provisional: WezTermPaneBinding,
    shared: std::sync::Arc<(std::sync::Mutex<PinResolution>, std::sync::Condvar)>,
}

#[cfg(windows)]
pub struct ResolvedPin {
    pub binding: WezTermPaneBinding,
    /// Pane text read right after resolution (stop time). Stale by the length of
    /// transcription, which `terminal_grid_insertion_attributable` tolerates: it
    /// compares occurrence counts of the transcript, not whole-grid equality.
    pub baseline: Option<WezTermReadback>,
}

#[cfg(windows)]
pub enum PinResolution {
    Resolving,
    Ready(std::sync::Arc<ResolvedPin>),
    Failed,
}

#[cfg(windows)]
pub enum PinView {
    Ready(std::sync::Arc<ResolvedPin>),
    Resolving,
    Failed,
}

#[cfg(windows)]
impl WezTermPin {
    pub fn begin(
        policy: &WezTermPolicy,
        hwnd: isize,
        pid: u32,
        creation: u64,
    ) -> Result<Self, WezTermError> {
        let started = Instant::now();
        let deadline = probe_deadline(policy.pin_resolve_budget)?;
        let provisional = prepare_provisional(policy, hwnd, pid, creation)?;
        let shared = std::sync::Arc::new((
            std::sync::Mutex::new(PinResolution::Resolving),
            std::sync::Condvar::new(),
        ));
        let worker_binding = provisional.clone();
        let worker_shared = std::sync::Arc::clone(&shared);
        let event_sink = policy.event_sink;
        start_probe("wezterm-pin-resolve", move || {
            let resolved = resolve_panes(&worker_binding, deadline).map(|binding| {
                let baseline = read_bound_pane_text(
                    &binding,
                    deadline.saturating_duration_since(Instant::now()),
                )
                .ok();
                std::sync::Arc::new(ResolvedPin { binding, baseline })
            });
            let elapsed_ms = started.elapsed().as_millis();
            let state = match resolved {
                Ok(pin) => {
                    if let Some(sink) = event_sink {
                        sink(
                            "wezterm_pin_resolved",
                            &format!(
                                "pid={} baseline_present={} elapsed_ms={elapsed_ms}",
                                worker_binding.pid,
                                pin.baseline.is_some()
                            ),
                        );
                    }
                    PinResolution::Ready(pin)
                }
                Err(error) => {
                    if let Some(sink) = event_sink {
                        sink(
                            "wezterm_pin_resolve_failed",
                            &format!(
                                "pid={} error={error:?} elapsed_ms={elapsed_ms}",
                                worker_binding.pid
                            ),
                        );
                    }
                    PinResolution::Failed
                }
            };
            if let Ok(mut guard) = worker_shared.0.lock() {
                *guard = state;
            }
            worker_shared.1.notify_all();
        })?;
        Ok(Self {
            provisional,
            shared,
        })
    }

    /// Window-level identity only (hwnd, pid, creation time, socket, CLI path);
    /// `window_id`/`pane_id` are zero. Enough for `validate_bound_window`.
    pub fn window_binding(&self) -> &WezTermPaneBinding {
        &self.provisional
    }

    /// Current resolution state, waiting at most `wait` for it to finish.
    pub fn view(&self, wait: Duration) -> PinView {
        let (lock, wake) = &*self.shared;
        let Ok(deadline) = probe_deadline(wait) else {
            return PinView::Failed;
        };
        let Ok(mut guard) = lock.lock() else {
            return PinView::Failed;
        };
        loop {
            match &*guard {
                PinResolution::Ready(pin) => return PinView::Ready(std::sync::Arc::clone(pin)),
                PinResolution::Failed => return PinView::Failed,
                PinResolution::Resolving => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return PinView::Resolving;
            }
            match wake.wait_timeout(guard, remaining) {
                Ok((next, _)) => guard = next,
                Err(_) => return PinView::Failed,
            }
        }
    }
}

#[cfg(windows)]
pub fn capture_process_identity(hwnd: isize) -> Result<(u32, u64), WezTermError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    unsafe {
        if hwnd == 0 || GetForegroundWindow() != hwnd as HWND {
            return Err(WezTermError::TargetChanged);
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd as HWND, &mut pid);
        // Same OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)+GetProcessTimes+
        // CloseHandle+FILETIME-combine sequence this used to inline — `None`
        // on either a null handle or a failed GetProcessTimes call, matching
        // this function's prior "either failure -> TargetChanged" behavior.
        let creation_time = crate::desktop::windows::process_creation_time(pid)
            .ok_or(WezTermError::TargetChanged)?;
        Ok((pid, creation_time))
    }
}

#[cfg(windows)]
fn filetime_u64(value: &windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

#[allow(dead_code)]
/// Validate foreground HWND/PID, then read the exact pane through its bound
/// socket. Run immediately before and after paste to reject focus/pane swaps.
pub fn read_bound_pane(
    binding: &WezTermPaneBinding,
    timeout: Duration,
) -> Result<WezTermReadback, WezTermError> {
    let deadline = probe_deadline(timeout)?;
    validate_target(binding)?;
    let clients = run_cli(binding, &["list-clients", "--format", "json"], deadline)?;
    parse_focused_client(&clients, binding.pid, Some(binding.pane_id))?;
    let list = run_cli(binding, &["list", "--format", "json"], deadline)?;
    parse_active_pane(&list, binding)?;
    let text = run_cli(
        binding,
        &["get-text", "--pane-id", &binding.pane_id.to_string()],
        deadline,
    )?;
    validate_target(binding)?;
    let clients_after = run_cli(binding, &["list-clients", "--format", "json"], deadline)?;
    parse_focused_client(&clients_after, binding.pid, Some(binding.pane_id))?;
    let list_after = run_cli(binding, &["list", "--format", "json"], deadline)?;
    parse_active_pane(&list_after, binding)?;
    validate_target(binding)?;
    Ok(WezTermReadback {
        text,
        window_id: binding.window_id,
        pane_id: binding.pane_id,
    })
}

pub fn validate_bound_pane(
    binding: &WezTermPaneBinding,
    timeout: Duration,
) -> Result<(), WezTermError> {
    let deadline = probe_deadline(timeout)?;
    validate_target(binding)?;
    let clients = run_cli(binding, &["list-clients", "--format", "json"], deadline)?;
    parse_focused_client(&clients, binding.pid, Some(binding.pane_id))?;
    let list = run_cli(binding, &["list", "--format", "json"], deadline)?;
    parse_active_pane(&list, binding)?;
    validate_target(binding)
}

/// Foreground HWND, owning pid and process-creation identity only. No CLI
/// process is spawned, so this is cheap enough for every delivery gate; the
/// pane-level proof runs once through `prepaste_probe`.
pub fn validate_bound_window(binding: &WezTermPaneBinding) -> Result<(), WezTermError> {
    validate_target(binding)
}

/// Single-spawn confirmation read: the exact bound pane's text (`get-text
/// --pane-id`), bracketed by the cheap foreground/process validation. The pane
/// id is fixed in the command, so the text is attributable to the bound pane
/// without re-listing clients and panes (five spawns, ~145 ms each, in
/// `read_bound_pane`). Used for the pre-paste baseline and the post-paste polls.
pub fn read_bound_pane_text(
    binding: &WezTermPaneBinding,
    timeout: Duration,
) -> Result<WezTermReadback, WezTermError> {
    let deadline = probe_deadline(timeout)?;
    validate_target(binding)?;
    let text = run_cli(
        binding,
        &["get-text", "--pane-id", &binding.pane_id.to_string()],
        deadline,
    )?;
    validate_target(binding)?;
    Ok(WezTermReadback {
        text,
        window_id: binding.window_id,
        pane_id: binding.pane_id,
    })
}

/// Result of the bounded pre-paste pane check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanePreflight {
    /// The bound pane is the focused client's active pane.
    Current,
    /// Foreground, process or pane identity definitively differs.
    Moved,
    /// The budget ran out before the CLI answered; the cheap foreground/process
    /// validation passed, but there is no pane-level proof. This is not
    /// permission to insert: the host refuses automatic insertion on anything
    /// but `Current` (heardright#359), so `Unknown` degrades to the clipboard
    /// fallback.
    Unknown,
}

/// Pane check (and, when `want_text`, a fresh baseline read) run
/// CONCURRENTLY on detached workers and are collected through a channel with
/// `recv_timeout`, so this returns at `budget` even when a `wezterm cli`
/// launch is still running or its process-tree cleanup is slow. A missing
/// baseline only removes the readback channel; an unfinished pane check is
/// `Unknown`.
pub fn prepaste_probe(
    binding: &WezTermPaneBinding,
    budget: Duration,
    want_text: bool,
) -> (PanePreflight, Option<WezTermReadback>) {
    enum Part {
        Clients(Result<(), WezTermError>),
        List(Result<(), WezTermError>),
        Text(Result<String, WezTermError>),
    }
    let Ok(deadline) = probe_deadline(budget) else {
        return (PanePreflight::Unknown, None);
    };
    if validate_target(binding).is_err() {
        return (PanePreflight::Moved, None);
    }
    let (tx, rx) = std::sync::mpsc::channel::<Part>();
    let mut expected = 2;
    {
        let (binding, tx) = (binding.clone(), tx.clone());
        if start_probe("wezterm-list-clients", move || {
            let result = run_cli(&binding, &["list-clients", "--format", "json"], deadline)
                .and_then(|json| parse_focused_client(&json, binding.pid, Some(binding.pane_id)))
                .map(|_| ());
            let _ = tx.send(Part::Clients(result));
        })
        .is_err()
        {
            return (PanePreflight::Unknown, None);
        }
    }
    {
        let (binding, tx) = (binding.clone(), tx.clone());
        if start_probe("wezterm-list-panes", move || {
            let result = run_cli(&binding, &["list", "--format", "json"], deadline)
                .and_then(|json| parse_active_pane(&json, &binding));
            let _ = tx.send(Part::List(result));
        })
        .is_err()
        {
            return (PanePreflight::Unknown, None);
        }
    }
    if want_text {
        let (binding, tx) = (binding.clone(), tx.clone());
        if start_probe("wezterm-read-pane", move || {
            let result = run_cli(
                &binding,
                &["get-text", "--pane-id", &binding.pane_id.to_string()],
                deadline,
            );
            let _ = tx.send(Part::Text(result));
        })
        .is_ok()
        {
            expected += 1;
        }
    }
    drop(tx);
    let (mut clients, mut list, mut text) = (None, None, None);
    for _ in 0..expected {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Part::Clients(result)) => clients = Some(result),
            Ok(Part::List(result)) => list = Some(result),
            Ok(Part::Text(result)) => text = Some(result),
            Err(_) => break,
        }
    }
    let preflight = match (clients, list) {
        (Some(Ok(())), Some(Ok(()))) => PanePreflight::Current,
        (Some(Err(error)), _) | (_, Some(Err(error))) if error != WezTermError::Timeout => {
            PanePreflight::Moved
        }
        _ => PanePreflight::Unknown,
    };
    let baseline = match text {
        Some(Ok(text)) if preflight != PanePreflight::Moved && validate_target(binding).is_ok() => {
            Some(WezTermReadback {
                text,
                window_id: binding.window_id,
                pane_id: binding.pane_id,
            })
        }
        _ => None,
    };
    (preflight, baseline)
}

fn validate_target(binding: &WezTermPaneBinding) -> Result<(), WezTermError> {
    if binding.hwnd == 0
        || binding.pid == 0
        || binding.process_creation_time == 0
        || binding.socket.as_os_str().is_empty()
        || !Path::new(&binding.socket).is_absolute()
    {
        return Err(WezTermError::InvalidBinding);
    }
    #[cfg(windows)]
    unsafe {
        if GetForegroundWindow() != binding.hwnd as HWND {
            return Err(WezTermError::TargetChanged);
        }
        let mut pid = 0;
        GetWindowThreadProcessId(binding.hwnd as HWND, &mut pid);
        if pid != binding.pid {
            return Err(WezTermError::TargetChanged);
        }
        // Same OpenProcess+GetProcessTimes+CloseHandle+FILETIME-combine this
        // used to inline: `None` on a null handle or failed GetProcessTimes,
        // matching the prior "`!ok || mismatch` -> TargetChanged" behavior.
        let creation_time = crate::desktop::windows::process_creation_time(binding.pid)
            .ok_or(WezTermError::TargetChanged)?;
        if creation_time != binding.process_creation_time {
            return Err(WezTermError::TargetChanged);
        }
    }
    Ok(())
}

fn run_cli(
    binding: &WezTermPaneBinding,
    args: &[&str],
    deadline: Instant,
) -> Result<String, WezTermError> {
    let mut command = OwnedCommand::new(&binding.cli_path);
    command
        .command_mut()
        .args(["cli", "--no-auto-start"])
        .args(args)
        .env("WEZTERM_UNIX_SOCKET", &binding.socket);
    bounded_output(command, deadline, binding.max_output_bytes)
}

fn bounded_output(
    command: OwnedCommand,
    deadline: Instant,
    max_output_bytes: usize,
) -> Result<String, WezTermError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(WezTermError::Timeout);
    }
    if max_output_bytes == 0 || max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(WezTermError::OutputTooLarge);
    }
    // The owner's capture runner drains the pipe on its own thread, keeps the first N bytes,
    // terminates and reaps the tree at the deadline, and bounds its own cleanup.
    let options = CaptureOptions::default()
        .timeout(deadline.saturating_duration_since(now))
        .stdout_cap(max_output_bytes)
        .stderr_cap(0);
    let output =
        run_command_capture(command, None, &options).map_err(|_| WezTermError::CliUnavailable)?;
    if output.timed_out {
        return Err(WezTermError::Timeout);
    }
    if !output.status.success() {
        return Err(WezTermError::Io("wezterm cli failed".into()));
    }
    if output.stdout.truncated {
        return Err(WezTermError::OutputTooLarge);
    }
    String::from_utf8(output.stdout.bytes).map_err(|_| WezTermError::InvalidUtf8)
}

impl Default for WezTermPaneBinding {
    fn default() -> Self {
        Self {
            hwnd: 0,
            pid: 0,
            window_id: 0,
            pane_id: 0,
            socket: PathBuf::new(),
            cli_path: PathBuf::new(),
            process_creation_time: 0,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

/// UIA-shaped proof for a bound pane's readback: terminal rows scoped to the
/// exact (window id, pane id, HWND), so a proof can only ever be compared
/// against a proof of the same pane.
pub fn text_proof(binding: &WezTermPaneBinding, read: WezTermReadback) -> TextProof {
    TextProof {
        value: None,
        document: None,
        terminal_rows: Some(read.text),
        terminal_scope_runtime_id: Some(
            [
                u64_scope_parts(read.window_id),
                u64_scope_parts(read.pane_id),
            ]
            .into_iter()
            .flatten()
            .collect(),
        ),
        terminal_scope_native_window_handle: Some(binding.hwnd),
    }
}

fn u64_scope_parts(value: u64) -> [i32; 2] {
    [value as u32 as i32, (value >> 32) as u32 as i32]
}
