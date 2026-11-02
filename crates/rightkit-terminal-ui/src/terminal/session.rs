//! Terminal session modes, screen lifecycle, and restoration.
//!
//! Lifted from donor `codex-rs/tui/src/tui.rs` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca` (Apache-2.0), reduced to the
//! terminal-session core: mode setup, alternate-scroll commands, screen/keyboard
//! restoration, and Windows virtual-terminal processing. Donor-owned seams that
//! are not absorbed here (event broker, frame requester, suspend/job control,
//! notifications, clipboard worker, stderr guard, pets) remain host/parent
//! concerns; the host app keeps its own one-reader input ownership and frame
//! scheduler.

use std::fmt;
use std::io::Result;
use std::io::Write;
use std::io::stdout;

use crossterm::Command;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::DisableBracketedPaste;
use crossterm::event::DisableFocusChange;
use crossterm::event::EnableBracketedPaste;
#[cfg(not(windows))]
use crossterm::event::EnableFocusChange;
use crossterm::execute;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;

use super::alternate_screen::ALTERNATE_SCREEN;
use super::keyboard_modes;

pub use super::input_boundary::discard_pending_terminal_input;

/// Alternate scroll lets wheel input drive native scrollback on the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EnableAlternateScroll;

impl Command for EnableAlternateScroll {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[?1007h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> Result<()> {
        Err(std::io::Error::other(
            "tried to execute EnableAlternateScroll using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DisableAlternateScroll;

impl Command for DisableAlternateScroll {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[?1007l")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> Result<()> {
        Err(std::io::Error::other(
            "tried to execute DisableAlternateScroll using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RawModeRestore {
    Disable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyboardRestore {
    PopStack,
    ResetAfterExit,
}

#[derive(Clone, Copy)]
pub(crate) enum TerminalHandoff {
    Restore,
}

/// The current screen's pointer policy; ordinary pickers retain alternate-scroll input.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg(all(test, unix))]
pub(crate) enum OverlayInput {
    #[default]
    Default,
    Onboarding,
    Transcript,
    StaticPager,
    Usage,
}

#[cfg(all(test, unix))]
impl OverlayInput {
    pub(crate) fn captures_mouse(self, owned_screen: bool) -> bool {
        match self {
            Self::Default => owned_screen,
            Self::Onboarding | Self::StaticPager => false,
            Self::Transcript | Self::Usage => true,
        }
    }
}

/// Enter a host-owned modal with the donor's independent screen/input stack.
pub fn enter_alternate_screen(writer: &mut impl std::io::Write) -> Result<()> {
    ALTERNATE_SCREEN.enter(writer, false)
}

/// Enter persistent fullscreen chat with pointer reporting enabled.
pub fn enter_fullscreen_screen(writer: &mut impl std::io::Write) -> Result<()> {
    ALTERNATE_SCREEN.enter(writer, true)
}

/// Return to inline output, popping alternate input modes before leaving.
pub fn leave_alternate_screen(writer: &mut impl std::io::Write) -> Result<()> {
    ALTERNATE_SCREEN.leave(writer)
}

/// Enter the modes the TUI owns for the lifetime of a session.
///
/// Inverse is [`restore`] / [`restore_after_exit`]; panic paths are covered by
/// `input_boundary::TerminalInitializationGuard` and the host's panic hook.
pub fn set_modes() -> Result<()> {
    ensure_virtual_terminal_processing()?;

    execute!(stdout(), EnableBracketedPaste)?;

    enable_raw_mode()?;
    #[cfg(windows)]
    super::windows_console::set_input_record_mode()?;
    // Enable keyboard enhancement flags so modifiers for keys like Enter are
    // disambiguated. Some terminals (notably legacy Windows consoles) do not
    // support keyboard enhancement flags; attempt it but continue gracefully.
    keyboard_modes::enable_keyboard_enhancement(&mut stdout());

    #[cfg(not(windows))]
    let _ = execute!(stdout(), EnableFocusChange);
    #[cfg(windows)]
    let _ = execute!(stdout(), DisableFocusChange);
    Ok(())
}

pub(super) fn restore_common(
    raw_mode_restore: RawModeRestore,
    keyboard_restore: KeyboardRestore,
    handoff: TerminalHandoff,
) -> Result<()> {
    let mut first_error = ensure_virtual_terminal_processing().err();

    let screen_result = match handoff {
        TerminalHandoff::Restore => ALTERNATE_SCREEN.restore(&mut stdout(), keyboard_restore),
    };
    if let Err(err) = screen_result {
        first_error.get_or_insert(err);
    }

    if let Err(err) = execute!(stdout(), DisableBracketedPaste) {
        first_error.get_or_insert(err);
    }
    let _ = execute!(stdout(), DisableFocusChange);
    if matches!(raw_mode_restore, RawModeRestore::Disable)
        && let Err(err) = disable_raw_mode()
    {
        first_error.get_or_insert(err);
    }
    #[cfg(windows)]
    if let Err(err) = super::windows_console::restore_input_mode() {
        first_error.get_or_insert(err);
    }
    if let Err(err) = execute!(
        stdout(),
        SetCursorStyle::DefaultUserShape,
        crossterm::cursor::Show
    ) {
        first_error.get_or_insert(err);
    }
    // Crossterm's Windows focus reset is a WinAPI no-op. Send the VT
    // inverse after console-mode restoration too, so ConPTY cannot leave
    // reporting enabled on the terminal handed back to the shell/editor.
    let mut output = stdout();
    if let Err(err) = output
        .write_all(b"\x1b[?1004l\x1b[?2004l")
        .and_then(|()| output.flush())
    {
        first_error.get_or_insert(err);
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Restore the terminal to its original state. Inverse of [`set_modes`].
pub fn restore() -> Result<()> {
    restore_common(
        RawModeRestore::Disable,
        KeyboardRestore::PopStack,
        TerminalHandoff::Restore,
    )
}

/// Restore the terminal after the process is exiting.
///
/// Uses a stronger keyboard reset than [`restore`] so the parent shell recovers
/// even if a terminal missed the stack pop that normally pairs with
/// [`set_modes`]. The donor also finishes its stderr guard here; the host app's
/// stderr policy is host-owned.
pub fn restore_after_exit() -> Result<()> {
    restore_common(
        RawModeRestore::Disable,
        KeyboardRestore::ResetAfterExit,
        TerminalHandoff::Restore,
    )
}

/// Ensure both stdout and stderr have virtual-terminal processing on Windows.
/// ConPTY on some Windows builds starts with it off, which renders raw escape
/// bytes instead of colors. A no-op elsewhere.
#[cfg(windows)]
fn ensure_virtual_terminal_processing() -> Result<()> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::ENABLE_PROCESSED_OUTPUT;
    use windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    use windows_sys::Win32::System::Console::GetConsoleMode;
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
    use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;
    use windows_sys::Win32::System::Console::SetConsoleMode;

    fn enable_for_handle(handle: HANDLE) -> Result<()> {
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Ok(());
        }

        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Ok(());
        }

        let requested = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if mode & requested == requested {
            return Ok(());
        }

        if unsafe { SetConsoleMode(handle, mode | requested) } == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(())
    }

    let stdout_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    enable_for_handle(stdout_handle)?;

    let stderr_handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    enable_for_handle(stderr_handle)?;

    Ok(())
}

#[cfg(not(windows))]
fn ensure_virtual_terminal_processing() -> Result<()> {
    Ok(())
}
