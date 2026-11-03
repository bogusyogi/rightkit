//! Terminal keyboard enhancement setup and teardown helpers.
//!
//! The TUI pairs pushes and pops on each screen's keyboard enhancement stack, but
//! process exit gets a stronger reset so the parent shell does not inherit enhanced key
//! reporting if a terminal misses the normal stack pop.
//! Windows terminal detection has a deadline, including WSL interop process launch.
//! Inconclusive detection keeps keyboard enhancements disabled on WSL.

use std::fmt;
use std::io::Write;

use crate::terminal::capability::TerminalName;
use crate::terminal::capability::terminal_info;
use crossterm::Command;
use crossterm::event::KeyboardEnhancementFlags;
use crossterm::event::PopKeyboardEnhancementFlags;
use crossterm::event::PushKeyboardEnhancementFlags;
use ratatui::crossterm::execute;

const DISABLE_KEYBOARD_ENHANCEMENT_ENV_VAR: &str = "RIGHTKIT_TUI_DISABLE_KEYBOARD_ENHANCEMENT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VscodeDetection {
    VsCode,
    Other,
    Unknown,
}

pub(super) fn keyboard_enhancement_disabled() -> bool {
    let disable_env = std::env::var(DISABLE_KEYBOARD_ENHANCEMENT_ENV_VAR).ok();
    let is_wsl = running_in_wsl();
    let vscode_detection = if is_wsl {
        detect_vscode_terminal()
    } else {
        VscodeDetection::Other
    };
    keyboard_enhancement_disabled_for(disable_env.as_deref(), is_wsl, vscode_detection)
}

fn keyboard_enhancement_disabled_for(
    disable_env: Option<&str>,
    is_wsl: bool,
    vscode_detection: VscodeDetection,
) -> bool {
    if let Some(disabled) = parse_bool_env(disable_env) {
        return disabled;
    }

    // VS Code running a WSL shell can hide TERM_PROGRAM from the Linux process
    // environment. If the Windows-side probe is inconclusive, avoid enabling the
    // keyboard mode that can break dead-key composition in VS Code on WSL.
    is_wsl
        && matches!(
            vscode_detection,
            VscodeDetection::VsCode | VscodeDetection::Unknown
        )
}

fn parse_bool_env(value: Option<&str>) -> Option<bool> {
    match value.map(str::trim) {
        Some("1") => Some(true),
        Some(value) if value.eq_ignore_ascii_case("true") => Some(true),
        Some(value) if value.eq_ignore_ascii_case("yes") => Some(true),
        Some("0") => Some(false),
        Some(value) if value.eq_ignore_ascii_case("false") => Some(false),
        Some(value) if value.eq_ignore_ascii_case("no") => Some(false),
        _ => None,
    }
}

/// Adapted seam for the donor's `clipboard_paste::is_probably_wsl` (the only
/// use of that clipboard module this port needs): detect WSL via its env
/// markers, falling back to the kernel release string.
#[cfg(target_os = "linux")]
fn is_probably_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

fn running_in_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        is_probably_wsl()
    }

    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub(crate) fn detect_vscode_terminal() -> VscodeDetection {
    if term_program_is_vscode(std::env::var("TERM_PROGRAM").ok().as_deref()) {
        return VscodeDetection::VsCode;
    }
    vscode_terminal_detected(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        windows_vscode_detection(),
    )
}

fn vscode_terminal_detected(
    linux_term_program: Option<&str>,
    windows_detection: VscodeDetection,
) -> VscodeDetection {
    if term_program_is_vscode(linux_term_program) {
        VscodeDetection::VsCode
    } else {
        windows_detection
    }
}

fn term_program_is_vscode(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("vscode"))
}

fn windows_vscode_detection() -> VscodeDetection {
    #[cfg(target_os = "linux")]
    {
        static WINDOWS_VSCODE_DETECTION: std::sync::OnceLock<VscodeDetection> =
            std::sync::OnceLock::new();
        *WINDOWS_VSCODE_DETECTION.get_or_init(read_windows_vscode_detection)
    }

    #[cfg(not(target_os = "linux"))]
    {
        VscodeDetection::Unknown
    }
}

#[cfg(target_os = "linux")]
fn read_windows_vscode_detection() -> VscodeDetection {
    if !running_in_wsl() {
        return VscodeDetection::Other;
    }
    read_windows_vscode_detection_with_timeout(
        |timeout| {
            let executable = super::tmux::system_executable("cmd.exe")?;
            let mut owned = rightkit_child_process::OwnedCommand::new(executable);
            owned
                .command_mut()
                .args(["/d", "/s", "/c", "set TERM_PROGRAM"]);
            run_bounded(owned, timeout)
        },
        std::time::Duration::from_secs(/*secs*/ 1),
    )
}

/// Run `owned` through the child-process owner with a deadline; the owner terminates and reaps the
/// whole tree when the deadline passes. `None` when the launch failed or the deadline passed.
#[cfg(target_os = "linux")]
fn run_bounded(
    owned: rightkit_child_process::OwnedCommand,
    timeout: std::time::Duration,
) -> Option<(std::process::ExitStatus, Vec<u8>)> {
    let options = rightkit_child_process::StreamOptions::new()
        .timeout(timeout)
        .stdout_tail_cap(64 * 1024)
        .stderr_tail_cap(0);
    let output = rightkit_child_process::run_streaming(
        owned,
        &options,
        &|| false,
        &mut |_chunk: &[u8]| {},
        &mut |_line: &str| {},
    )
    .ok()?;
    if output.timed_out {
        return None;
    }
    Some((output.status, output.stdout_tail))
}

#[cfg(target_os = "linux")]
fn read_windows_vscode_detection_with_timeout(
    probe: impl FnOnce(std::time::Duration) -> Option<(std::process::ExitStatus, Vec<u8>)>
    + Send
    + 'static,
    timeout: std::time::Duration,
) -> VscodeDetection {
    let deadline = std::time::Instant::now() + timeout;
    let (sender, receiver) = std::sync::mpsc::channel();
    // WSL interop can block inside spawn itself. Keep launch and cleanup off the
    // startup thread; the OnceLock caches a timeout so this worker is not retried.
    let worker = std::thread::Builder::new()
        .name("windows-term-program".into())
        .spawn(move || {
            let _ = sender.send(probe(timeout));
        });
    if worker.is_err() {
        return VscodeDetection::Unknown;
    }

    let Ok(Some((status, stdout))) =
        receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    else {
        return VscodeDetection::Unknown;
    };
    if !status.success() {
        // `cmd.exe /c set TERM_PROGRAM` exits with 1 when the variable is absent.
        return if status.code() == Some(1) {
            VscodeDetection::Other
        } else {
            VscodeDetection::Unknown
        };
    }

    let output = String::from_utf8_lossy(&stdout);
    let term_program = output
        .lines()
        .find_map(|line| line.trim_end_matches('\r').strip_prefix("TERM_PROGRAM="))
        .filter(|value| !value.trim().is_empty());
    if term_program_is_vscode(term_program) {
        VscodeDetection::VsCode
    } else {
        VscodeDetection::Other
    }
}

/// Restore keyboard reporting and return the mouse policy from the same fresh tmux probe.
pub(super) fn enable_keyboard_enhancement(writer: &mut impl Write) -> super::tmux::MouseCapture {
    let tmux_options = super::tmux::options();
    if keyboard_enhancement_disabled() {
        return tmux_options.mouse_capture;
    }

    let running_in_tmux_session = running_in_tmux_session();
    let tmux_extended_keys_format = if running_in_tmux_session {
        tmux_options.extended_keys_format.as_deref()
    } else {
        None
    };

    let _ = execute!(
        writer,
        DisableModifyOtherKeys,
        PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
            terminal_info().name,
            running_in_tmux_session,
            tmux_extended_keys_format
        ))
    );

    if tmux_should_enable_modify_other_keys_for(running_in_tmux_session, tmux_extended_keys_format)
    {
        let _ = execute!(writer, EnableModifyOtherKeys);
    }
    tmux_options.mouse_capture
}

fn keyboard_enhancement_flags(
    terminal_name: TerminalName,
    running_in_tmux_session: bool,
    tmux_extended_keys_format: Option<&str>,
) -> KeyboardEnhancementFlags {
    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS;

    // iTerm and Ghostty can leak shortcut release events that the terminal consumes.
    // tmux's xterm key format also loses Shift-Enter when event types are
    // reported. An unavailable/unrecognized tmux probe must take the same safe
    // fallback. Preserve repeat classification on confirmed csi-u transports.
    if matches!(terminal_name, TerminalName::Ghostty | TerminalName::Iterm2)
        || (running_in_tmux_session && !matches!(tmux_extended_keys_format, Some("csi-u")))
    {
        flags
    } else {
        flags | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
    }
}

fn running_in_tmux_session() -> bool {
    tmux_session_detected(
        std::env::var("TMUX").ok().as_deref(),
        std::env::var("TMUX_PANE").ok().as_deref(),
    )
}

fn tmux_session_detected(tmux: Option<&str>, tmux_pane: Option<&str>) -> bool {
    tmux.is_some() || tmux_pane.is_some()
}

fn tmux_should_enable_modify_other_keys_for(
    running_in_tmux_session: bool,
    extended_keys_format: Option<&str>,
) -> bool {
    // Only request mode 2 when tmux confirms csi-u formatting. Older tmux
    // versions do not expose this option and may emit xterm-style sequences,
    // which crossterm does not parse consistently for modified keys.
    running_in_tmux_session && matches!(extended_keys_format, Some("csi-u"))
}

pub(super) fn restore_keyboard_enhancement_stack(writer: &mut impl Write) {
    let _ = execute!(writer, PopKeyboardEnhancementFlags, DisableModifyOtherKeys);
}

pub(super) fn reset_keyboard_reporting_after_exit(writer: &mut impl Write) {
    let _ = execute!(
        writer,
        PopKeyboardEnhancementFlags,
        ResetKeyboardEnhancementFlags,
        DisableModifyOtherKeys
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetKeyboardEnhancementFlags;

impl Command for ResetKeyboardEnhancementFlags {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[<u")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "keyboard enhancement reset is not implemented for the legacy Windows API",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableModifyOtherKeys;

impl Command for EnableModifyOtherKeys {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[>4;2m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "modifyOtherKeys enable is not implemented for the legacy Windows API",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DisableModifyOtherKeys;

impl Command for DisableModifyOtherKeys {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[>4;0m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "modifyOtherKeys reset is not implemented for the legacy Windows API",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}
