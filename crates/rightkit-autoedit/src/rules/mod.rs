//! Scoped editing rules and their resolution matrix (CutRight #101, #103).
//!
//! A [`Rule`] sets one editing [`Parameter`] to a [`Value`]. It applies to a
//! [`Context`] when its [`Scope`] matches the context and its [`Condition`]
//! holds. [`RuleMatrix::resolve`] picks the effective value per parameter:
//!
//! 1. Narrower scope wins: channel, then content type, then platform, then project.
//! 2. Within one scope rank, the higher `priority` wins.
//! 3. Rules that still tie must agree. Disagreeing top-tier rules are reported as a
//!    [`Conflict`] and the parameter has no effective value.
//!
//! Provenance (who set a rule and when) is recorded for audit and is never a
//! precedence input, so a later rule cannot silently override an earlier one.

mod error;
pub mod ladder;
mod matrix;
mod rule;

pub use error::RuleError;
pub use matrix::{Conflict, Effective, Overridden, Resolution, RuleMatrix};
pub use rule::{
    Condition, Context, Effect, Field, Parameter, Provenance, Rule, Scope, Value, ValueKind,
};
