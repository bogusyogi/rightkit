//! `chat_term_read` and `chat_term_write`: ChatGPT reads the recent screen text of a chat's live
//! terminal and types into it, through the ChatGPT gateway only.
//!
//! **What can be addressed, and what cannot.** the host app knows a chat's process id (the roster's `pid`:
//! Claude's session file, Muse's hook registration). It has no way into another program's
//! terminal window of its own, and it does not guess. The one exact addressing it has is tmux:
//! when the chat's process is inside a tmux pane (the pane's process is the chat's process or one of
//! its ancestors, checked in the process table), `tmux capture-pane` reads that pane and
//! `tmux send-keys` types into it. Everything else is refused `not_addressable` with the reason:
//! * a chat run in Terminal.app, iTerm2, Ghostty, Warp, VS Code's terminal, Windows Terminal or a
//!   console window: the terminal program owns the screen and offers the host app no API that does not
//!   need that program's own scripting permission (Apple Events) or a console attach that would
//!   share the host app's process console; not built;
//! * a Codex thread: the host app lists Codex threads from its database with no process, so there is
//!   nothing to look up;
//! * Windows: tmux does not run there and no per-console API is safe to use inside the gateway.
//!
//! **Gates**, all checked on every call: the gateway (`remoteGateway`) with `remoteTools`; the
//! terminal family's switch and session grant (`remoteTerminals`, `term::gate`, the same grant
//! `term_exec` uses); the chat is on the approved-chats list (`remoteChatRead`, the one `chat_read`
//! uses; `not_selected` otherwise, before anything about the chat is looked up). The master
//! read-only switch (`remoteReadOnly`) refuses `chat_term_write`. A write also needs one approval
//! for that exact text on that pane (single-use, shown in full, never longer than 1000
//! characters); control characters (Escape, line breaks, Ctrl keys) cannot be typed, and `enter`
//! presses Return as a separate, stated step. Text that looks like a card number, bank detail or
//! government ID is refused (`sensitive_input`).
//!
//! **What leaves the computer.** The screen text passes the secret redactor and is cut to the newest
//! 64 KiB. Activity keeps the tool, the chat kind and a short id prefix, and the byte count.

mod imp {
    use super::super::settings::Settings;
    use super::super::{Origin, Refusal, Result, approvals, chat, redact, term};
    use serde_json::{Value, json};

    /// Most lines of screen history one read returns, and the default.
    const LINES_MAX: u64 = 2000;
    const LINES_DEFAULT: u64 = 200;
    /// Most bytes of screen text returned (the newest).
    const READ_MAX: usize = 64 * 1024;
    /// Longest text one write types: what its approval can show in full.
    const WRITE_MAX: usize = 1000;

    fn invalid(why: impl Into<String>) -> Refusal {
        Refusal::new("invalid_request", why)
    }

    fn not_addressable(why: impl Into<String>) -> Refusal {
        Refusal::new("not_addressable", why)
    }

    /// The chat a call names, after every gate.
    struct Target {
        kind: String,
        id: String,
        pid: u32,
    }

    fn target(settings: &Settings, origin: Origin, args: &Value) -> Result<Target> {
        if origin != Origin::Gateway {
            return Err(Refusal::new(
                "not_built",
                "chat terminals are offered through the ChatGPT gateway only",
            ));
        }
        term::gate(settings, origin)?;
        let named = args["chat"]
            .as_str()
            .map(str::trim)
            .filter(|c| !c.is_empty() && c.len() <= 200)
            .ok_or_else(|| invalid("chat is a chat id from chat_peers"))?;
        let (kind, id) = match named.split_once(':') {
            Some((kind, id)) if chat::CHAT_KINDS.contains(&kind) => (Some(kind), id),
            Some(_) => {
                return Err(Refusal::new(
                    "not_local",
                    "that chat is not on this computer",
                ));
            }
            None => (None, named),
        };
        // The approved-chats list first: an unselected chat never shows whether it exists.
        if !chat::selected(settings, kind, id) {
            return Err(Refusal::new(
                "not_selected",
                "that chat is not selected for remote use; select it in the app's settings (remoteChatRead)",
            ));
        }
        let store = rightkit_chat_bridge::chat::store::Store::open_default().map_err(|e| {
            Refusal::new(
                "read_failed",
                format!("could not open the app's chat store: {}", e.kind()),
            )
        })?;
        let session = rightkit_chat_bridge::chat::roster::local_sessions_with_terminal(&store)
            .into_iter()
            .find(|s| s.id == id && kind.is_none_or(|k| s.kind == k))
            .ok_or_else(|| Refusal::new("not_found", "no chat with that id is running here"))?;
        if !chat::selected(settings, Some(session.kind.as_str()), id) {
            return Err(Refusal::new(
                "not_selected",
                "that chat is not selected for remote use; select it in the app's settings (remoteChatRead)",
            ));
        }
        let pid = session.pid.ok_or_else(|| {
            not_addressable(format!(
                "The app lists this {} chat without a process, so there is no terminal to find",
                session.kind
            ))
        })?;
        Ok(Target {
            kind: session.kind,
            id: id.to_string(),
            pid,
        })
    }

    #[cfg(unix)]
    mod tmux {
        use super::super::super::{Refusal, Result};
        use std::path::PathBuf;
        use std::time::Duration;

        fn binary() -> Option<PathBuf> {
            [
                "/opt/homebrew/bin/tmux",
                "/usr/local/bin/tmux",
                "/usr/bin/tmux",
                "/bin/tmux",
            ]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
        }

        /// One tmux command; its standard output.
        pub fn run(args: &[&str]) -> Result<String> {
            let bin = binary().ok_or_else(|| {
                super::not_addressable(
                    "tmux is not installed here, and tmux is the only terminal the app can address",
                )
            })?;
            let options = rightkit_child_process::RunOptions::new(Duration::from_secs(5))
                .stdout_cap(1024 * 1024)
                .stderr_cap(4096);
            let done = rightkit_child_process::run(&bin, args, &options).map_err(|e| {
                Refusal::new("read_failed", format!("could not run tmux: {}", e.kind()))
            })?;
            if done.timed_out || !done.status.success() {
                return Err(super::not_addressable(
                    "tmux answered with an error (no tmux server is running for this user, or the pane is gone)",
                ));
            }
            Ok(String::from_utf8_lossy(&done.stdout.bytes).into_owned())
        }

        /// The tmux pane whose process is `pid` or the nearest of its ancestors.
        pub fn pane_of(pid: u32) -> Result<String> {
            let listing = run(&["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"])?;
            let panes: Vec<(String, u32)> = listing
                .lines()
                .filter_map(|line| {
                    let (id, pane_pid) = line.split_once(' ')?;
                    let digits = id.strip_prefix('%')?;
                    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                        return None;
                    }
                    Some((id.to_string(), pane_pid.trim().parse().ok()?))
                })
                .collect();
            let chain: Vec<u32> =
                rightkit_system_monitor::process_sampler::with_processes(|system| {
                    let mut chain = vec![pid];
                    let mut at = pid;
                    for _ in 0..64 {
                        let Some(parent) = system
                            .process(sysinfo::Pid::from_u32(at))
                            .and_then(|p| p.parent())
                        else {
                            break;
                        };
                        at = parent.as_u32();
                        if at <= 1 {
                            break;
                        }
                        chain.push(at);
                    }
                    chain
                });
            chain
                .iter()
                .find_map(|p| panes.iter().find(|(_, pane_pid)| pane_pid == p))
                .map(|(id, _)| id.clone())
                .ok_or_else(|| {
                    super::not_addressable(
                        "this chat is not running inside a tmux pane; the app can address a chat's terminal only through tmux (Terminal.app, iTerm2, Ghostty, Warp and the editors' terminals are not addressable)",
                    )
                })
        }
    }

    #[cfg(unix)]
    use tmux::{pane_of, run as tmux_run};

    #[cfg(not(unix))]
    fn pane_of(_pid: u32) -> Result<String> {
        Err(not_addressable(
            "The app can address a chat's terminal only through tmux, which does not run on Windows; no other console is addressable",
        ))
    }

    #[cfg(not(unix))]
    fn tmux_run(_args: &[&str]) -> Result<String> {
        pane_of(0)
    }

    /// `chat_term_read`. `args`: `chat`, `lines` (1 to 2000 lines of history, default 200).
    pub fn read(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
        let target = target(settings, origin, args)?;
        let lines = match &args["lines"] {
            Value::Null => LINES_DEFAULT,
            value => value
                .as_u64()
                .filter(|n| (1..=LINES_MAX).contains(n))
                .ok_or_else(|| invalid(format!("lines is a whole number from 1 to {LINES_MAX}")))?,
        };
        let pane = pane_of(target.pid)?;
        let from = format!("-{lines}");
        let screen = tmux_run(&["capture-pane", "-p", "-J", "-t", &pane, "-S", &from])?;
        let (masked, redactions) = redact::redact(&screen);
        // The newest bytes are kept.
        let mut start = masked.len().saturating_sub(READ_MAX);
        while !masked.is_char_boundary(start) {
            start += 1;
        }
        let truncated = start > 0;
        let text = &masked[start..];
        Ok(json!({
            "ok": true,
            "chat": format!("{}:{}", target.kind, target.id),
            "text": text,
            "lines": lines,
            "truncated": truncated,
            "redactions": redactions,
            "returned": text.len(),
            "bytes": text.len(),
        }))
    }

    /// `chat_term_write`. `args`: `chat`, `text` (1 to 1000 characters, no control characters),
    /// `enter` (press Return afterwards), `approval`.
    pub fn write(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
        let target = target(settings, origin, args)?;
        let text = match &args["text"] {
            Value::String(t)
                if !t.is_empty() && t.len() <= WRITE_MAX && !t.chars().any(char::is_control) =>
            {
                t.as_str()
            }
            _ => {
                return Err(invalid(format!(
                    "text is 1 to {WRITE_MAX} bytes with no control characters (no Escape, line breaks or Ctrl keys); use enter to press Return"
                )));
            }
        };
        let enter = match &args["enter"] {
            Value::Null => false,
            Value::Bool(b) => *b,
            _ => return Err(invalid("enter is true or false")),
        };
        let approval = match &args["approval"] {
            Value::Null => None,
            Value::String(id) => Some(id.as_str()),
            _ => return Err(invalid("approval must be a string")),
        };
        #[cfg(any(target_os = "macos", windows))]
        if super::super::sensitive::sensitive_value(text).is_some() {
            return Err(Refusal::new(
                "sensitive_input",
                "The app does not type a password, card number, bank detail or government ID for an agent",
            ));
        }
        let pane = pane_of(target.pid)?;
        let bind = approvals::bind_of(&[
            b"chat_term.write",
            origin.as_str().as_bytes(),
            target.kind.as_bytes(),
            target.id.as_bytes(),
            pane.as_bytes(),
            text.as_bytes(),
            &[u8::from(enter)],
        ]);
        let short: String = target.id.chars().take(8).collect();
        let summary = format!(
            "type {} characters into the live terminal of the {} chat {short}\u{2026} for ChatGPT (unverified){}: \"{text}\"",
            text.chars().count(),
            target.kind,
            if enter { " and press Return" } else { "" },
        );
        match approval.filter(|a| !a.is_empty()) {
            None => {
                return Err(approvals::request_act_held(
                    origin,
                    "chat_term.write",
                    &bind,
                    &summary,
                ));
            }
            Some(id) => approvals::take_act_held(origin, "chat_term.write", &bind, id)?,
        }
        // The pane is looked up again after the approval: a chat that moved is not typed into.
        if pane_of(target.pid)? != pane {
            return Err(Refusal::new(
                "target_changed",
                "the chat's terminal changed after the approval was asked for; nothing was typed",
            ));
        }
        tmux_run(&["send-keys", "-t", &pane, "-l", "--", text])?;
        if enter {
            tmux_run(&["send-keys", "-t", &pane, "Enter"])?;
        }
        let typed = text.len();
        Ok(json!({
            "ok": true,
            "chat": format!("{}:{}", target.kind, target.id),
            "typed": text.chars().count(),
            "enter": enter,
            "returned": typed,
            "bytes": typed,
        }))
    }
}

pub use imp::{read, write};
