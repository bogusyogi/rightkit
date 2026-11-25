//! The model behind a Claude Desktop account switcher card: a row per Desktop account slot (Main
//! and each added account, the running one ticked), an "Add account" row, and the phase a press
//! moves through (idle, working, waiting for a sign-in, failed).
//!
//! The host runs `<app> claude desktop list|switch|add --json` (the commands over
//! [`crate::desktop`]) and hands the printed JSON here; this module reads the answers and decides
//! the transitions. It draws nothing and words nothing: every message on the card is the host's.
//!
//! Ported from Pulse `windows/src/claude_desktop.rs`.

use std::time::Duration;

use serde_json::Value;

/// Largest command output read.
pub const OUTPUT_MAX_BYTES: usize = 256 * 1024;
/// How old the list may be before drawing the card asks for it again.
pub const REFRESH_AFTER: Duration = Duration::from_secs(10);
/// The same while an account is waiting for its sign-in.
pub const REFRESH_WAITING: Duration = Duration::from_secs(4);
/// How long a failure stays on the card.
pub const RESULT_SHOWN: Duration = Duration::from_secs(6);

/// One Desktop account slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub slot: String,
    pub label: String,
    /// Claude is running on this account.
    pub active: bool,
    /// This account's login is the one in Claude's folder (ticked on the card).
    pub current: bool,
    pub signed_in: bool,
}

/// Where the switcher is. The strings are the host's own words for the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// A switch or an add is running (the text says which).
    Working(String),
    /// A new account's folder is open and waits for its sign-in in the browser.
    Waiting(String),
    Failed(String),
}

impl Phase {
    /// How long the card may keep a list before asking for a fresh one.
    pub fn refresh_limit(&self) -> Duration {
        if matches!(self, Phase::Waiting(_)) {
            REFRESH_WAITING
        } else {
            REFRESH_AFTER
        }
    }

    /// Whether a background list refresh may start now: none is running, no press is working,
    /// and the list held is older than [`Phase::refresh_limit`] (`age` is `None` when never read).
    pub fn wants_refresh(&self, refreshing: bool, age: Option<Duration>) -> bool {
        !refreshing
            && !matches!(self, Phase::Working(_))
            && age.is_none_or(|age| age > self.refresh_limit())
    }

    /// The phase after a fresh list: a sign-in that finished (`waiting` false: the core started
    /// the quit instances again) ends waiting; an account found waiting while idle starts it.
    pub fn after_list(self, waiting: bool) -> Phase {
        match self {
            Phase::Waiting(_) if !waiting => Phase::Idle,
            Phase::Idle if waiting => Phase::Waiting(String::new()),
            other => other,
        }
    }
}

/// Parses command output into JSON; `None` when it is not JSON or is over [`OUTPUT_MAX_BYTES`].
fn parse_output(bytes: &[u8]) -> Option<Value> {
    if bytes.len() > OUTPUT_MAX_BYTES {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}

/// The slots of the output of `desktop list --json`, and whether an account is waiting for its
/// sign-in (`desktop.pending.name`). `None` for output that is not that answer.
pub fn read_list(output: &[u8]) -> Option<(Vec<Slot>, bool)> {
    let value = parse_output(output)?;
    let desktop = value.get("desktop")?;
    let flag = |s: &Value, key: &str| s.get(key).and_then(Value::as_bool);
    let slots = desktop
        .get("slots")?
        .as_array()?
        .iter()
        .filter_map(|s| {
            Some(Slot {
                slot: s.get("slot")?.as_str()?.to_string(),
                label: s.get("label")?.as_str()?.to_string(),
                active: flag(s, "active").unwrap_or(false),
                current: flag(s, "current")
                    .or_else(|| flag(s, "active"))
                    .unwrap_or(false),
                signed_in: flag(s, "signed_in").unwrap_or(false),
            })
        })
        .collect();
    let waiting = desktop
        .get("pending")
        .is_some_and(|p| p.get("name").is_some());
    Some((slots, waiting))
}

/// Whether the output of `desktop switch --json` reports a switch.
pub fn switch_succeeded(output: Option<&[u8]>) -> bool {
    output
        .and_then(parse_output)
        .is_some_and(|v| v.get("switch").is_some())
}

/// Whether the output of `desktop add --json` says the new account waits for its sign-in.
pub fn add_waiting(output: Option<&[u8]>) -> bool {
    output
        .and_then(parse_output)
        .is_some_and(|v| v.pointer("/add/waiting_for_sign_in") == Some(&Value::Bool(true)))
}

/// The first unused name for a new account: `account2`, `account3`, ... not among `taken`
/// (the slot names).
pub fn next_account_name(taken: &[String]) -> String {
    (2..)
        .map(|n| format!("account{n}"))
        .find(|n| !taken.contains(n))
        .unwrap_or_else(|| "account".to_string())
}
