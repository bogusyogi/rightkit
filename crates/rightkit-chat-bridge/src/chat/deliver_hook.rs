//! Delivery into a chat of a hook harness (Muse, and any later harness whose plugin runs
//! `<cli> chat hook --harness <name>`).
//!
//! Such a chat has no socket or queue to push into. A message is written to the chat's hook inbox
//! (`<state>/chat/hook-inbox/<harness>/<session>/<message-id>.txt`, `codex_hook::enqueue_hook`)
//! and the harness's hook claims it at the chat's next prompt, tool call or end of run, which
//! answers the sender's receipt: `delivered` when the claim follows within a few seconds,
//! `queued` while the chat is busy and has not claimed it yet, `held` when the chat is between
//! turns or has been quiet (the message waits in the hook inbox, and in the chat inbox too). The
//! claim itself sends the sender a `seen` receipt (`codex_hook`, `receipts::seen`).
//!
//! `unsupported` (so the message is kept in the chat inbox) when the harness's hook has never run
//! on this computer: the plugin is not installed or not approved, and nothing would ever claim it.

use super::deliver_claude::shim;
use super::deliver_codex::thread_text;
use super::{ChatError, Envelope, LocalSession, Receipt, Store, codex_hook};
use std::time::Duration;

/// How often, and how many times, an enqueued message is checked for its claim (3 s in all).
const POLL_GAP: Duration = Duration::from_millis(250);
const POLLS: u32 = 12;
/// How recent the last hook run must be, with no end of run after it, for the chat to count as busy.
const ACTIVE_WITHIN: Duration = Duration::from_secs(10 * 60);
/// How recently any chat of the harness must have run the hook for the plugin to count as working.
const PLUGIN_SEEN_WITHIN: Duration = Duration::from_secs(7 * 24 * 3600);

/// What a hook harness's last hook run says its chat is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookState {
    /// A hook ran within `ACTIVE_WITHIN` and was not the end of a run.
    Busy,
    /// The last run ended, or the chat has only started.
    Idle,
    /// The hook ran, but longer ago than `ACTIVE_WITHIN` and not at an end of run.
    Quiet,
    /// The hook never ran for this chat.
    Unknown,
}

pub fn state_of(harness: &str, session: &str) -> HookState {
    match codex_hook::hook_harness_activity(harness, session) {
        None => HookState::Unknown,
        Some(h) if h.ended => HookState::Idle,
        Some(h) if h.age_secs <= ACTIVE_WITHIN.as_secs() => HookState::Busy,
        Some(_) => HookState::Quiet,
    }
}

/// The harness as the people using it write it: its name, capitalised.
fn label(harness: &str) -> String {
    let mut chars = harness.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => "Hook".to_string(),
    }
}

/// Leave `env` in the chat's hook inbox and report how far it got. `Err` only when the inbox
/// could not be written, which the caller keeps as a held message.
pub fn deliver(session: &LocalSession, env: &Envelope) -> Result<Receipt, ChatError> {
    let harness = session.kind.as_str();
    let thread = session.id.as_str();
    let id = shim::env_id(env);
    let label = label(harness);
    let name = label.as_str();
    if !codex_hook::safe_component(thread) {
        return Ok(shim::refused(&format!(
            "This {name} chat's id is not valid."
        )));
    }
    let ran_here = codex_hook::newest_hook_heartbeat(harness).is_some_and(|secs| {
        super::envelope::now_ms() / 1000 <= secs.saturating_add(PLUGIN_SEEN_WITHIN.as_secs())
    });
    if !ran_here {
        let how = if codex_hook::HOOK_HARNESSES.contains(&harness) {
            super::brand::branded(&format!(
                "Install it with `@cli@ chat install --{harness}` and approve it in {name}."
            ))
        } else {
            format!("Install the app plugin in {name} and approve it there.")
        };
        return Ok(shim::unsupported(&format!(
            "the app's {name} plugin has not run on this computer, so nothing would read the message. {how}"
        )));
    }
    let state = state_of(harness, thread);
    codex_hook::enqueue_hook(harness, thread, id, &thread_text(env)).map_err(ChatError::Io)?;
    Ok(match state {
        HookState::Busy => {
            for _ in 0..POLLS {
                std::thread::sleep(POLL_GAP);
                if !codex_hook::pending_hook(harness, thread, id) {
                    return Ok(shim::delivered(&format!(
                        "The {name} chat read the message through the app's {name} hook."
                    )));
                }
            }
            shim::queued(&format!(
                "Waiting in the app for this {name} chat; it reads the message after its next tool call or when its run ends."
            ))
        }
        HookState::Idle => shim::held(&idle_note(harness, name)),
        HookState::Quiet | HookState::Unknown => shim::held(&format!(
            "This {name} chat has not run anything lately; it reads the message at its next prompt or tool call."
        )),
    })
}

/// The receipt for a message to an idle hook-harness chat. Muse alone has a wake switch and a
/// wake entry; a registered harness has neither, so its chat
/// reads the message with its owner's next prompt.
fn idle_note(harness: &str, name: &str) -> String {
    if harness != super::muse::KIND {
        return format!(
            "This {name} chat is between turns; it reads the message with its owner's next prompt."
        );
    }
    let wake_on = Store::open_default().is_ok_and(|store| store.muse_wake_idle());
    if !wake_on {
        "This Muse chat is between turns and waking idle Muse chats is off; it reads the message with its owner's next prompt.".to_string()
    } else if super::muse::muse_wake_command().is_none() {
        "This Muse chat is between turns; waking Muse chats is not available in this Muse version, so it reads the message with its owner's next prompt.".to_string()
    } else {
        "This Muse chat is between turns; it reads the message with its owner's next prompt."
            .to_string()
    }
}
