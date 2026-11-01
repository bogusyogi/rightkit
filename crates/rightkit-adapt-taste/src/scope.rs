//! Fail-closed scope normalization and matching (canon §5.4).
//!
//! The Python oracle dropped unknown scope-dimension keys and degraded to
//! "unqualified", which could WIDEN applicability. That is a known defect the
//! native port intentionally corrects: malformed or unknown narrowing
//! dimensions quarantine the candidate; they are never silently dropped.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Canonical scope dimension keys. Adding a key is a schema change.
pub const SCOPE_DIMENSION_KEYS: &[&str] = &[
    "user",
    "org",
    "repo",
    "path_prefix",
    "package",
    "language",
    "framework",
    "task_family",
    "artifact_type",
    "model",
    "client",
    "environment",
    "risk_class",
    "branch",
];

pub const MAX_DIMENSION_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    UnknownDimension { key: String },
    EmptyDimensionValue { key: String },
    DimensionTooLong { key: String, len: usize },
    EmptyKey,
    DuplicateDimension { key: String },
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScopeError::UnknownDimension { key } => write!(f, "unknown scope dimension: {key}"),
            ScopeError::EmptyDimensionValue { key } => {
                write!(f, "empty value for scope dimension: {key}")
            }
            ScopeError::DimensionTooLong { key, len } => {
                write!(
                    f,
                    "scope dimension {key} exceeds {MAX_DIMENSION_CHARS} chars ({len})"
                )
            }
            ScopeError::DuplicateDimension { key } => {
                write!(f, "duplicate normalized scope dimension: {key}")
            }
            ScopeError::EmptyKey => write!(f, "empty scope dimension key"),
        }
    }
}

impl std::error::Error for ScopeError {}

/// Normalized, ordered (by key) set of declared narrowing dimensions.
/// Absent/empty means unqualified — matches every context. A record only ever
/// gets NARROWER by declaring dimensions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeDimensions(BTreeMap<String, String>);

impl ScopeDimensions {
    /// Fail-closed normalization. Unknown keys, empty keys, empty values, and
    /// over-long values are ERRORS, not silent drops.
    pub fn normalize(raw: &BTreeMap<String, String>) -> Result<Self, ScopeError> {
        let mut out = BTreeMap::new();
        for (key, value) in raw {
            let key_norm = key.trim().to_lowercase();
            if key_norm.is_empty() {
                return Err(ScopeError::EmptyKey);
            }
            if !SCOPE_DIMENSION_KEYS.contains(&key_norm.as_str()) {
                return Err(ScopeError::UnknownDimension { key: key_norm });
            }
            let value_norm = value.trim().to_string();
            if value_norm.is_empty() {
                return Err(ScopeError::EmptyDimensionValue { key: key_norm });
            }
            if value_norm.chars().count() > MAX_DIMENSION_CHARS {
                return Err(ScopeError::DimensionTooLong {
                    len: value_norm.chars().count(),
                    key: key_norm,
                });
            }
            if out.insert(key_norm.clone(), value_norm).is_some() {
                return Err(ScopeError::DuplicateDimension { key: key_norm });
            }
        }
        Ok(ScopeDimensions(out))
    }

    pub fn is_unqualified(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter()
    }

    /// Fail-closed matching against a context.
    ///
    /// * Unqualified records match everything (historical corpus semantics).
    /// * Every declared dimension must be satisfied. A dimension the context
    ///   cannot speak to (missing in context) is a NON-match — silently
    ///   applying a narrowed rule is exactly the failure this prevents.
    /// * `path_prefix` matches by normalized prefix; everything else matches
    ///   case-insensitively and exactly.
    pub fn matches(&self, context: &ScopeDimensions) -> bool {
        if self.is_unqualified() {
            return true;
        }
        for (key, wanted) in &self.0 {
            let Some(actual) = context.get(key) else {
                return false;
            };
            let norm = |s: &str| s.replace('\\', "/").to_lowercase();
            if key == "path_prefix" {
                let w = norm(wanted);
                let a = norm(actual);
                let w = w.trim_end_matches('/');
                if a != w && !a.starts_with(&format!("{w}/")) {
                    return false;
                }
            } else if !actual.eq_ignore_ascii_case(wanted) {
                return false;
            }
        }
        true
    }
}
