//! The policy of the agent's own browser tools (`browser.start`, `browser.go`, `browser.look`,
//! `browser.do`, `browser.handoff`, `browser.stop`): who may call them, how a browser module error
//! becomes a refusal code, how arguments are checked, and which action asks the owner, is refused
//! as sensitive input, or goes ahead.
//!
//! The browser itself (sessions, refs, the page) stays with the host's browser module; this module
//! holds only the decisions, so the host wires its own tool names, switches and copy around them.
//!
//! Codes: `agent_only`, `feature_off`, `invalid_request`, `approval_required`, `sensitive_input`,
//! `stale_ref`, `no_element`, `ambiguous_target`, `not_interactable`, `action_loop`,
//! `not_approved`, `busy`, `denied`, `expired`, `site_not_approved`, `owner_timeout`,
//! `unsupported`, `browser_unavailable`, `browser_timeout` and `browser_failed`.

use crate::sensitive;
use crate::{Origin, Refusal, Requester, Result};
use serde_json::Value;

/// How long one call waits for the owner (an approval, a sign-in) before it answers `waiting`;
/// the agent repeats the call to wait on.
pub const WAIT_SECS: u64 = 20;
/// Characters of the page's text `browser.look` adds when asked.
pub const TEXT_CHARS: usize = 8_000;

/// An `invalid_request` refusal.
pub fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

/// The refusal for a browser module error given as its code and message. A code outside the known
/// list becomes `browser_failed`.
pub fn fail(code: &str, message: impl Into<String>) -> Refusal {
    let code = match code {
        "feature_off" => "feature_off",
        "unsupported" => "unsupported",
        "invalid_request" => "invalid_request",
        "busy" => "busy",
        "denied" => "denied",
        "expired" => "expired",
        "not_approved" => "not_approved",
        "site_not_approved" => "site_not_approved",
        "stale_ref" => "stale_ref",
        "no_element" => "no_element",
        "ambiguous_target" => "ambiguous_target",
        "not_interactable" => "not_interactable",
        "action_loop" => "action_loop",
        "owner_timeout" => "owner_timeout",
        "browser_unavailable" => "browser_unavailable",
        "browser_timeout" => "browser_timeout",
        _ => "browser_failed",
    };
    Refusal::new(code, message)
}

/// The door check every browser tool shares: a local call by the agent the host started for the
/// owner's own task. `switches` is the host's own switch check (`remoteTools`, `agentBrowser`),
/// run only when the door check passes.
pub fn gate(
    origin: Origin,
    requester: &Requester,
    refusal_text: &str,
    switches: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if origin != Origin::Local || !matches!(requester, Requester::Agent) {
        return Err(Refusal::new("agent_only", refusal_text));
    }
    switches()
}

/// A text argument: trimmed, not empty, at most `max` characters, no control characters.
pub fn text_arg<'a>(args: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.chars().count() <= max && !t.chars().any(char::is_control))
        .ok_or_else(|| invalid(format!("{key} is plain text of at most {max} characters")))
}

/// A session or request id as the browser module mints one: letters, digits and `_`.
pub fn id_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let id = text_arg(args, key, 64)?;
    if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(id)
    } else {
        Err(invalid(format!("{key} is the id browser_start returned")))
    }
}

/// Whether a control's name reads as one that deletes or cancels something that cannot be
/// brought back.
pub fn destructive(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "delete",
        "remove",
        "erase",
        "deactivate",
        "close account",
        "cancel booking",
        "cancel reservation",
        "cancel order",
        "cancel subscription",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// Whether a control's name reads as the one that pays for an order.
pub fn pays(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "pay",
        "place order",
        "place your order",
        "buy",
        "purchase",
        "complete order",
        "submit order",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// What the page's current reading says about the control an action targets.
pub struct Control<'a> {
    /// The control's name (masked secrets cut).
    pub name: &'a str,
    /// A password-like field.
    pub sensitive: bool,
}

/// The action, as far as the decision needs it.
pub enum Intent<'a> {
    /// Typing a value into a control.
    Fill {
        control: &'a Control<'a>,
        text: &'a str,
    },
    /// A click on a control.
    Click { control: &'a Control<'a> },
    /// A key press (Enter submits whatever has focus).
    Press { key: &'a str },
    /// Anything else: hover, select, scroll.
    Other,
}

/// What an action brings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Goes ahead.
    Go,
    /// Asks the owner, bound to the exact control and address.
    Ask,
    /// Refused as `sensitive_input`: the owner types it through the handoff.
    SensitiveInput,
}

/// Decides one action. `pay_cleared` says, only when asked, whether the pay control was cleared of
/// the question (a purchase that asked none, for the same site, within ten minutes).
pub fn decide(intent: &Intent<'_>, pay_cleared: impl FnOnce() -> bool) -> Decision {
    match intent {
        Intent::Fill { control, text } => {
            if control.sensitive
                || sensitive::sensitive_label(control.name)
                || sensitive::sensitive_value(text).is_some()
            {
                Decision::SensitiveInput
            } else {
                Decision::Go
            }
        }
        Intent::Click { control } => {
            let risky = destructive(control.name);
            let submits = sensitive::submit_label(control.name);
            let asks = if submits && !risky && pays(control.name) {
                // The one press of a pay control that follows a purchase which asked no question
                // (at most the card's no-question limit, a card the owner listed for this site).
                !pay_cleared()
            } else {
                submits || risky
            };
            if asks { Decision::Ask } else { Decision::Go }
        }
        Intent::Press { key } => {
            if sensitive::keys_submit(key) {
                Decision::Ask
            } else {
                Decision::Go
            }
        }
        Intent::Other => Decision::Go,
    }
}
