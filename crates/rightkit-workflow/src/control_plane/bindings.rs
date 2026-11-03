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
