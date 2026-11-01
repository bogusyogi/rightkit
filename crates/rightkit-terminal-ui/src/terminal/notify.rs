//! Terminal-side attention signals: completion/approval notifications while
//! the terminal is unfocused, and the window title.
//!
//! Pure escape-sequence emission over a caller-supplied writer; the host feeds
//! it focus changes and turn/approval state, and calls
//! [`TerminalNotifier::observe`] once per loop iteration.
//!
//! Configuration (environment, read once by [`TerminalNotifier::from_env`]):
//! - `RIGHTKIT_NOTIFY`: `off`, `bel`, `osc9`, or `auto` (default: OSC 9 on
//!   terminals known to implement it, BEL elsewhere).
//! - `RIGHTKIT_TERMINAL_TITLE`: `off` leaves the title alone.

use std::io::{self, Write};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// Product name shown in the window title and notifications until the host
/// calls [`set_app_name`].
pub const DEFAULT_APP_NAME: &str = "Agent";

static APP_NAME: RwLock<Option<String>> = RwLock::new(None);

/// Set the product name used in titles (`<name> · <leaf>`) and notifications
/// (`<name> finished`, `<name> needs approval`).
pub fn set_app_name(name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    if let Ok(mut slot) = APP_NAME.write() {
        *slot = Some(name.to_string());
    }
}

/// The current product name.
pub fn app_name() -> String {
    APP_NAME
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| DEFAULT_APP_NAME.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyMethod {
    Off,
    Bel,
    Osc9,
}

/// A turn must run at least this long before a notification is worth sending
/// when focus is not being reported (Windows consoles never report it).
const BLIND_FOCUS_MIN_TURN: Duration = Duration::from_secs(10);

pub struct TerminalNotifier {
    method: NotifyMethod,
    title_enabled: bool,
    title_leaf: String,
    focused: bool,
    focus_reported: bool,
    working: bool,
    working_since: Option<Instant>,
    approval_pending: bool,
    title_written: Option<bool>,
}

impl TerminalNotifier {
    pub fn new(method: NotifyMethod, title_enabled: bool, cwd: &std::path::Path) -> Self {
        let title_leaf = cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| crate::render::display_path_str(&cwd.display().to_string()));
        Self {
            method,
            title_enabled,
            title_leaf,
            focused: true,
            focus_reported: false,
            working: false,
            working_since: None,
            approval_pending: false,
            title_written: None,
        }
    }

    pub fn from_env(cwd: &std::path::Path) -> Self {
        let get = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_ascii_lowercase())
        };
        let method = method_from_setting(
            get("RIGHTKIT_NOTIFY").as_deref(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var_os("WT_SESSION").is_some(),
            std::env::var_os("KITTY_WINDOW_ID").is_some(),
        );
        let title_enabled = !matches!(
            get("RIGHTKIT_TERMINAL_TITLE").as_deref(),
            Some("off" | "0" | "false" | "no")
        );
        Self::new(method, title_enabled, cwd)
    }

    pub fn set_focus(&mut self, focused: bool) {
        self.focus_reported = true;
        self.focused = focused;
    }

    /// Update from the current turn/approval state, writing the title on a
    /// working-state change and a notification on `working -> idle` or a newly
    /// pending approval while the terminal is not in front.
    pub fn observe(
        &mut self,
        out: &mut impl Write,
        working: bool,
        approval_pending: bool,
    ) -> io::Result<()> {
        let now = Instant::now();
        let mut notice: Option<String> = None;
        if working && !self.working {
            self.working_since = Some(now);
        }
        let long_enough = self
            .working_since
            .is_some_and(|since| now.duration_since(since) >= BLIND_FOCUS_MIN_TURN);
        if approval_pending && !self.approval_pending && self.should_notify(true) {
            notice = Some(format!("{} needs approval", app_name()));
        } else if !working && self.working && !approval_pending && self.should_notify(long_enough) {
            notice = Some(format!("{} finished", app_name()));
        }
        if !working {
            self.working_since = None;
        }
        self.working = working;
        self.approval_pending = approval_pending;

        let mut wrote = false;
        if self.title_enabled && self.title_written != Some(working) {
            self.title_written = Some(working);
            write!(out, "{}", title_sequence(&self.title_leaf, working))?;
            wrote = true;
        }
        if let Some(message) = notice {
            write!(out, "{}", notification_sequence(self.method, &message))?;
            wrote = true;
        }
        if wrote {
            out.flush()?;
        }
        Ok(())
    }

    /// Restore a neutral title on exit.
    pub fn clear_title(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.title_enabled && self.title_written.is_some() {
            self.title_written = None;
            write!(out, "\x1b]0;\x07")?;
            out.flush()?;
        }
        Ok(())
    }

    fn should_notify(&self, blind_focus_ok: bool) -> bool {
        if self.method == NotifyMethod::Off {
            return false;
        }
        if self.focus_reported {
            !self.focused
        } else {
            blind_focus_ok
        }
    }
}

fn method_from_setting(
    setting: Option<&str>,
    term_program: Option<&str>,
    windows_terminal: bool,
    kitty: bool,
) -> NotifyMethod {
    match setting {
        Some("off" | "0" | "false" | "no" | "none") => NotifyMethod::Off,
        Some("bel") => NotifyMethod::Bel,
        Some("osc9" | "osc-9") => NotifyMethod::Osc9,
        _ => {
            let osc9 = windows_terminal
                || kitty
                || matches!(
                    term_program,
                    Some("iterm.app" | "wezterm" | "ghostty" | "iTerm.app" | "WezTerm" | "Ghostty")
                );
            if osc9 {
                NotifyMethod::Osc9
            } else {
                NotifyMethod::Bel
            }
        }
    }
}

/// `<app name> · <leaf>` (see [`set_app_name`]), prefixed with a working marker while a turn runs.
pub fn title_sequence(leaf: &str, working: bool) -> String {
    let leaf = sanitize(leaf);
    let marker = if working { "● " } else { "" };
    format!("\x1b]0;{marker}{} · {leaf}\x07", app_name())
}

pub fn notification_sequence(method: NotifyMethod, message: &str) -> String {
    match method {
        NotifyMethod::Off => String::new(),
        NotifyMethod::Bel => "\x07".to_string(),
        NotifyMethod::Osc9 => format!("\x1b]9;{}\x07", sanitize(message)),
    }
}

/// Control characters would terminate or corrupt the OSC payload.
fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn run(n: &mut TerminalNotifier, working: bool, approval: bool) -> String {
        let mut out = Vec::new();
        n.observe(&mut out, working, approval).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn setting_selects_the_method_and_auto_detects_osc9_terminals() {
        use NotifyMethod::*;
        assert_eq!(method_from_setting(Some("off"), None, false, false), Off);
        assert_eq!(method_from_setting(Some("bel"), None, true, false), Bel);
        assert_eq!(method_from_setting(Some("osc9"), None, false, false), Osc9);
        assert_eq!(method_from_setting(None, None, false, false), Bel);
        assert_eq!(method_from_setting(Some("auto"), None, true, false), Osc9);
        assert_eq!(
            method_from_setting(None, Some("WezTerm"), false, false),
            Osc9
        );
        assert_eq!(method_from_setting(None, None, false, true), Osc9);
    }

    #[test]
    fn sequences_are_well_formed_and_strip_control_characters() {
        assert_eq!(notification_sequence(NotifyMethod::Bel, "x"), "\x07");
        assert_eq!(notification_sequence(NotifyMethod::Off, "x"), "");
        assert_eq!(
            notification_sequence(NotifyMethod::Osc9, "done\x1b]0;evil\x07"),
            "\x1b]9;done]0;evil\x07"
        );
        assert_eq!(title_sequence("repo", true), "\x1b]0;● Agent · repo\x07");
        assert_eq!(title_sequence("repo", false), "\x1b]0;Agent · repo\x07");
    }

    #[test]
    fn unfocused_turn_completion_notifies_and_focused_does_not() {
        let mut n = TerminalNotifier::new(NotifyMethod::Osc9, false, Path::new("/w/repo"));
        n.set_focus(false);
        assert_eq!(run(&mut n, true, false), "");
        assert_eq!(run(&mut n, false, false), "\x1b]9;Agent finished\x07");

        let mut focused = TerminalNotifier::new(NotifyMethod::Osc9, false, Path::new("/w/repo"));
        focused.set_focus(true);
        run(&mut focused, true, false);
        assert_eq!(run(&mut focused, false, false), "");
    }

    #[test]
    fn approval_prompt_notifies_once_and_off_is_silent() {
        let mut n = TerminalNotifier::new(NotifyMethod::Bel, false, Path::new("/w/repo"));
        n.set_focus(false);
        run(&mut n, true, false);
        assert_eq!(run(&mut n, true, true), "\x07");
        assert_eq!(run(&mut n, true, true), "");

        let mut off = TerminalNotifier::new(NotifyMethod::Off, false, Path::new("/w/repo"));
        off.set_focus(false);
        run(&mut off, true, false);
        assert_eq!(run(&mut off, false, false), "");
    }

    #[test]
    fn short_turn_without_focus_reports_stays_quiet_and_title_tracks_state() {
        let mut n = TerminalNotifier::new(NotifyMethod::Bel, true, Path::new("/w/repo"));
        assert_eq!(run(&mut n, true, false), title_sequence("repo", true));
        // Focus never reported and the turn was shorter than the blind minimum.
        assert_eq!(run(&mut n, false, false), title_sequence("repo", false));
        let mut cleared = Vec::new();
        n.clear_title(&mut cleared).unwrap();
        assert_eq!(cleared, b"\x1b]0;\x07");
    }
}
