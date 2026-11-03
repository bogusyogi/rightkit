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
