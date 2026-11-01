//! WP-04 — Deterministic, side-effect-free expressions.
//!
//! ADR-0021 constraint: expressions are non-Turing-complete, deterministic,
//! depth/size bounded, and never read clocks or mutate state. The grammar
//! supports comparison, boolean, and length / existence operators over
//! [`EvalContext`]. NaN / Inf literals are rejected at parse time so the
//! reducer cannot observe non-deterministic numeric behaviour.
//!
//! The textual grammar (informal):
//!
//! ```text
//! expression := or
//! or         := and ( '||' and )*
//! and        := unary ( '&&' unary )*
//! unary      := '!' unary | comparison
//! comparison := primary ( cmp_op primary )?
//! cmp_op     := '==' | '!=' | '<' | '<=' | '>' | '>='
//! primary    := '(' expression ')' | literal | path | length_call | exists_call
//! literal    := number | string | 'true' | 'false' | 'null'
//! path       := identifier ( '.' identifier | '[' integer ']' )*
//! ```
//!
//! Path roots recognised at evaluation time:
//!
//! - `inputs`, `nodes`, `iteration_index`, `last_error`, `env`, `secrets`.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::control_plane::bindings::{BindingPath, BindingPathSegment};

// ─── Expression AST ──────────────────────────────────────────────────────────

/// One comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// One expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Compile-time literal value.
    Literal(Value),
    /// Path drilled into the [`EvalContext`].
    Path(BindingPath),
    /// `lhs <op> rhs` comparison. Both sides are arbitrary expressions.
    Compare(Box<Expr>, CmpOp, Box<Expr>),
    /// Boolean conjunction. Short-circuits on the first `false`.
    And(Vec<Expr>),
    /// Boolean disjunction. Short-circuits on the first `true`.
    Or(Vec<Expr>),
    /// Boolean negation.
    Not(Box<Expr>),
    /// `length(<expr>)` — length of an array, string, or object.
    Length(Box<Expr>),
    /// `exists(<expr>)` — true when the expression does not raise `MissingPath`.
    Exists(Box<Expr>),
}

impl Serialize for Expr {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_expr(self))
    }
}

impl<'de> Deserialize<'de> for Expr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        parse_json_binding(&v).map_err(serde::de::Error::custom)
    }
}

fn format_expr(e: &Expr) -> String {
    match e {
        Expr::Literal(v) => literal_to_string(v),
        Expr::Path(p) => format_path(p),
        Expr::Compare(l, op, r) => format!(
            "({} {} {})",
            format_expr(l),
            cmp_op_str(*op),
            format_expr(r)
        ),
        Expr::And(xs) => xs.iter().map(format_expr).collect::<Vec<_>>().join(" && "),
        Expr::Or(xs) => xs.iter().map(format_expr).collect::<Vec<_>>().join(" || "),
        Expr::Not(i) => format!("!{}", format_expr(i)),
        Expr::Length(i) => format!("length({})", format_expr(i)),
        Expr::Exists(i) => format!("exists({})", format_expr(i)),
    }
}

/// Canonical text persisted for a compiled expression.
pub fn canonical_text(expr: &Expr) -> String {
    format_expr(expr)
}

/// Enforce AST size/depth before an expression enters a compiled plan.
pub fn is_bounded(expr: &Expr, max_nodes: usize, max_depth: usize) -> bool {
    fn visit(expr: &Expr, depth: usize, nodes: &mut usize, max_depth: usize) -> bool {
        *nodes += 1;
        if depth > max_depth {
            return false;
        }
        match expr {
            Expr::Compare(left, _, right) => {
                visit(left, depth + 1, nodes, max_depth)
                    && visit(right, depth + 1, nodes, max_depth)
            }
            Expr::And(items) | Expr::Or(items) => items
                .iter()
                .all(|item| visit(item, depth + 1, nodes, max_depth)),
            Expr::Not(inner) | Expr::Length(inner) | Expr::Exists(inner) => {
                visit(inner, depth + 1, nodes, max_depth)
            }
            Expr::Literal(_) | Expr::Path(_) => true,
        }
    }
    let mut nodes = 0;
    visit(expr, 1, &mut nodes, max_depth) && nodes <= max_nodes
}

fn literal_to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        _ => v.to_string(),
    }
}

fn format_path(p: &BindingPath) -> String {
    let mut out = String::new();
    for (i, seg) in p.segments.iter().enumerate() {
        match seg {
            BindingPathSegment::Field(name) => {
                if i > 0 {
                    out.push('.');
                }
                out.push_str(name);
            }
            BindingPathSegment::Index(n) => {
                out.push('[');
                out.push_str(&n.to_string());
                out.push(']');
            }
        }
    }
    out
}

fn cmp_op_str(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "==",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
    }
}

// ─── Evaluation context ──────────────────────────────────────────────────────

/// Inputs / outputs / iteration state passed to [`evaluate`]. Pure data —
/// nothing in here is read from the wall clock or the filesystem.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvalContext {
    /// Declared workflow run inputs (`user`, `repo`, ...).
    pub inputs: serde_json::Map<String, Value>,
    /// Per-node outputs (`n1` -> output value).
    pub node_outputs: serde_json::Map<String, Value>,
    /// Current `Map` / `Loop` iteration index, if any.
    pub iteration_index: Option<usize>,
    /// Last error value, if any.
    pub last_error: Option<Value>,
    /// Environment variable map (string-only).
    pub env: BTreeMap<String, String>,
    /// Secret map (string-only, populated only for nodes with the
    /// `secret.read` capability).
    pub secrets: BTreeMap<String, String>,
}

impl EvalContext {
    /// Build an empty context. Useful in tests.
    pub fn empty() -> Self {
        Self::default()
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Typed failures from expression evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExpressionError {
    /// Path resolves to a missing field / index / root.
    #[error("missing path: {0}")]
    MissingPath(String),
    /// Type mismatch between operands.
    #[error("type mismatch: {0}")]
    TypeMismatch(String),
}

/// Typed failures from the expression parser. Carries a position and a
/// snippet so the planner / UI can highlight the offending span.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// Empty expression.
    #[error("expression is empty")]
    Empty,
    /// Unexpected token at `position`.
    #[error("unexpected token at position {position}: {kind} (snippet: `{snippet}`)")]
    UnexpectedToken {
        /// Category of the offending token.
        kind: String,
        /// Byte offset into the source.
        position: usize,
        /// Surrounding source bytes (≤ 16 chars around the cursor).
        snippet: String,
    },
    /// Unexpected end-of-input.
    #[error("unexpected end of input at position {position}: {kind} (snippet: `{snippet}`)")]
    UnexpectedEof {
        /// Category of the expected token.
        kind: String,
        /// Byte offset into the source.
        position: usize,
        /// Surrounding source bytes.
        snippet: String,
    },
    /// Invalid numeric literal.
    #[error("invalid number at position {position}: `{literal}` ({reason})")]
    InvalidNumber {
        /// Byte offset into the source.
        position: usize,
        /// The offending literal.
        literal: String,
        /// Reason for rejection.
        reason: String,
    },
    /// Invalid string literal.
    #[error("invalid string at position {position}: {reason}")]
    InvalidString {
        /// Byte offset into the source.
        position: usize,
        /// Reason for rejection.
        reason: String,
    },
    /// `NaN` / `Inf` literal.
    #[error("non-finite literal at position {position}: `{literal}`")]
    NonFiniteLiteral {
        /// Byte offset into the source.
        position: usize,
        /// The offending literal.
        literal: String,
    },
    /// Expected a different token.
    #[error("expected {kind} at position {position} (snippet: `{snippet}`)")]
    Expected {
        /// What was expected.
        kind: String,
        /// Byte offset into the source.
        position: usize,
        /// Surrounding source bytes.
        snippet: String,
    },
}

// ─── Evaluator ───────────────────────────────────────────────────────────────

/// Evaluate `expr` against `ctx`. Pure: no I/O, no clocks, no panics on
/// missing data — returns [`ExpressionError::MissingPath`] instead.
pub fn evaluate(expr: &Expr, ctx: &EvalContext) -> Result<Value, ExpressionError> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Path(p) => resolve_path(p, ctx),
        Expr::Compare(l, op, r) => compare(&evaluate(l, ctx)?, *op, &evaluate(r, ctx)?),
        Expr::And(xs) => short_circuit(xs, ctx, false),
        Expr::Or(xs) => short_circuit(xs, ctx, true),
        Expr::Not(i) => Ok(Value::Bool(!is_truthy(&evaluate(i, ctx)?))),
        Expr::Length(i) => len_of(&evaluate(i, ctx)?).ok_or_else(|| {
            ExpressionError::TypeMismatch("length requires array, string, or object".into())
        }),
        Expr::Exists(i) => match evaluate(i, ctx) {
            Ok(_) => Ok(Value::Bool(true)),
            Err(ExpressionError::MissingPath(_)) => Ok(Value::Bool(false)),
            Err(other) => Err(other),
        },
    }
}

fn short_circuit(xs: &[Expr], ctx: &EvalContext, want: bool) -> Result<Value, ExpressionError> {
    for e in xs {
        let v = evaluate(e, ctx)?;
        if is_truthy(&v) == want {
            return Ok(Value::Bool(want));
        }
    }
    Ok(Value::Bool(!want))
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn resolve_path(p: &BindingPath, ctx: &EvalContext) -> Result<Value, ExpressionError> {
    let mut iter = p.segments.iter();
    let first = iter
        .next()
        .ok_or_else(|| ExpressionError::MissingPath("empty path".into()))?;
    let BindingPathSegment::Field(root) = first else {
        return Err(ExpressionError::MissingPath(
            "path must start with an identifier".into(),
        ));
    };
    let mut cur = lookup_root(root, ctx)?;
    for seg in iter {
        match seg {
            BindingPathSegment::Field(name) => match cur {
                Value::Object(map) => {
                    cur = map
                        .get(name)
                        .cloned()
                        .ok_or_else(|| ExpressionError::MissingPath(format!("field `{name}`")))?;
                }
                _ => {
                    return Err(ExpressionError::TypeMismatch(format!(
                        "field access on non-object `{root}`"
                    )));
                }
            },
            BindingPathSegment::Index(i) => match cur {
                Value::Array(a) => {
                    cur = a
                        .get(*i)
                        .cloned()
                        .ok_or_else(|| ExpressionError::MissingPath(format!("index `{i}`")))?;
                }
                _ => {
                    return Err(ExpressionError::TypeMismatch(format!(
                        "index access on non-array `{root}`"
                    )));
                }
            },
        }
    }
    Ok(cur)
}

fn lookup_root(name: &str, ctx: &EvalContext) -> Result<Value, ExpressionError> {
    match name {
        "inputs" => Ok(Value::Object(ctx.inputs.clone())),
        "nodes" => Ok(Value::Object(ctx.node_outputs.clone())),
        "iteration_index" => match ctx.iteration_index {
            Some(n) => Ok(Value::Number(serde_json::Number::from(n))),
            None => Err(ExpressionError::MissingPath("iteration_index".into())),
        },
        "last_error" => match &ctx.last_error {
            Some(v) => Ok(v.clone()),
            None => Err(ExpressionError::MissingPath("last_error".into())),
        },
        "env" | "secrets" => {
            let src: &BTreeMap<String, String> = if name == "env" {
                &ctx.env
            } else {
                &ctx.secrets
            };
            let mut m = serde_json::Map::new();
            for (k, v) in src {
                m.insert(k.clone(), Value::String(v.clone()));
            }
            Ok(Value::Object(m))
        }
        other => Err(ExpressionError::MissingPath(format!(
            "unknown root `{other}`"
        ))),
    }
}

fn compare(a: &Value, op: CmpOp, b: &Value) -> Result<Value, ExpressionError> {
    use CmpOp::*;
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let xf = x.as_f64().ok_or_else(num_err)?;
            let yf = y.as_f64().ok_or_else(num_err)?;
            Ok(Value::Bool(match op {
                Eq => xf == yf,
                Ne => xf != yf,
                Lt => xf < yf,
                Le => xf <= yf,
                Gt => xf > yf,
                Ge => xf >= yf,
            }))
        }
        (Value::String(x), Value::String(y)) => Ok(Value::Bool(match op {
            Eq => x == y,
            Ne => x != y,
            Lt => x < y,
            Le => x <= y,
            Gt => x > y,
            Ge => x >= y,
        })),
        (Value::Bool(x), Value::Bool(y)) => match op {
            Eq => Ok(Value::Bool(x == y)),
            Ne => Ok(Value::Bool(x != y)),
            _ => Err(ExpressionError::TypeMismatch(
                "ordering on bools not defined".into(),
            )),
        },
        (Value::Null, Value::Null) => match op {
            Eq => Ok(Value::Bool(true)),
            Ne => Ok(Value::Bool(false)),
            _ => Err(ExpressionError::TypeMismatch(
                "ordering on nulls not defined".into(),
            )),
        },
        _ => Err(ExpressionError::TypeMismatch(format!(
            "cannot compare {} and {}",
            value_kind(a),
            value_kind(b)
        ))),
    }
}

fn num_err() -> ExpressionError {
    ExpressionError::TypeMismatch("number coercion".into())
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn len_of(v: &Value) -> Option<Value> {
    let n = match v {
        Value::Array(a) => a.len(),
        Value::String(s) => s.chars().count(),
        Value::Object(o) => o.len(),
        _ => return None,
    };
    Some(Value::Number(serde_json::Number::from(n)))
}

// ─── Parser ──────────────────────────────────────────────────────────────────

/// Parse `text` into an [`Expr`]. See module docs for the grammar.
pub fn parse(text: &str) -> Result<Expr, ParseError> {
    let mut p = Parser::new(text);
    let e = p.parse_expression()?;
    p.skip_ws();
    if !p.at_end() {
        return Err(ParseError::UnexpectedToken {
            kind: format!("trailing input `{}`", p.peek().unwrap_or('?')),
            position: p.pos,
            snippet: p.snippet(),
        });
    }
    Ok(e)
}

/// Parse a JSON-shaped value as an expression. Strings are parsed with
/// [`parse`]; booleans / numbers / null are wrapped as literals; arrays /
/// objects are rejected in V1 (structured AST serialisation is not supported).
pub fn parse_json_binding(value: &Value) -> Result<Expr, ParseError> {
    match value {
        Value::String(s) => parse(s),
        Value::Bool(b) => Ok(Expr::Literal(Value::Bool(*b))),
        Value::Number(n) => Ok(Expr::Literal(Value::Number(n.clone()))),
        Value::Null => Ok(Expr::Literal(Value::Null)),
        Value::Array(_) | Value::Object(_) => Err(ParseError::UnexpectedToken {
            kind: format!("structured JSON expression shape is not supported in V1; got `{value}`"),
            position: 0,
            snippet: value.to_string(),
        }),
    }
}

// ─── Internal parser ─────────────────────────────────────────────────────────

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.advance();
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn snippet(&self) -> String {
        let start = self.pos.saturating_sub(8);
        let end = (self.pos + 8).min(self.src.len());
        self.src[start..end].to_string()
    }

    fn expect_char(&mut self, want: char, kind: &str) -> Result<(), ParseError> {
        if self.peek() == Some(want) {
            self.advance();
            Ok(())
        } else {
            Err(ParseError::Expected {
                kind: kind.to_string(),
                position: self.pos,
                snippet: self.snippet(),
            })
        }
    }

    fn consume_str(&mut self, s: &str) -> bool {
        if self.src[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.advance();
        }
        self.src[start..self.pos].to_string()
    }

    fn parse_expression(&mut self) -> Result<Expr, ParseError> {
        self.skip_ws();
        if self.at_end() {
            return Err(ParseError::Empty);
        }
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        loop {
            self.skip_ws();
            if self.consume_str("||") {
                left = Expr::Or(vec![left, self.parse_and()?]);
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_ws();
            if self.consume_str("&&") {
                left = Expr::And(vec![left, self.parse_unary()?]);
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        self.skip_ws();
        if self.peek() == Some('!') {
            self.advance();
            if self.peek() == Some('=') {
                return Err(ParseError::UnexpectedToken {
                    kind: "`!=` is a comparison operator, not a unary not".into(),
                    position: self.pos.saturating_sub(1),
                    snippet: self.snippet(),
                });
            }
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_primary()?;
        self.skip_ws();
        let op = if self.consume_str("==") {
            Some(CmpOp::Eq)
        } else if self.consume_str("!=") {
            Some(CmpOp::Ne)
        } else if self.consume_str("<=") {
            Some(CmpOp::Le)
        } else if self.consume_str(">=") {
            Some(CmpOp::Ge)
        } else if self.peek() == Some('<') {
            self.advance();
            Some(CmpOp::Lt)
        } else if self.peek() == Some('>') {
            self.advance();
            Some(CmpOp::Gt)
        } else {
            None
        };
        if let Some(op) = op {
            self.skip_ws();
            return Ok(Expr::Compare(
                Box::new(left),
                op,
                Box::new(self.parse_primary()?),
            ));
        }
        Ok(left)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        self.skip_ws();
        let c = self.peek().ok_or(ParseError::UnexpectedEof {
            kind: "expected literal, path, or `(`".into(),
            position: self.pos,
            snippet: self.snippet(),
        })?;
        match c {
            '(' => {
                self.advance();
                let inner = self.parse_expression()?;
                self.skip_ws();
                self.expect_char(')', "closing `)`")?;
                Ok(inner)
            }
            '"' => Ok(Expr::Literal(Value::String(self.parse_string()?))),
            '0'..='9' | '-' => {
                let n = self.parse_number()?;
                if let Value::Number(num) = &n {
                    if let Some(f) = num.as_f64() {
                        if !f.is_finite() {
                            return Err(ParseError::NonFiniteLiteral {
                                position: self.pos,
                                literal: num.to_string(),
                            });
                        }
                    }
                }
                Ok(Expr::Literal(n))
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = self.pos;
                let ident = self.read_ident();
                match ident.as_str() {
                    "true" => Ok(Expr::Literal(Value::Bool(true))),
                    "false" => Ok(Expr::Literal(Value::Bool(false))),
                    "null" => Ok(Expr::Literal(Value::Null)),
                    "length" => self.parse_call(Expr::Length),
                    "exists" => self.parse_call(Expr::Exists),
                    // Reject non-finite literal identifiers so the reducer
                    // cannot observe NaN / Infinity. Carry a span so the
                    // planner / UI can highlight the rejection site.
                    "NaN" | "Infinity" | "Inf" => Err(ParseError::UnexpectedToken {
                        kind: format!("non-finite literal `{ident}` is not allowed"),
                        position: start,
                        snippet: self.src
                            [start.saturating_sub(8)..(self.pos + 8).min(self.src.len())]
                            .to_string(),
                    }),
                    _ => Ok(Expr::Path(self.parse_path_tail(ident)?)),
                }
            }
            _ => Err(ParseError::UnexpectedToken {
                kind: format!("unexpected character `{c}`"),
                position: self.pos,
                snippet: self.snippet(),
            }),
        }
    }

    fn parse_call<F: FnOnce(Box<Expr>) -> Expr>(&mut self, ctor: F) -> Result<Expr, ParseError> {
        self.skip_ws();
        self.expect_char('(', "opening `(` after function name")?;
        let arg = self.parse_expression()?;
        self.skip_ws();
        self.expect_char(')', "closing `)`")?;
        Ok(ctor(Box::new(arg)))
    }

    fn parse_path_tail(&mut self, first: String) -> Result<BindingPath, ParseError> {
        let mut segs: Vec<BindingPathSegment> = vec![BindingPathSegment::Field(first)];
        loop {
            self.skip_ws();
            match self.peek() {
                Some('.') => {
                    let save = self.pos;
                    self.advance();
                    if self.peek() == Some('.') {
                        return Err(ParseError::UnexpectedToken {
                            kind: "`..` not allowed in path".into(),
                            position: save,
                            snippet: self.snippet(),
                        });
                    }
                    let field = self.read_ident();
                    if field.is_empty() {
                        return Err(ParseError::Expected {
                            kind: "field name after `.`".into(),
                            position: self.pos,
                            snippet: self.snippet(),
                        });
                    }
                    segs.push(BindingPathSegment::Field(field));
                }
                Some('[') => {
                    self.advance();
                    let start = self.pos;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.advance();
                    }
                    let s = &self.src[start..self.pos];
                    if s.is_empty() {
                        return Err(ParseError::Expected {
                            kind: "index digits".into(),
                            position: self.pos,
                            snippet: self.snippet(),
                        });
                    }
                    let idx: usize = s.parse().map_err(|_| ParseError::Expected {
                        kind: "valid index".into(),
                        position: self.pos,
                        snippet: self.snippet(),
                    })?;
                    self.expect_char(']', "`]`")?;
                    segs.push(BindingPathSegment::Index(idx));
                }
                _ => break,
            }
        }
        let path = BindingPath::new(segs);
        path.validate().map_err(|_| ParseError::Expected {
            kind: "well-formed path (no Field after Index)".into(),
            position: self.pos,
            snippet: self.snippet(),
        })?;
        Ok(path)
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        self.advance(); // opening `"`
        let mut out = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError::UnexpectedEof {
                        kind: "closing `\"`".into(),
                        position: self.pos,
                        snippet: self.snippet(),
                    });
                }
                Some('"') => {
                    self.advance();
                    return Ok(out);
                }
                Some('\\') => {
                    self.advance();
                    let esc = self.peek().ok_or(ParseError::UnexpectedEof {
                        kind: "string body".into(),
                        position: self.pos,
                        snippet: self.snippet(),
                    })?;
                    let c = match esc {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '/' => '/',
                        other => {
                            return Err(ParseError::InvalidString {
                                position: self.pos,
                                reason: format!("unknown escape `\\{other}`"),
                            });
                        }
                    };
                    out.push(c);
                    self.advance();
                }
                Some(c) => {
                    out.push(c);
                    self.advance();
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.advance();
            // `-<ident>` is not a number; if the ident is a non-finite
            // literal (e.g. `-Infinity`, `-NaN`) reject it with a span so
            // the planner / UI can highlight the rejection site.
            if self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                let ident = self.read_ident();
                let literal = format!("-{}", ident);
                let pos_after = self.pos;
                return Err(ParseError::UnexpectedToken {
                    kind: format!("non-finite literal `{literal}` is not allowed"),
                    position: start,
                    snippet: self.src[start.saturating_sub(8)..(pos_after + 8).min(self.src.len())]
                        .to_string(),
                });
            }
        }
        let (mut has_dot, mut has_exp) = (false, false);
        while let Some(c) = self.peek() {
            match c {
                '0'..='9' => {
                    self.advance();
                }
                '.' => {
                    if has_dot {
                        return Err(ParseError::InvalidNumber {
                            position: self.pos,
                            literal: self.src[start..self.pos].into(),
                            reason: "duplicate decimal".into(),
                        });
                    }
                    has_dot = true;
                    self.advance();
                }
                'e' | 'E' => {
                    if has_exp {
                        return Err(ParseError::InvalidNumber {
                            position: self.pos,
                            literal: self.src[start..self.pos].into(),
                            reason: "duplicate exponent".into(),
                        });
                    }
                    has_exp = true;
                    self.advance();
                    if matches!(self.peek(), Some('+') | Some('-')) {
                        self.advance();
                    }
                }
                _ => break,
            }
        }
        let s = &self.src[start..self.pos];
        if s.is_empty() || s == "-" {
            return Err(ParseError::InvalidNumber {
                position: self.pos,
                literal: s.into(),
                reason: "no digits".into(),
            });
        }
        // Explicit reject for NaN / Inf literals.
        let upper = s.to_ascii_uppercase();
        if upper.contains("NAN") || upper.contains("INF") {
            return Err(ParseError::NonFiniteLiteral {
                position: start,
                literal: s.into(),
            });
        }
        let n: serde_json::Number =
            s.parse::<serde_json::Number>()
                .map_err(|e: serde_json::Error| ParseError::InvalidNumber {
                    position: start,
                    literal: s.into(),
                    reason: e.to_string(),
                })?;
        Ok(Value::Number(n))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::bindings::{BindingPath, BindingPathSegment};
    use serde_json::json;

    fn ctx() -> EvalContext {
        let mut inputs = serde_json::Map::new();
        inputs.insert("user".into(), json!({"name": "adrian", "age": 38}));
        inputs.insert("items".into(), json!(["a", "b", "c"]));
        inputs.insert("flag".into(), Value::Bool(true));
        inputs.insert("count".into(), json!(0));
        let mut outs = serde_json::Map::new();
        outs.insert("n1".into(), json!({"ok": true}));
        let mut env = BTreeMap::new();
        env.insert("HOME".into(), "/home/adrian".into());
        EvalContext {
            inputs,
            node_outputs: outs,
            iteration_index: Some(2),
            last_error: None,
            env,
            secrets: BTreeMap::new(),
        }
    }

    // ── Parser round-trip across 12 example strings ─────────────────────────

    #[test]
    fn parse_eval_round_trip_across_twelve_strings() {
        let twelve = [
            "true",
            "false",
            "null",
            "42",
            "-7",
            "3.14",
            "\"hello\"",
            "!true",
            "1 == 1",
            "1 != 2",
            "(1 < 2) && (3 >= 3)",
            "length(inputs.items)",
        ];
        assert_eq!(twelve.len(), 12);
        for s in twelve {
            let e = parse(s).unwrap_or_else(|err| panic!("{s}: parse: {err:?}"));
            evaluate(&e, &ctx()).unwrap_or_else(|err| panic!("{s}: eval: {err:?}"));
        }
    }

    // ── Every Expr variant on representative values ─────────────────────────
    #[test]
    fn evaluate_each_expr_variant() {
        let c = &ctx();
        assert_eq!(
            evaluate(&Expr::Literal(json!({"k": "v"})), c).unwrap(),
            json!({"k": "v"})
        );
        assert_eq!(
            evaluate(
                &Expr::Path(BindingPath::new(vec![
                    BindingPathSegment::Field("inputs".into()),
                    BindingPathSegment::Field("user".into()),
                    BindingPathSegment::Field("name".into()),
                ])),
                c
            )
            .unwrap(),
            Value::String("adrian".into())
        );
        assert_eq!(
            evaluate(
                &Expr::Compare(
                    Box::new(Expr::Literal(json!(1))),
                    CmpOp::Eq,
                    Box::new(Expr::Literal(json!(1)))
                ),
                c
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(
                &Expr::And(vec![
                    Expr::Literal(Value::Bool(true)),
                    Expr::Literal(Value::Bool(true))
                ]),
                c
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(
                &Expr::Or(vec![
                    Expr::Literal(Value::Bool(false)),
                    Expr::Literal(Value::Bool(true))
                ]),
                c
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(&Expr::Not(Box::new(Expr::Literal(Value::Bool(true)))), c).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            evaluate(
                &Expr::Length(Box::new(Expr::Literal(json!([1, 2, 3, 4])))),
                c
            )
            .unwrap(),
            Value::Number(serde_json::Number::from(4))
        );
        assert_eq!(
            evaluate(
                &Expr::Exists(Box::new(Expr::Path(BindingPath::new(vec![
                    BindingPathSegment::Field("iteration_index".into())
                ])))),
                c
            )
            .unwrap(),
            Value::Bool(true)
        );
    }

    // ── Compare variants, AND/OR precedence, error paths ───────────────────

    #[test]
    fn evaluate_compare_and_or_and_precedence() {
        let c = &ctx();
        let cases = [
            ("1 == 1", Value::Bool(true)),
            ("1 != 2", Value::Bool(true)),
            ("1 < 2", Value::Bool(true)),
            ("2 <= 2", Value::Bool(true)),
            ("3 > 2", Value::Bool(true)),
            ("2 >= 2", Value::Bool(true)),
            ("\"a\" < \"b\"", Value::Bool(true)),
            ("true == true", Value::Bool(true)),
            ("null == null", Value::Bool(true)),
        ];
        for (s, want) in cases {
            assert_eq!(evaluate(&parse(s).unwrap(), c).unwrap(), want, "case `{s}`");
        }
        // && binds tighter than ||; ! binds tighter than &&.
        assert_eq!(
            evaluate(&parse("true && false || true").unwrap(), c).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(&parse("false || true && false").unwrap(), c).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            evaluate(&parse("(true || false) && true").unwrap(), c).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(&parse("!false && true").unwrap(), c).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(&parse("(true || false) && (false || true)").unwrap(), c).unwrap(),
            Value::Bool(true)
        );
        // Errors
        assert!(matches!(
            evaluate(&parse("inputs.does_not_exist").unwrap(), c).unwrap_err(),
            ExpressionError::MissingPath(_)
        ));
        assert!(matches!(
            evaluate(&parse("1 < \"a\"").unwrap(), c).unwrap_err(),
            ExpressionError::TypeMismatch(_)
        ));
        assert!(matches!(
            evaluate(&Expr::Length(Box::new(Expr::Literal(json!(42)))), c).unwrap_err(),
            ExpressionError::TypeMismatch(_)
        ));
    }

    // ── Parser rejections carry a span ─────────────────────────────────────

    #[test]
    fn parser_rejections_carry_span() {
        let expect_span = |e: ParseError| match e {
            ParseError::UnexpectedToken {
                position: _,
                snippet,
                ..
            }
            | ParseError::Expected {
                position: _,
                snippet,
                ..
            } => {
                assert!(!snippet.is_empty());
            }
            other => panic!("expected span-tagged error, got {other:?}"),
        };
        expect_span(parse("inputs.user.name + 1").unwrap_err());
        expect_span(parse("inputs..foo").unwrap_err());
        expect_span(parse("inputs.items[0].x").unwrap_err());
        expect_span(parse("NaN").unwrap_err());
        expect_span(parse("Infinity").unwrap_err());
        expect_span(parse("-Infinity").unwrap_err());
        assert!(matches!(parse("").unwrap_err(), ParseError::Empty));
        assert!(matches!(parse("   ").unwrap_err(), ParseError::Empty));
    }

    // ── Determinism + serde round-trip + parse_json_binding ───────────────

    #[test]
    fn determinism_and_serde_round_trip() {
        let c = &ctx();
        let e = parse("inputs.items[0] == \"a\" && length(inputs.items) == 3").unwrap();
        let a = evaluate(&e, c).unwrap();
        let b = evaluate(&e, c).unwrap();
        let cc = evaluate(&e, c).unwrap();
        assert_eq!(a, b);
        assert_eq!(b, cc);
        assert_eq!(a, Value::Bool(true));

        // Serde round-trip preserves evaluation semantics.
        let original =
            parse("(inputs.flag && inputs.count == 0) || length(inputs.items) > 0").unwrap();
        let j = serde_json::to_string(&original).unwrap();
        let back: Expr = serde_json::from_str(&j).unwrap();
        assert_eq!(evaluate(&back, c).unwrap(), evaluate(&original, c).unwrap());

        // parse_json_binding accepts strings / numbers / bools / null.
        assert_eq!(
            evaluate(
                &parse_json_binding(&json!("inputs.user.name == \"adrian\"")).unwrap(),
                c
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(&parse_json_binding(&json!(42)).unwrap(), c).unwrap(),
            json!(42)
        );
        assert_eq!(
            evaluate(&parse_json_binding(&json!(true)).unwrap(), c).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate(&parse_json_binding(&Value::Null).unwrap(), c).unwrap(),
            Value::Null
        );

        // Serde deserialises a JSON string into an Expr automatically.
        let v = json!("inputs.items[0] == \"a\"");
        let e: Expr = serde_json::from_value(v).unwrap();
        assert_eq!(evaluate(&e, c).unwrap(), Value::Bool(true));
    }
}
