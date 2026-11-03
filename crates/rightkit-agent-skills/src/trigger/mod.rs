//! The trigger DSL: a small boolean expression language over an evaluation
//! context, with a hand-written lexer, recursive-descent parser, and
//! evaluator.

pub mod binder;
pub mod eval;
pub mod lexer;
pub mod parser;
pub mod types;

pub use binder::{CompiledTrigger, TriggerBinder};
pub use eval::{eval, evaluate_with_regex};
pub use lexer::tokenize;
pub use parser::parse;
pub use types::{CmpOp, Expr, Token, TriggerContext, TriggerError};

/// Convenience: lex, parse, and evaluate a trigger expression string against
/// a context in one call.
pub fn evaluate_trigger(expr_str: &str, ctx: &TriggerContext) -> Result<bool, TriggerError> {
    let tokens = tokenize(expr_str)?;
    let expr = parse(tokens)?;
    Ok(eval(&expr, ctx))
}
