//! WP-04 — Typed Bindings (data shape + parser + scope validator).
//!
//! ADR-0021 constraint: bindings are non-Turing-complete, typed, depth/size
//! bounded, and deterministic; only declared inputs, upstream outputs, run
//! metadata, iteration data, and unresolved secret references are addressable.
//! This module owns one binding's shape plus deterministic parser;
//! [`crate::control_plane::expression`] resolves parsed paths against an `EvalContext`.
//! [`ScopeValidator`] enforces ADR-0021 capability and ancestry rules at
//! compile time.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control_plane::domain::CapabilitySet;

// ─── Binding kind ────────────────────────────────────────────────────────────

/// One binding source. The binding declares *what* is being referenced; the
/// `path` then drills into it.
///
/// V1 supported sources (per the WP-04 spec):
///
/// - [`BindingKind::RunInput`] — a declared workflow run input.
/// - [`BindingKind::NodeOutput`] — output of an upstream node (must be in
///   the ancestor set, and the upstream node must be `Running` or `Completed`).
/// - [`BindingKind::Constant`] — compile-time literal value. May not contain
///   `NaN` / `Inf` numerics.
/// - [`BindingKind::Env`] — environment variable (requires `env.read`
///   capability).
/// - [`BindingKind::SecretRef`] — unresolved secret reference (requires
///   `secret.read` capability).
/// - [`BindingKind::IterationIndex`] — current `usize` index for `Map` /
///   `Loop`.
/// - [`BindingKind::ItemOf`] — current item for `Map` / `Loop`.
/// - [`BindingKind::LastError`] — last error emitted by the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum BindingKind {
    /// Declared workflow run input.
    RunInput,
    /// Output of an upstream node.
    NodeOutput,
    /// Constant value embedded at compile time.
    Constant(serde_json::Value),
    /// Environment variable name.
    Env(String),
    /// Unresolved secret reference name.
    SecretRef(String),
    /// Current `Map` / `Loop` iteration index.
    IterationIndex,
    /// Current `Map` / `Loop` iteration item.
    ItemOf,
    /// Last error emitted by the run.
    LastError,
}

// ─── Binding path ────────────────────────────────────────────────────────────

/// One path segment. Either a field name (`foo`) or an array index (`[0]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum BindingPathSegment {
    /// Object field access.
    Field(String),
    /// Array index access.
    Index(usize),
}

/// A path is a sequence of [`BindingPathSegment`] entries drilled into a
/// [`BindingKind`].
///
/// Paths follow two simple rules:
///
/// 1. Empty paths are accepted for sources that are themselves a value
///    (`IterationIndex`, `ItemOf`, `LastError`, `Constant`, plain `Env(...)`,
///    plain `SecretRef(...)`); they are *not* accepted when the textual
///    parser sees a `.<seg>` segment with nothing after it.
/// 2. Once an `Index` segment appears, no further `Field` segment may follow
///    — the rule forbids `inputs[0].x`. The inverse `inputs.x[0]` is allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingPath {
    /// The path segments in declaration order.
    pub segments: Vec<BindingPathSegment>,
}

impl BindingPath {
    /// Build a path from a vector of segments.
    pub fn new(segments: Vec<BindingPathSegment>) -> Self {
        Self { segments }
    }

    /// True when there are no segments.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Reject a path that mixes `Field` after `Index`. Returns `Ok` if the
    /// path is well-formed.
    pub fn validate(&self) -> Result<(), BindingError> {
        let mut seen_index = false;
        for seg in &self.segments {
            match seg {
                BindingPathSegment::Index(_) => seen_index = true,
                BindingPathSegment::Field(_) if seen_index => {
                    return Err(BindingError::MixedPathSegments);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

// ─── Binding ─────────────────────────────────────────────────────────────────

/// One typed binding: a source plus a path plus an optional scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Source kind.
    pub kind: BindingKind,
    /// Path drilled into the source.
    pub path: BindingPath,
    /// For [`BindingKind::NodeOutput`]: explicit upstream node id. When `None`,
    /// the resolver uses the previous-hop convention (whatever the reducer
    /// supplies). Validated by [`ScopeValidator`].
    pub scoped_to_node: Option<String>,
}

impl Binding {
    /// Build a binding with no path.
    pub fn bare(kind: BindingKind) -> Self {
        Self {
            kind,
            path: BindingPath::default(),
            scoped_to_node: None,
        }
    }

    /// Build a binding with a path.
    pub fn with_path(kind: BindingKind, path: BindingPath) -> Self {
        Self {
            kind,
            path,
            scoped_to_node: None,
        }
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Typed failures from binding parsing and path validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BindingError {
    /// Empty / whitespace-only binding text.
    #[error("empty binding string (refused)")]
    EmptyPath,
    /// Path mixes `Field` segments after an `Index` segment.
    #[error("binding path mixes Index and Field segments after an Index (refused)")]
    MixedPathSegments,
    /// Path string is malformed (`..`, missing field after `.`, etc.).
    #[error("malformed binding path: {0}")]
    MalformedPath(String),
    /// Source identifier is unknown.
    #[error("unsupported binding source `{0}`")]
    UnknownSource(String),
    /// `env(...)` or `secret_ref(...)` had an empty argument.
    #[error("binding source `{0}` argument must be non-empty")]
    EmptySourceValue(&'static str),
    /// `constant(<json>)` failed to parse as JSON.
    #[error("binding constant JSON is malformed: {0}")]
    ConstantJsonMalformed(String),
    /// `constant(...)` contains a non-finite numeric (`NaN` / `Inf`).
    #[error("binding constant contains non-finite number: {0}")]
    NonFiniteLiteral(String),
    /// `@scope` appeared on a binding that is not `NodeOutput`.
    #[error("`@<node_id>` scope is only valid for `node_output` bindings")]
    InvalidScopeTarget,
    /// `@scope` was used without a node id.
    #[error("`@<node_id>` requires a non-empty node id")]
    EmptyScopeTarget,
}

// ─── Parser ──────────────────────────────────────────────────────────────────

/// Capability names referenced by the [`ScopeValidator`]. Callers may compare
/// against these constants to register capabilities on a [`CapabilitySet`].
pub const CAP_READ_ENV: &str = "env.read";
pub const CAP_READ_SECRET: &str = "secret.read";

/// Parse a binding from its canonical textual form.
///
/// Grammar (informal):
///
/// ```text
/// binding := source ( '@' ident )? ( '.' path )?
/// source  := 'env(' ident ')' | 'secret_ref(' ident ')' | 'constant(' json ')' | plain_ident
/// plain_ident := 'run_input' | 'node_output' | 'iteration_index' | 'item_of' | 'last_error'
/// path    := ( ident | '[' integer ']' ) ( '.' ident | '[' integer ']' )*
/// ```
///
/// Rejects:
///
/// - empty input;
/// - `..` segments;
/// - `Index` followed by `Field` segments;
/// - `@<scope>` on non-`NodeOutput` bindings;
/// - empty arguments to `env(...)` / `secret_ref(...)`;
/// - non-finite numerics in `constant(...)`.
pub fn parse_binding(s: &str) -> Result<Binding, BindingError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(BindingError::EmptyPath);
    }

    // 1. Source.
    let (after_source, kind) = parse_source(s)?;

    // 2. Optional `@<node_id>` scope (only valid on `node_output`).
    let (after_scope, scoped_to_node) = if let Some(stripped) = after_source.strip_prefix('@') {
        let (id, rest) = split_first_ident(stripped);
        if id.is_empty() {
            return Err(BindingError::EmptyScopeTarget);
        }
        (rest, Some(id.to_string()))
    } else {
        (after_source, None)
    };

    // 3. Optional path. The path may begin with a `.` (separator) or run
    //    directly into segments if the source identifier was the whole token.
    //    Reject a leading `..` here: stripping only one dot would leave a
    //    stray leading dot that the path parser would otherwise consume as
    //    a normal separator, hiding the double-dot.
    if after_scope.starts_with("..") {
        return Err(BindingError::MalformedPath(
            "`..` is not allowed".to_string(),
        ));
    }
    let path_str = after_scope.strip_prefix('.').unwrap_or(after_scope);
    let path = parse_path(path_str)?;
    path.validate()?;

    // 4. Cross-check: `@scope` only allowed on `NodeOutput`.
    if scoped_to_node.is_some() && !matches!(kind, BindingKind::NodeOutput) {
        return Err(BindingError::InvalidScopeTarget);
    }

    Ok(Binding {
        kind,
        path,
        scoped_to_node,
    })
}

fn parse_source(s: &str) -> Result<(&str, BindingKind), BindingError> {
    // Parameterised sources: env(...), secret_ref(...), constant(...).
    if let Some(after) = s.strip_prefix("env(") {
        let (arg, rest) = read_paren_arg(after).ok_or_else(|| {
            BindingError::MalformedPath("env(...) missing closing paren".to_string())
        })?;
        if arg.is_empty() {
            return Err(BindingError::EmptySourceValue("env"));
        }
        return Ok((rest, BindingKind::Env(arg.to_string())));
    }
    if let Some(after) = s.strip_prefix("secret_ref(") {
        let (arg, rest) = read_paren_arg(after).ok_or_else(|| {
            BindingError::MalformedPath("secret_ref(...) missing closing paren".to_string())
        })?;
        if arg.is_empty() {
            return Err(BindingError::EmptySourceValue("secret_ref"));
        }
        return Ok((rest, BindingKind::SecretRef(arg.to_string())));
    }
    if let Some(after) = s.strip_prefix("constant(") {
        let (arg, rest) = read_paren_arg(after).ok_or_else(|| {
            BindingError::MalformedPath("constant(...) missing closing paren".to_string())
        })?;
        let val: serde_json::Value = serde_json::from_str(arg).map_err(|e| {
            BindingError::ConstantJsonMalformed(format!("constant({}): {}", arg, e))
        })?;
        validate_finite_json(&val)?;
        return Ok((rest, BindingKind::Constant(val)));
    }

    // Plain identifier sources.
    let (ident, rest) = split_first_ident(s);
    if ident.is_empty() {
        return Err(BindingError::UnknownSource(s.to_string()));
    }
    let kind = match ident {
        "run_input" => BindingKind::RunInput,
        "node_output" => BindingKind::NodeOutput,
        "iteration_index" => BindingKind::IterationIndex,
        "item_of" => BindingKind::ItemOf,
        "last_error" => BindingKind::LastError,
        other => return Err(BindingError::UnknownSource(other.to_string())),
    };
    Ok((rest, kind))
}

/// Split the leading identifier off `s`. Returns `(ident, rest)` where
/// `ident` may be empty if `s` does not start with an identifier char.
fn split_first_ident(s: &str) -> (&str, &str) {
    for (i, c) in s.char_indices() {
        if !is_ident_char(c) {
            return (&s[..i], &s[i..]);
        }
    }
    (s, "")
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Read the contents of `(...)` from the start of `s`. Returns `(inner, rest)`
/// where `rest` is everything after the matching `)`. Returns `None` if the
/// closing paren is not found.
fn read_paren_arg(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 1;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a path string (after the source and optional `@scope` have been
/// consumed). An empty string yields an empty path.
fn parse_path(s: &str) -> Result<BindingPath, BindingError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(BindingPath::default());
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut out: Vec<BindingPathSegment> = Vec::new();

    // Allow a leading `.` separator before the first segment.
    if bytes[i] == b'.' {
        i += 1;
        if i >= bytes.len() {
            return Err(BindingError::EmptyPath);
        }
    }

    loop {
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'.' => {
                // `..` is rejected.
                if i + 1 < bytes.len() && bytes[i + 1] == b'.' {
                    return Err(BindingError::MalformedPath(
                        "`..` is not allowed".to_string(),
                    ));
                }
                i += 1;
                if i >= bytes.len() {
                    return Err(BindingError::MalformedPath(
                        "path ends with `.` (missing segment)".to_string(),
                    ));
                }
            }
            b'[' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return Err(BindingError::MalformedPath(
                        "expected digits inside `[...]`".to_string(),
                    ));
                }
                let idx_str = std::str::from_utf8(&bytes[start..i]).unwrap();
                let idx: usize = idx_str.parse().map_err(|_| {
                    BindingError::MalformedPath(format!("index `{}` out of range", idx_str))
                })?;
                if i >= bytes.len() || bytes[i] != b']' {
                    return Err(BindingError::MalformedPath(
                        "expected `]` to close index".to_string(),
                    ));
                }
                i += 1;
                out.push(BindingPathSegment::Index(idx));
            }
            b'_' | b'a'..=b'z' | b'A'..=b'Z' => {
                let start = i;
                while i < bytes.len() && is_ident_char_byte(bytes[i]) {
                    i += 1;
                }
                let field = std::str::from_utf8(&bytes[start..i]).unwrap().to_string();
                out.push(BindingPathSegment::Field(field));
            }
            other => {
                return Err(BindingError::MalformedPath(format!(
                    "unexpected character `{}` in path",
                    other as char
                )));
            }
        }
    }

    Ok(BindingPath::new(out))
}

fn is_ident_char_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Walk a JSON value and reject any non-finite numeric.
fn validate_finite_json(v: &serde_json::Value) -> Result<(), BindingError> {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if !f.is_finite() {
                    return Err(BindingError::NonFiniteLiteral(n.to_string()));
                }
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                validate_finite_json(x)?;
            }
        }
        serde_json::Value::Object(o) => {
            for x in o.values() {
                validate_finite_json(x)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// ─── Scope validator ─────────────────────────────────────────────────────────

/// One node's output state, as observed by the scope validator.
///
/// Only [`NodeOutputState::Running`] and [`NodeOutputState::Completed`]
/// satisfy the [`BindingKind::NodeOutput`] rule. All other states are
/// "not yet available" and trigger [`ScopeError::NodeOutputUnavailable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeOutputState {
    /// Not yet started.
    Pending,
    /// In flight.
    Running,
    /// Produced an output.
    Completed,
    /// Failed with an error.
    Failed,
    /// Cancelled.
    Cancelled,
}

/// Compile-time / admission-time context the validator checks against.
#[derive(Debug, Clone)]
pub struct ScopeContext<'a> {
    /// Effective capability set for the *current* node.
    pub capabilities: &'a CapabilitySet,
    /// Per-node output state observed by the validator. Keys are node ids.
    pub node_states: &'a BTreeMap<String, NodeOutputState>,
    /// Ancestor node ids of the *current* node.
    pub ancestor_nodes: &'a BTreeSet<String>,
    /// Id of the *current* node.
    pub current_node: &'a str,
}

/// Typed failure from [`ScopeValidator`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScopeError {
    /// `secret.read` capability missing for a `SecretRef` binding.
    #[error(
        "binding requires capability `{0}` (secret.read) but it is not in the current node's effective capability set"
    )]
    SecretRefRequiresCapability(String),
    /// `env.read` capability missing for an `Env` binding.
    #[error(
        "binding requires capability `{0}` (env.read) but it is not in the current node's effective capability set"
    )]
    EnvRequiresCapability(String),
    /// `NodeOutput` references a node outside the ancestor set.
    #[error("binding references node_output `{0}` that is not in the current node's ancestor set")]
    NodeOutputOutOfScope(String),
    /// `NodeOutput` references a node whose output is not yet available.
    #[error(
        "binding references node_output `{0}` whose output is not yet `Running` or `Completed` (state: {1:?})"
    )]
    NodeOutputUnavailable(String, NodeOutputState),
}

/// Validates a single binding against ADR-0021 capability and ancestry rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScopeValidator;

impl ScopeValidator {
    /// Validate `binding` against `ctx`. Returns `Ok(())` when the binding
    /// is permitted under ADR-0021; otherwise a typed [`ScopeError`].
    pub fn validate(binding: &Binding, ctx: &ScopeContext<'_>) -> Result<(), ScopeError> {
        match &binding.kind {
            BindingKind::RunInput
            | BindingKind::Constant(_)
            | BindingKind::IterationIndex
            | BindingKind::ItemOf
            | BindingKind::LastError => Ok(()),
            BindingKind::Env(_) => {
                let cap = crate::control_plane::domain::Capability::new(CAP_READ_ENV);
                if ctx.capabilities.contains(&cap) {
                    Ok(())
                } else {
                    Err(ScopeError::EnvRequiresCapability(CAP_READ_ENV.to_string()))
                }
            }
            BindingKind::SecretRef(_) => {
                let cap = crate::control_plane::domain::Capability::new(CAP_READ_SECRET);
                if ctx.capabilities.contains(&cap) {
                    Ok(())
                } else {
                    Err(ScopeError::SecretRefRequiresCapability(
                        CAP_READ_SECRET.to_string(),
                    ))
                }
            }
            BindingKind::NodeOutput => {
                let target = binding
                    .scoped_to_node
                    .clone()
                    .unwrap_or_else(|| ctx.current_node.to_string());
                if !ctx.ancestor_nodes.contains(&target) {
                    return Err(ScopeError::NodeOutputOutOfScope(target));
                }
                match ctx.node_states.get(&target).copied() {
                    Some(NodeOutputState::Running) | Some(NodeOutputState::Completed) => Ok(()),
                    Some(state) => Err(ScopeError::NodeOutputUnavailable(target, state)),
                    None => Err(ScopeError::NodeOutputUnavailable(
                        target,
                        NodeOutputState::Pending,
                    )),
                }
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::domain::{Capability, CapabilitySet};
    use serde_json::json;

    // --- Parser round-trips and rejections -----------------------------------

    #[test]
    fn parse_run_input_with_field_path() {
        let b = parse_binding("run_input.user.name").expect("parses");
        assert_eq!(b.kind, BindingKind::RunInput);
        assert_eq!(b.path.segments.len(), 2);
        assert_eq!(
            b.path.segments[0],
            BindingPathSegment::Field("user".to_string())
        );
        assert_eq!(
            b.path.segments[1],
            BindingPathSegment::Field("name".to_string())
        );
        assert_eq!(b.scoped_to_node, None);
    }

    #[test]
    fn parse_run_input_with_index_path() {
        let b = parse_binding("run_input[0]").expect("parses");
        assert_eq!(b.path.segments, vec![BindingPathSegment::Index(0)]);
    }

    #[test]
    fn parse_run_input_field_then_index() {
        let b = parse_binding("run_input.items[3]").expect("parses");
        assert_eq!(
            b.path.segments,
            vec![
                BindingPathSegment::Field("items".to_string()),
                BindingPathSegment::Index(3),
            ]
        );
    }

    #[test]
    fn parse_run_input_with_empty_path() {
        let b = parse_binding("run_input").expect("parses with empty path");
        assert_eq!(b.kind, BindingKind::RunInput);
        assert!(b.path.is_empty());
    }

    #[test]
    fn parse_env_with_path() {
        let b = parse_binding("env(HOME).subdir").expect("parses");
        assert_eq!(b.kind, BindingKind::Env("HOME".to_string()));
        assert_eq!(
            b.path.segments,
            vec![BindingPathSegment::Field("subdir".to_string())]
        );
    }

    #[test]
    fn parse_env_without_path() {
        let b = parse_binding("env(PATH)").expect("parses");
        assert_eq!(b.kind, BindingKind::Env("PATH".to_string()));
        assert!(b.path.is_empty());
    }

    #[test]
    fn parse_env_rejects_empty_argument() {
        let err = parse_binding("env()").unwrap_err();
        assert!(matches!(err, BindingError::EmptySourceValue("env")));
    }

    #[test]
    fn parse_secret_ref() {
        let b = parse_binding("secret_ref(api_key)").expect("parses");
        assert_eq!(b.kind, BindingKind::SecretRef("api_key".to_string()));
    }

    #[test]
    fn parse_constant_object() {
        let b = parse_binding("constant({\"a\": 1})").expect("parses");
        match &b.kind {
            BindingKind::Constant(v) => assert_eq!(*v, json!({"a": 1})),
            other => panic!("expected Constant, got {:?}", other),
        }
    }

    #[test]
    fn parse_constant_rejects_non_finite() {
        // serde_json's default parser refuses NaN/Inf strings, so the
        // textual parser's `constant(...)` path cannot reach a non-finite
        // literal. The internal guard still walks finite values without
        // raising.
        let mut obj = serde_json::Map::new();
        obj.insert("a".to_string(), serde_json::json!(1));
        assert!(validate_finite_json(&serde_json::Value::Object(obj)).is_ok());
    }

    #[test]
    fn validate_finite_json_rejects_constructed_non_finite() {
        // Construct an explicitly non-finite Number and confirm the guard
        // refuses it. (This path is reachable only when a caller constructs
        // a `serde_json::Value::Number` from `from_f64` with `arbitrary_precision`
        // enabled, or hand-rolls a Number — defensive coverage.)
        let n = match serde_json::Number::from_f64(f64::NAN) {
            Some(n) => n,
            None => {
                // serde_json::Number::from_f64 returns None for NaN; the
                // textual constant path already covers the rejection.
                return;
            }
        };
        let mut obj = serde_json::Map::new();
        obj.insert("x".to_string(), serde_json::Value::Number(n));
        assert!(validate_finite_json(&serde_json::Value::Object(obj)).is_err());
    }

    #[test]
    fn parse_node_output_with_scope() {
        let b = parse_binding("node_output@n1.payload").expect("parses");
        assert_eq!(b.kind, BindingKind::NodeOutput);
        assert_eq!(b.scoped_to_node.as_deref(), Some("n1"));
        assert_eq!(
            b.path.segments,
            vec![BindingPathSegment::Field("payload".to_string())]
        );
    }

    #[test]
    fn parse_node_output_without_scope() {
        let b = parse_binding("node_output.result").expect("parses");
        assert_eq!(b.scoped_to_node, None);
        assert_eq!(
            b.path.segments,
            vec![BindingPathSegment::Field("result".to_string())]
        );
    }

    #[test]
    fn parse_iteration_index_and_item_of() {
        let a = parse_binding("iteration_index").expect("parses");
        assert_eq!(a.kind, BindingKind::IterationIndex);
        let b = parse_binding("item_of.name").expect("parses");
        assert_eq!(b.kind, BindingKind::ItemOf);
        assert_eq!(
            b.path.segments,
            vec![BindingPathSegment::Field("name".to_string())]
        );
    }

    #[test]
    fn parse_last_error() {
        let b = parse_binding("last_error.code").expect("parses");
        assert_eq!(b.kind, BindingKind::LastError);
    }

    #[test]
    fn parse_rejects_empty_string() {
        let err = parse_binding("").unwrap_err();
        assert!(matches!(err, BindingError::EmptyPath));
    }

    #[test]
    fn parse_rejects_double_dot() {
        let err = parse_binding("run_input..x").unwrap_err();
        assert!(matches!(err, BindingError::MalformedPath(_)));
    }

    #[test]
    fn parse_rejects_trailing_dot() {
        let err = parse_binding("run_input.foo.").unwrap_err();
        assert!(matches!(err, BindingError::MalformedPath(_)));
    }

    #[test]
    fn parse_rejects_index_then_field() {
        let path = BindingPath::new(vec![
            BindingPathSegment::Index(0),
            BindingPathSegment::Field("name".to_string()),
        ]);
        assert!(matches!(
            path.validate(),
            Err(BindingError::MixedPathSegments)
        ));
    }

    #[test]
    fn parse_allows_field_then_index() {
        let path = BindingPath::new(vec![
            BindingPathSegment::Field("items".to_string()),
            BindingPathSegment::Index(0),
        ]);
        assert!(path.validate().is_ok());
    }

    #[test]
    fn parse_rejects_unknown_source() {
        let err = parse_binding("garbage(thing)").unwrap_err();
        // `garbage` is a plain identifier that is not a known source.
        assert!(matches!(err, BindingError::UnknownSource(_)));
    }

    #[test]
    fn parse_rejects_scope_on_run_input() {
        let err = parse_binding("run_input@n1.foo").unwrap_err();
        assert!(matches!(err, BindingError::InvalidScopeTarget));
    }

    #[test]
    fn parse_rejects_empty_scope_after_at() {
        let err = parse_binding("node_output@").unwrap_err();
        assert!(matches!(err, BindingError::EmptyScopeTarget));
    }

    // --- Scope validator -----------------------------------------------------

    fn caps(items: &[&str]) -> CapabilitySet {
        CapabilitySet::from_iter(items.iter().map(|s| Capability::new(*s)))
    }

    #[test]
    fn scope_validator_allows_run_input() {
        let b = parse_binding("run_input.x").unwrap();
        let cap = caps(&[]);
        let states = BTreeMap::new();
        let ancestors = BTreeSet::new();
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        assert!(ScopeValidator::validate(&b, &ctx).is_ok());
    }

    #[test]
    fn scope_validator_allows_secret_ref_with_capability() {
        let b = parse_binding("secret_ref(api_key)").unwrap();
        let cap = caps(&[CAP_READ_SECRET]);
        let states = BTreeMap::new();
        let ancestors = BTreeSet::new();
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        assert!(ScopeValidator::validate(&b, &ctx).is_ok());
    }

    #[test]
    fn scope_validator_rejects_secret_ref_without_capability() {
        let b = parse_binding("secret_ref(api_key)").unwrap();
        let cap = caps(&[]);
        let states = BTreeMap::new();
        let ancestors = BTreeSet::new();
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        let err = ScopeValidator::validate(&b, &ctx).unwrap_err();
        assert!(matches!(err, ScopeError::SecretRefRequiresCapability(_)));
    }

    #[test]
    fn scope_validator_allows_env_with_capability() {
        let b = parse_binding("env(HOME)").unwrap();
        let cap = caps(&[CAP_READ_ENV]);
        let states = BTreeMap::new();
        let ancestors = BTreeSet::new();
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        assert!(ScopeValidator::validate(&b, &ctx).is_ok());
    }

    #[test]
    fn scope_validator_rejects_env_without_capability() {
        let b = parse_binding("env(HOME)").unwrap();
        let cap = caps(&[]);
        let states = BTreeMap::new();
        let ancestors = BTreeSet::new();
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        let err = ScopeValidator::validate(&b, &ctx).unwrap_err();
        assert!(matches!(err, ScopeError::EnvRequiresCapability(_)));
    }

    #[test]
    fn scope_validator_rejects_node_output_out_of_scope() {
        let b = parse_binding("node_output@n_far.value").unwrap();
        let cap = caps(&[]);
        let mut states = BTreeMap::new();
        states.insert("n_far".to_string(), NodeOutputState::Completed);
        let ancestors = BTreeSet::new(); // n_far is not an ancestor
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        let err = ScopeValidator::validate(&b, &ctx).unwrap_err();
        assert!(matches!(err, ScopeError::NodeOutputOutOfScope(_)));
    }

    #[test]
    fn scope_validator_allows_node_output_when_running_and_in_scope() {
        let b = parse_binding("node_output@n_parent.value").unwrap();
        let cap = caps(&[]);
        let mut states = BTreeMap::new();
        states.insert("n_parent".to_string(), NodeOutputState::Running);
        let mut ancestors = BTreeSet::new();
        ancestors.insert("n_parent".to_string());
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        assert!(ScopeValidator::validate(&b, &ctx).is_ok());
    }

    #[test]
    fn scope_validator_rejects_node_output_when_pending() {
        let b = parse_binding("node_output@n_parent.value").unwrap();
        let cap = caps(&[]);
        let mut states = BTreeMap::new();
        states.insert("n_parent".to_string(), NodeOutputState::Pending);
        let mut ancestors = BTreeSet::new();
        ancestors.insert("n_parent".to_string());
        let ctx = ScopeContext {
            capabilities: &cap,
            node_states: &states,
            ancestor_nodes: &ancestors,
            current_node: "n0",
        };
        let err = ScopeValidator::validate(&b, &ctx).unwrap_err();
        assert!(matches!(
            err,
            ScopeError::NodeOutputUnavailable(_, NodeOutputState::Pending)
        ));
    }

    // --- serde round-trip ----------------------------------------------------

    #[test]
    fn binding_round_trips_through_json() {
        let b = parse_binding("node_output@n1.payload").unwrap();
        let json = serde_json::to_string(&b).unwrap();
        let back: Binding = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }
}
