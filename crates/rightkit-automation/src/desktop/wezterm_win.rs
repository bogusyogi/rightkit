//! Bounded, pane-attributable WezTerm readback for Windows paste confirmation.
//!
//! WezTerm does not expose usable UIA text patterns.  Its CLI can provide
//! insertion evidence, but only after the caller has supplied an exact mux
//! binding.  This module deliberately has no discovery fallback: pane 0,
//! `WEZTERM_PANE`, or a default/global socket is unsafe for production use.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rightkit_child_process::{run_command_capture, CaptureOptions, OwnedCommand};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

const MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// Immutable identity captured while the intended WezTerm pane owns focus.
/// `window_id` is obtained by a trusted integration resolver; the CLI cannot
/// derive an HWND-to-mux mapping on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WezTermPaneBinding {
    pub(crate) hwnd: isize,
    pub(crate) pid: u32,
    pub(crate) window_id: u64,
    pub(crate) pane_id: u64,
    pub(crate) socket: PathBuf,
    pub(crate) cli_path: PathBuf,
    pub(crate) process_creation_time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WezTermReadback {
    pub(crate) text: String,
    pub(crate) window_id: u64,
    pub(crate) pane_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WezTermError {
    InvalidBinding,
    TargetChanged,
    AmbiguousPane,
    CliUnavailable,
    Timeout,
    OutputTooLarge,
    Io(String),
    InvalidUtf8,
}

/// Parse `wezterm cli list --format json`, requiring one active exact pane.
/// The caller must separately establish that `window_id` belongs to `hwnd/pid`.
pub(crate) fn parse_active_pane(
    json: &str,
    binding: &WezTermPaneBinding,
) -> Result<(), WezTermError> {
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
pub(crate) fn parse_focused_client(
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
        let profile =
            crate::desktop::windows_known_folder(&windows::Win32::UI::Shell::FOLDERID_Profile)
                .ok_or(WezTermError::InvalidBinding)?;
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
pub(crate) fn resolve_singleton(
    hwnd: isize,
    pid: u32,
    expected_creation_time: u64,
    timeout: Duration,
) -> Result<WezTermPaneBinding, WezTermError> {
    let deadline = Instant::now() + timeout;
    let provisional = prepare_provisional(hwnd, pid, expected_creation_time)?;
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
pub(crate) struct ResolvedPin {
    pub(crate) binding: WezTermPaneBinding,
    /// Pane text read right after resolution (stop time). Stale by the length of
    /// transcription, which `terminal_grid_insertion_attributable` tolerates: it
    /// compares occurrence counts of the transcript, not whole-grid equality.
    pub(crate) baseline: Option<WezTermReadback>,
}

#[cfg(windows)]
pub(crate) enum PinResolution {
    Resolving,
    Ready(std::sync::Arc<ResolvedPin>),
    Failed,
}

#[cfg(windows)]
pub(crate) enum PinView {
    Ready(std::sync::Arc<ResolvedPin>),
    Resolving,
    Failed,
}

#[cfg(windows)]
const PIN_RESOLVE_BUDGET: Duration = Duration::from_millis(3_000);

#[cfg(windows)]
impl WezTermPin {
    pub(crate) fn begin(hwnd: isize, pid: u32, creation: u64) -> Result<Self, WezTermError> {
        let started = Instant::now();
        let provisional = prepare_provisional(hwnd, pid, creation)?;
        let shared = std::sync::Arc::new((
            std::sync::Mutex::new(PinResolution::Resolving),
            std::sync::Condvar::new(),
        ));
        let worker_binding = provisional.clone();
        let worker_shared = std::sync::Arc::clone(&shared);
        std::thread::Builder::new()
            .name("wezterm-pin-resolve".to_string())
            .spawn(move || {
                let deadline = Instant::now() + PIN_RESOLVE_BUDGET;
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
                        rightkit_dictation_telemetry::telemetry::log_message(
                            "wezterm_pin_resolved",
                            &format!(
                                "pid={} baseline_present={} elapsed_ms={elapsed_ms}",
                                worker_binding.pid,
                                pin.baseline.is_some()
                            ),
                        );
                        PinResolution::Ready(pin)
                    }
                    Err(error) => {
                        rightkit_dictation_telemetry::telemetry::log_message(
                            "wezterm_pin_resolve_failed",
                            &format!(
                                "pid={} error={error:?} elapsed_ms={elapsed_ms}",
                                worker_binding.pid
                            ),
                        );
                        PinResolution::Failed
                    }
                };
                if let Ok(mut guard) = worker_shared.0.lock() {
                    *guard = state;
                }
                worker_shared.1.notify_all();
            })
            .map_err(|_| WezTermError::CliUnavailable)?;
        Ok(Self {
            provisional,
            shared,
        })
    }

    /// Window-level identity only (hwnd, pid, creation time, socket, CLI path);
    /// `window_id`/`pane_id` are zero. Enough for `validate_bound_window`.
    pub(crate) fn window_binding(&self) -> &WezTermPaneBinding {
        &self.provisional
    }

    /// Current resolution state, waiting at most `wait` for it to finish.
    pub(crate) fn view(&self, wait: Duration) -> PinView {
        let (lock, wake) = &*self.shared;
        let deadline = Instant::now() + wait;
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
pub(crate) fn capture_process_identity(hwnd: isize) -> Result<(u32, u64), WezTermError> {
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
pub(crate) fn read_bound_pane(
    binding: &WezTermPaneBinding,
    timeout: Duration,
) -> Result<WezTermReadback, WezTermError> {
    let deadline = Instant::now() + timeout;
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

pub(crate) fn validate_bound_pane(
    binding: &WezTermPaneBinding,
    timeout: Duration,
) -> Result<(), WezTermError> {
    let deadline = Instant::now() + timeout;
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
pub(crate) fn validate_bound_window(binding: &WezTermPaneBinding) -> Result<(), WezTermError> {
    validate_target(binding)
}

/// Single-spawn confirmation read: the exact bound pane's text (`get-text
/// --pane-id`), bracketed by the cheap foreground/process validation. The pane
/// id is fixed in the command, so the text is attributable to the bound pane
/// without re-listing clients and panes (five spawns, ~145 ms each, in
/// `read_bound_pane`). Used for the pre-paste baseline and the post-paste polls.
pub(crate) fn read_bound_pane_text(
    binding: &WezTermPaneBinding,
    timeout: Duration,
) -> Result<WezTermReadback, WezTermError> {
    let deadline = Instant::now() + timeout;
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
pub(crate) enum PanePreflight {
    /// The bound pane is the focused client's active pane.
    Current,
    /// Foreground, process or pane identity definitively differs.
    Moved,
    /// The budget ran out before the CLI answered; the cheap foreground/process
    /// validation passed, so delivery proceeds without pane-level proof.
    Unknown,
}

/// Pane check (and, when `want_text`, a fresh baseline read) run
/// CONCURRENTLY on detached workers and are collected through a channel with
/// `recv_timeout`, so this returns at `budget` even when a `wezterm cli`
/// launch is still running or its process-tree cleanup is slow. A missing
/// baseline only removes the readback channel; an unfinished pane check is
/// `Unknown`.
pub(crate) fn prepaste_probe(
    binding: &WezTermPaneBinding,
    budget: Duration,
    want_text: bool,
) -> (PanePreflight, Option<WezTermReadback>) {
    enum Part {
        Clients(Result<(), WezTermError>),
        List(Result<(), WezTermError>),
        Text(Result<String, WezTermError>),
    }
    let deadline = Instant::now() + budget;
    if validate_target(binding).is_err() {
        return (PanePreflight::Moved, None);
    }
    let (tx, rx) = std::sync::mpsc::channel::<Part>();
    let mut expected = 2;
    {
        let (binding, tx) = (binding.clone(), tx.clone());
        std::thread::spawn(move || {
            let result = run_cli(&binding, &["list-clients", "--format", "json"], deadline)
                .and_then(|json| parse_focused_client(&json, binding.pid, Some(binding.pane_id)))
                .map(|_| ());
            let _ = tx.send(Part::Clients(result));
        });
    }
    {
        let (binding, tx) = (binding.clone(), tx.clone());
        std::thread::spawn(move || {
            let result = run_cli(&binding, &["list", "--format", "json"], deadline)
                .and_then(|json| parse_active_pane(&json, &binding));
            let _ = tx.send(Part::List(result));
        });
    }
    if want_text {
        expected += 1;
        let (binding, tx) = (binding.clone(), tx.clone());
        std::thread::spawn(move || {
            let result = run_cli(
                &binding,
                &["get-text", "--pane-id", &binding.pane_id.to_string()],
                deadline,
            );
            let _ = tx.send(Part::Text(result));
        });
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
    bounded_output(command, deadline)
}

fn bounded_output(command: OwnedCommand, deadline: Instant) -> Result<String, WezTermError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(WezTermError::Timeout);
    }
    // The owner's capture runner drains the pipe on its own thread, keeps the first N bytes,
    // terminates and reaps the tree at the deadline, and bounds its own cleanup.
    let options = CaptureOptions::default()
        .timeout(deadline.saturating_duration_since(now))
        .stdout_cap(MAX_OUTPUT_BYTES + 1)
        .stderr_cap(0);
    let output =
        run_command_capture(command, None, &options).map_err(|_| WezTermError::CliUnavailable)?;
    if output.timed_out {
        return Err(WezTermError::Timeout);
    }
    if !output.status.success() {
        return Err(WezTermError::Io("wezterm cli failed".into()));
    }
    let bytes = output.stdout.bytes;
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(WezTermError::OutputTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| WezTermError::InvalidUtf8)
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
        }
    }
}

#[cfg(test)]
#[path = "wezterm_win_tests.rs"]
mod tests;
