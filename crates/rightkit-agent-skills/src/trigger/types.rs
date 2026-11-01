//! Core types for the trigger DSL: tokens, AST, and evaluation context.

use std::collections::HashMap;

use thiserror::Error;

/// Errors that can occur while lexing, parsing, or compiling a trigger.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TriggerError {
    /// A lexical error (e.g. unterminated string literal).
    #[error("lex error: {0}")]
    Lex(String),
    /// A parse error with a human-readable message.
    #[error("parse error: {0}")]
    Parse(String),
    /// The token stream ended before a complete expression was parsed.
    #[error("unexpected end of input")]
    UnexpectedEof,
    /// An unexpected token was encountered during parsing.
    #[error("unexpected token: {0}")]
    UnexpectedToken(String),
    /// A regex pattern failed to compile.
    #[error("regex error: {0}")]
    Regex(String),
}

/// A lexical token in the trigger DSL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A bare identifier such as `event` or `SessionStart`.
    Ident(String),
    /// A double-quoted string literal (without the quotes).
    Str(String),
    /// The `and` keyword.
    And,
    /// The `or` keyword.
    Or,
    /// The `not` keyword.
    Not,
    /// The `==` operator.
    Eq,
    /// The `!=` operator.
    Ne,
    /// The `contains` operator.
    Contains,
    /// The `matches` operator.
    Matches,
    /// The `=~` regex operator.
    Regex,
    /// A left parenthesis `(`.
    LParen,
    /// A right parenthesis `)`.
    RParen,
}

/// A comparison operator in a trigger expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// Equality.
    Eq,
    /// Inequality.
    Ne,
    /// Substring containment.
    Contains,
    /// Glob match (`*` wildcard).
    Matches,
    /// Regex match (`=~`). The value is a regex pattern matched against the payload.
    Regex,
}

/// The trigger expression AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Logical AND of two sub-expressions.
    And(Box<Expr>, Box<Expr>),
    /// Logical OR of two sub-expressions.
    Or(Box<Expr>, Box<Expr>),
    /// Logical NOT of a sub-expression.
    Not(Box<Expr>),
    /// A bare variable reference, evaluated for truthiness.
    Var(String),
    /// A comparison between a context variable and a value.
    Cmp {
        /// The context variable name.
        var: String,
        /// The comparison operator.
        op: CmpOp,
        /// The value to compare against.
        value: String,
    },
}

/// Evaluation context: a string-to-string map of variables plus structured
/// trigger metadata for per-trigger-type regex matching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerContext {
    vars: HashMap<String, String>,
    /// The trigger type that fired this context evaluation, e.g. `"tool_call"`,
    /// `"message"`, `"file_change"`, `"cron"`.
    pub trigger_type: Option<String>,
    /// The raw payload associated with the trigger: the tool name for
    /// `tool_call`, the message text for `message`, the file path for
    /// `file_change`, the cron expression for `cron`, etc.
    pub trigger_payload: Option<String>,
}

impl TriggerContext {
    /// Create an empty context.
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
            trigger_type: None,
            trigger_payload: None,
        }
    }

    /// Set a variable in the context.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.vars.insert(key.into(), value.into());
        self
    }

    /// Get a variable from the context.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    /// Is the named variable truthy? A variable is truthy if it is present
    /// and its value is non-empty and not equal to `"false"`.
    pub fn truthy(&self, key: &str) -> bool {
        match self.vars.get(key) {
            Some(v) => !v.is_empty() && v != "false",
            None => false,
        }
    }
}
