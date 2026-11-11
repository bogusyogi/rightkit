//! The gate's checks and its one audit row.
//!
//! Check order, never reordered and never bypassable (no parameter, environment variable or cfg
//! skips a step):
//!
//! 1. caller known and not revoked ([`registry::verify_key`](crate::registry::verify_key), or
//!    the door for an app with one owner)          -> [`Denied::Unauthorised`]
//! 2. app unlocked                                  -> [`Denied::Locked`]
//! 3. method in the static table ([`lookup`])       -> [`Denied::UnknownMethod`]
//! 4. caller's level at least the method's ([`allow_level`]) -> [`Denied::ForbiddenLevel`]
//! 5. the scope argument in the caller's list ([`allow_scope`]) -> [`Denied::ScopeNotAllowed`]
//! 6. a read-only door refuses a method that changes things ([`allow_door`]) -> [`Denied::ReadOnly`]
//! 7. the adapter's handler runs (an approval-gated effect files a pending approval in the
//!    `rightkit-approvals` core instead of acting)
//! 8. exactly one audit row, written by the gate, never by a handler ([`conclude`])
//!
//! The method table is the only source of policy for a method; nothing in a request changes it.

use crate::Refusal;

/// A permission level: the caller's, read from the registry row, never from a request; and the
/// minimum a method needs. `Ord` is the privilege order.
pub trait Level: Copy + Ord {
    /// The level every new agent starts with.
    const LOWEST: Self;
    /// The word stored in the registry.
    fn as_str(self) -> &'static str;
    /// The stored word back; `None` for anything unknown.
    fn parse(s: &str) -> Option<Self>;
}

/// An app with one owner and no paired agents has a single level.
impl Level for () {
    const LOWEST: Self = ();
    fn as_str(self) -> &'static str {
        "owner"
    }
    fn parse(s: &str) -> Option<Self> {
        (s == "owner").then_some(())
    }
}

/// One row of an adapter's static method table.
#[derive(Clone, Copy, Debug)]
pub struct Method<P, K> {
    /// The method name a request names.
    pub name: &'static str,
    /// The lowest level that may call it.
    pub min: P,
    /// Whether it takes a scope argument the caller must be allowed.
    pub scoped: bool,
    /// Whether it changes things (files, mail, settings): what a read-only door refuses.
    pub changes: bool,
    /// The adapter's own dispatch tag.
    pub kind: K,
}

/// Why the gate refused before any handler ran. [`Denied::refusal`] gives the stable code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// Unknown or revoked caller.
    Unauthorised,
    /// The app is locked.
    Locked,
    /// No such method in the table.
    UnknownMethod,
    /// The caller's level is below the method's.
    ForbiddenLevel,
    /// The scope argument is not in the caller's list.
    ScopeNotAllowed,
    /// The door is read-only and the method changes things.
    ReadOnly,
}

impl Denied {
    /// The stable code.
    pub fn code(self) -> &'static str {
        match self {
            Denied::Unauthorised => "unauthorised",
            Denied::Locked => "app_locked",
            Denied::UnknownMethod => "unknown_method",
            Denied::ForbiddenLevel => "forbidden_profile",
            Denied::ScopeNotAllowed => "scope_not_allowed",
            Denied::ReadOnly => "read_only_tool",
        }
    }

    /// The refusal with the stable code and a fixed sentence.
    pub fn refusal(self) -> Refusal {
        let message = match self {
            Denied::Unauthorised => "unknown or revoked caller",
            Denied::Locked => "the app is locked",
            Denied::UnknownMethod => "unknown method",
            Denied::ForbiddenLevel => "this caller's profile does not allow that method",
            Denied::ScopeNotAllowed => "this caller may not use that scope",
            Denied::ReadOnly => "this door is read-only",
        };
        Refusal::new(self.code(), message)
    }
}

/// Step 3: the table row for `name`.
pub fn lookup<'t, P, K>(table: &'t [Method<P, K>], name: &str) -> Result<&'t Method<P, K>, Denied> {
    table
        .iter()
        .find(|m| m.name == name)
        .ok_or(Denied::UnknownMethod)
}

/// Step 4: the caller's level against the method's minimum.
pub fn allow_level<P: Level, K>(have: P, method: &Method<P, K>) -> Result<(), Denied> {
    if have < method.min {
        Err(Denied::ForbiddenLevel)
    } else {
        Ok(())
    }
}

/// Step 5: `scope` must be one of `allowed`.
pub fn allow_scope(allowed: &[String], scope: &str) -> Result<(), Denied> {
    if allowed.iter().any(|a| a == scope) {
        Ok(())
    } else {
        Err(Denied::ScopeNotAllowed)
    }
}

/// Step 6: a read-only door refuses a method that changes things.
pub fn allow_door(read_only: bool, changes: bool) -> Result<(), Denied> {
    if read_only && changes {
        Err(Denied::ReadOnly)
    } else {
        Ok(())
    }
}

/// What happens to a result whose audit row could not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logging {
    /// No row, no result: the call answers with the error `unlogged` makes. A refusal stands
    /// either way. MailRight's rule.
    Required,
    /// The row is best effort: the call answers anyway. Pulse's rule for its activity file.
    BestEffort,
}

/// Step 8: one audit row per call. `written` is the outcome of writing the row for `result`.
pub fn conclude<T, E>(
    result: Result<T, E>,
    written: Result<(), String>,
    logging: Logging,
    unlogged: impl FnOnce(String) -> E,
) -> Result<T, E> {
    match (result, written, logging) {
        (Ok(_), Err(e), Logging::Required) => Err(unlogged(e)),
        (result, _, _) => result,
    }
}
