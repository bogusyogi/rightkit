//! Small helpers shared by the store-bound social-publish modules: the stable error constructors,
//! input parsing, the path-segment and non-empty checks, the author rule and the channel lookup.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::protocol::Scope;
use crate::Fail;

/// `bad_input` with a message.
pub fn bad(msg: impl Into<String>) -> Fail {
    ("bad_input", msg.into())
}

/// `db_error` from any displayable store error.
pub fn db_err(e: impl std::fmt::Display) -> Fail {
    ("db_error", e.to_string())
}

/// `internal` from any displayable error.
pub fn internal(e: impl std::fmt::Display) -> Fail {
    ("internal", e.to_string())
}

/// `not_found` for a missing publish job of the scope.
pub fn not_found(job_id: &str) -> Fail {
    (
        "not_found",
        format!("no publish job {job_id} for this scope"),
    )
}

/// `invalid_transition` with a message.
pub fn invalid_transition(msg: impl Into<String>) -> Fail {
    ("invalid_transition", msg.into())
}

/// Parses a command input into its typed form; a failure is `bad_input` naming `method`.
pub fn parse_input<T: DeserializeOwned>(input: Value, method: &str) -> Result<T, Fail> {
    serde_json::from_value(input).map_err(|e| bad(format!("{method} input: {e}")))
}

/// Serialises an output; a failure is `internal`.
pub fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, Fail> {
    serde_json::to_value(value).map_err(internal)
}

/// `bad_input` unless `value` has a non-blank character.
pub fn non_empty(value: &str, name: &str) -> Result<(), Fail> {
    if value.trim().is_empty() {
        Err(bad(format!("{name} must not be empty")))
    } else {
        Ok(())
    }
}

/// A platform account id becomes a URL path segment, so it is limited to plain characters.
pub fn path_safe(value: &str, name: &str) -> Result<(), Fail> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        return Err(bad(format!(
            "{name} must be letters, digits, '-', '_', '.' or ':', got {value:?}"
        )));
    }
    Ok(())
}

/// The one author rule of the publish commands: `value` (the input field called `field`) must name a
/// known author (`adrian`, `skill` or `daemon`) and be `adrian`, the person. `purpose` completes the
/// sentence "only a person (adrian) ...". Returns the trimmed name; `bad_input` for an empty or unknown
/// name, `author_forbidden` for an agent author.
pub fn require_person(field: &str, value: &str, purpose: &str) -> Result<String, Fail> {
    let name = value.trim();
    if name.is_empty() {
        return Err(bad(format!("{field} must not be empty")));
    }
    match name {
        "adrian" => Ok(name.to_string()),
        "skill" | "daemon" => Err((
            "author_forbidden",
            format!("{field} {name} is an agent author: only a person (adrian) {purpose}"),
        )),
        _ => Err(bad(format!(
            "{field} must be adrian, skill or daemon, got {name}"
        ))),
    }
}

/// The host's stored channels, as the stores check them.
pub trait ChannelLookup {
    /// Whether a stored channel matches `venture` and `channel` exactly (and `account` when given).
    fn channel_known(
        &self,
        venture: &str,
        channel: &str,
        account: Option<&str>,
    ) -> Result<bool, Fail>;
}

/// `foreign_scope` unless the scope exactly matches a stored channel.
pub fn require_channel<S: ChannelLookup + ?Sized>(store: &S, scope: &Scope) -> Result<(), Fail> {
    if !store.channel_known(&scope.venture, &scope.channel, Some(&scope.account))? {
        return Err((
            "foreign_scope",
            format!(
                "no stored channel for venture={} channel={} account={}",
                scope.venture, scope.channel, scope.account
            ),
        ));
    }
    Ok(())
}
