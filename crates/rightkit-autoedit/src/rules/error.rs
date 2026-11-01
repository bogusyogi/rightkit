use crate::rules::{Parameter, ValueKind};

/// Validation failures for rules and matrices. Every rule-level variant names
/// the rule so a refusal is traceable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    #[error("rule id must not be empty")]
    EmptyId,
    #[error("rule {0} is defined twice")]
    DuplicateId(String),
    #[error("rule {0} has an empty scope name")]
    EmptyScope(String),
    #[error("rule {0} has no provenance (set_by and set_at are required)")]
    MissingProvenance(String),
    #[error("rule {0} has a condition with an empty value")]
    EmptyCondition(String),
    #[error("rule {rule}: {parameter:?} takes {expected:?}, not {got:?}")]
    TypeMismatch {
        rule: String,
        parameter: Parameter,
        expected: ValueKind,
        got: ValueKind,
    },
    #[error("rule {rule}: {parameter:?} must be within {min}..={max}, got {got}")]
    OutOfRange {
        rule: String,
        parameter: Parameter,
        min: i64,
        max: i64,
        got: i64,
    },
    #[error("rule {rule}: {parameter:?} does not accept {value:?}")]
    NotAllowed {
        rule: String,
        parameter: Parameter,
        value: String,
    },
}
