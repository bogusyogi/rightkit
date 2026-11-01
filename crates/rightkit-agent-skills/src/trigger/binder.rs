//! Compilation of trigger strings into reusable, pre-parsed predicates.

use super::eval::eval;
use super::lexer::tokenize;
use super::parser::parse;
use super::types::{Expr, TriggerContext, TriggerError};

/// A compiled trigger: a pre-parsed expression that can be evaluated many
/// times against different contexts without re-lexing or re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledTrigger {
    expr: Expr,
}

impl CompiledTrigger {
    /// Does this trigger fire for the given context?
    pub fn fires(&self, ctx: &TriggerContext) -> bool {
        eval(&self.expr, ctx)
    }

    /// Borrow the underlying expression AST.
    pub fn expr(&self) -> &Expr {
        &self.expr
    }
}

/// Compiles trigger strings into [`CompiledTrigger`]s.
#[derive(Debug, Clone, Copy, Default)]
pub struct TriggerBinder;

impl TriggerBinder {
    /// Create a new binder.
    pub fn new() -> Self {
        Self
    }

    /// Compile a trigger expression string once.
    pub fn compile(&self, s: &str) -> Result<CompiledTrigger, TriggerError> {
        let tokens = tokenize(s)?;
        let expr = parse(tokens)?;
        Ok(CompiledTrigger { expr })
    }
}
