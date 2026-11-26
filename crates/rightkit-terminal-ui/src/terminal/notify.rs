//! Terminal-side attention signals: completion/approval notifications while
//! the terminal is unfocused, and the window title.
//!
//! Pure escape-sequence emission over a caller-supplied writer; the host feeds
//! it focus changes and turn/approval state, and calls
//! [`TerminalNotifier::observe`] once per loop iteration.
//!
//! Configuration (environment, read once by [`TerminalNotifier::from_env`]);
//! `RIGHTKIT` is the default prefix, see [`set_env_prefix`]:
//! - `RIGHTKIT_NOTIFY`: `off`, `bel`, `osc9`, or `auto` (default: OSC 9 on
//!   terminals known to implement it, BEL elsewhere).
//! - `RIGHTKIT_TERMINAL_TITLE`: `off` leaves the title alone.
//! - `RIGHTKIT_PROGRAM_STATUS`: `off` stops the program status report (OSC 7501: working,
//!   blocked on an approval (`kind=permission`), a question (`kind=question`) or a credential
//!   prompt (`kind=auth`), idle, and `done` or `error` for a turn that ended while the terminal
//!   was not in front, per the spec's "result ready, user has not seen it") that terminals and
//!   multiplexers can show per tab; `on`
//!   sends it to any terminal; `auto` (default) sends it unless `TERM` names a terminal that
//!   answers no escape sequences. The protocol requires terminals to ignore a report they do
//!   not know, so no query is sent first.

use std::io::{self, Write};
use std::sync::{OnceLock, RwLock};
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

/// Environment-variable prefix (without the trailing `_`) until the host calls
/// [`set_env_prefix`].
pub const DEFAULT_ENV_PREFIX: &str = "RIGHTKIT";

static ENV_PREFIX: OnceLock<String> = OnceLock::new();

/// Set the host's environment-variable prefix, upper case, no trailing
/// underscore (for example `MYAPP` reads `MYAPP_NOTIFY`). Call once at startup,
/// before the first read of any of this crate's variables; returns `false` if
/// the prefix is empty or one was already set. Same shape as
/// `rightkit_agent_config::set_env_prefix`, which a host sets to the same value.
pub fn set_env_prefix(prefix: &str) -> bool {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return false;
    }
    ENV_PREFIX.set(prefix.to_string()).is_ok()
}

/// The environment-variable prefix in effect.
pub fn env_prefix() -> &'static str {
    ENV_PREFIX
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_ENV_PREFIX)
}

/// `<prefix>_<suffix>`, the environment variable name this crate reads for
/// `suffix` (for example `NOTIFY`).
pub fn env_name(suffix: &str) -> String {
    format!("{}_{suffix}", env_prefix())
}

/// The lower-case form of [`env_prefix`] (`rightkit` by default): the host's
/// command and file-name token, used for the external-editor temp file and for
/// the host's own command in the allow-rule subcommand list.
pub fn app_slug() -> String {
    env_prefix().to_ascii_lowercase()
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
    program_status: bool,
    status_written: Option<ProgramState>,
    /// A turn ended while the terminal was reported as not in front: `Some(failed)` until the user
    /// returns or the next turn starts (OSC 7501 `done` / `error`).
    unseen_result: Option<bool>,
}

/// What the program is blocked on (OSC 7501 `kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Permission,
    Question,
    Auth,
}

/// What the program reports through OSC 7501.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgramState {
    Idle,
    Working,
    Blocked(BlockKind),
    Done,
    Error,
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
            program_status: program_status_enabled(
                std::env::var(env_name("PROGRAM_STATUS"))
                    .ok()
                    .map(|value| value.trim().to_ascii_lowercase())
                    .as_deref(),
                std::env::var("TERM").ok().as_deref(),
                std::env::var_os("TERM_PROGRAM").is_some(),
            ),
            status_written: None,
            unseen_result: None,
        }
    }

    pub fn from_env(cwd: &std::path::Path) -> Self {
        let get = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_ascii_lowercase())
        };
        let method = method_from_setting(
            get(&env_name("NOTIFY")).as_deref(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var_os("WT_SESSION").is_some(),
            std::env::var_os("KITTY_WINDOW_ID").is_some(),
        );
        let title_enabled = !matches!(
            get(&env_name("TERMINAL_TITLE")).as_deref(),
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
    /// pending approval while the terminal is not in front. Same as
    /// [`Self::observe_status`] with an approval as the only block and a turn
    /// that never failed.
    pub fn observe(
        &mut self,
        out: &mut impl Write,
        working: bool,
        approval_pending: bool,
    ) -> io::Result<()> {
        self.observe_status(
            out,
            working,
            approval_pending.then_some(BlockKind::Permission),
            false,
        )
    }

    /// Update from the current turn state: writes the title on a working-state change and a
    /// notification on `working -> idle` or a newly pending approval while the terminal is not in
    /// front. `block` says what a block is; `turn_failed` is read when `working` goes false.
    pub fn observe_status(
        &mut self,
        out: &mut impl Write,
        working: bool,
        block: Option<BlockKind>,
        turn_failed: bool,
    ) -> io::Result<()> {
        let approval_pending = block == Some(BlockKind::Permission);
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
        if self.working && !working && block.is_none() && self.focus_reported && !self.focused {
            self.unseen_result = Some(turn_failed);
        }
        if working || self.focused {
            self.unseen_result = None;
        }
        self.working = working;
        self.approval_pending = approval_pending;

        let mut wrote = false;
        if self.title_enabled && self.title_written != Some(working) {
            self.title_written = Some(working);
            write!(out, "{}", title_sequence(&self.title_leaf, working))?;
            wrote = true;
        }
        if self.program_status {
            let state = if let Some(kind) = block {
                ProgramState::Blocked(kind)
            } else if working {
                ProgramState::Working
            } else if let Some(failed) = self.unseen_result {
                if failed {
                    ProgramState::Error
                } else {
                    ProgramState::Done
                }
            } else {
                ProgramState::Idle
            };
            if self.status_written != Some(state) {
                self.status_written = Some(state);
                write!(out, "{}", program_status_sequence(state))?;
                wrote = true;
            }
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

    /// Restore a neutral title on exit and remove the program status record.
    pub fn clear_title(&mut self, out: &mut impl Write) -> io::Result<()> {
        let mut wrote = false;
        if self.title_enabled && self.title_written.is_some() {
            self.title_written = None;
            write!(out, "\x1b]0;\x07")?;
            wrote = true;
        }
        if self.program_status && self.status_written.take().is_some() {
            write!(out, "\x1b]7501;state=clear\x07")?;
            wrote = true;
        }
        if wrote {
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

/// Whether to send OSC 7501 program status. A terminal that answers no escape sequences
/// (`TERM` empty, `dumb`, `linux`, ...) is skipped unless a program identity says it is a
/// modern terminal; an explicit setting wins.
fn program_status_enabled(
    setting: Option<&str>,
    term: Option<&str>,
    has_term_program: bool,
) -> bool {
    match setting {
        Some("off" | "0" | "false" | "no" | "none") => false,
        Some("on" | "1" | "true" | "yes") => true,
        _ => {
            let term = term.unwrap_or_default().to_ascii_lowercase();
            !matches!(term.as_str(), "" | "dumb" | "linux" | "cons25" | "emacs") || has_term_program
        }
    }
}

/// OSC 7501 report for the root record (`https://www.superlogical.com/rex/docs/build/program-status`):
/// BEL-terminated like the title sequence (the protocol allows ST or BEL).
/// `state=working`, `state=blocked:kind=permission|question|auth` while the program waits on the user,
/// `state=idle` at rest, `state=done` / `state=error` for a turn that ended out of sight.
fn program_status_sequence(state: ProgramState) -> String {
    // The `app=` token is the host's command name (`app_slug`), so a host sees its own name.
    let app = app_slug();
    let pairs = match state {
        ProgramState::Idle => format!("state=idle:app={app}"),
        ProgramState::Working => format!("state=working:app={app}"),
        // msg is base64 of "Approval needed".
        ProgramState::Blocked(BlockKind::Permission) => {
            format!("state=blocked:kind=permission:app={app}:msg=QXBwcm92YWwgbmVlZGVk")
        }
        // msg is base64 of "Question for you".
        ProgramState::Blocked(BlockKind::Question) => {
            format!("state=blocked:kind=question:app={app}:msg=UXVlc3Rpb24gZm9yIHlvdQ==")
        }
        // msg is base64 of "Credential needed".
        ProgramState::Blocked(BlockKind::Auth) => {
            format!("state=blocked:kind=auth:app={app}:msg=Q3JlZGVudGlhbCBuZWVkZWQ=")
        }
        // msg is base64 of "Turn finished".
        ProgramState::Done => format!("state=done:app={app}:msg=VHVybiBmaW5pc2hlZA=="),
        // msg is base64 of "Turn failed".
        ProgramState::Error => format!("state=error:app={app}:msg=VHVybiBmYWlsZWQ="),
    };
    format!("\x1b]7501;{pairs}\x07")
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
