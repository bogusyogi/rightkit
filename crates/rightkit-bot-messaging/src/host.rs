//! Wording and switches the host supplies to the bot layer, each fixed once at start-up.
//!
//! Without a call every value is the one this crate always used, so a host that sets nothing
//! sees the same replies and descriptors as before.

use std::sync::OnceLock;

/// How the channel notes name the host until it calls [`set_product_name`].
pub const DEFAULT_PRODUCT_NAME: &str = "the host";

static PRODUCT_NAME: OnceLock<String> = OnceLock::new();
static FINAL_REPLIES_ONLY: OnceLock<bool> = OnceLock::new();
static APPROVAL_ARGUMENTS: OnceLock<bool> = OnceLock::new();

/// Sets the name of the host in user-visible text (for example `MyApp`: "Telegram messages
/// enter MyApp as remote untrusted content."). Call once at start-up; returns `false` (and
/// changes nothing) when one was already set or `name` is empty.
pub fn set_product_name(name: &str) -> bool {
    !name.is_empty() && PRODUCT_NAME.set(name.to_string()).is_ok()
}

/// The host name in force.
pub fn product_name() -> &'static str {
    PRODUCT_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_PRODUCT_NAME)
}

/// Chooses which assistant messages the Telegram adapter forwards. `false` (the default)
/// forwards every completed assistant message without tool calls. `true` forwards only the one
/// that ends the turn (phase `final`); messages of a turn that continues (a steer, a blocking
/// Stop hook, goal continuation) are skipped. Call once at start-up; returns `false` when one
/// was already set.
pub fn set_final_replies_only(final_only: bool) -> bool {
    FINAL_REPLIES_ONLY.set(final_only).is_ok()
}

/// Whether only turn-ending replies are forwarded.
pub fn final_replies_only() -> bool {
    FINAL_REPLIES_ONLY.get().copied().unwrap_or(false)
}

/// Chooses whether a Telegram approval prompt shows what is being approved. `false` (the
/// default) names the tool and its risk only. `true` adds the command, or the arguments, bounded
/// to 800 characters with control characters dropped. Call once at start-up; returns `false` when
/// one was already set.
pub fn set_approval_arguments(show: bool) -> bool {
    APPROVAL_ARGUMENTS.set(show).is_ok()
}

/// Whether approval prompts show the arguments.
pub fn approval_arguments() -> bool {
    APPROVAL_ARGUMENTS.get().copied().unwrap_or(false)
}
