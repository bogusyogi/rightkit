//! Evaluator for the trigger DSL.

use regex::Regex;

use super::types::{CmpOp, Expr, TriggerContext, TriggerError};

/// Evaluate an expression against a context, returning a boolean.
///
/// `CmpOp::Regex` patterns in `expr` are compiled on each call. For
/// hot paths use [`evaluate_with_regex`] which surfaces compile errors,
/// or pre-compile triggers via [`crate::trigger::TriggerBinder`].
pub fn eval(expr: &Expr, ctx: &TriggerContext) -> bool {
    evaluate_with_regex(expr, ctx).unwrap_or(false)
}

/// Evaluate an expression against a context, returning `Ok(bool)` on success
/// or a [`TriggerError::Regex`] if a `=~` pattern fails to compile.
///
/// This is the preferred entry-point when regex errors should be visible to
/// callers (e.g. at skill-load time). [`eval`] silently returns `false` on
/// bad patterns.
pub fn evaluate_with_regex(expr: &Expr, ctx: &TriggerContext) -> Result<bool, TriggerError> {
    match expr {
        Expr::And(l, r) => Ok(evaluate_with_regex(l, ctx)? && evaluate_with_regex(r, ctx)?),
        Expr::Or(l, r) => Ok(evaluate_with_regex(l, ctx)? || evaluate_with_regex(r, ctx)?),
        Expr::Not(inner) => Ok(!evaluate_with_regex(inner, ctx)?),
        Expr::Var(name) => Ok(ctx.truthy(name)),
        Expr::Cmp { var, op, value } => {
            // For `=~` the variable name may be the literal string
            // "trigger_payload" to match against `ctx.trigger_payload`, or any
            // named context variable.
            let actual: &str = if var == "trigger_payload" {
                ctx.trigger_payload.as_deref().unwrap_or("")
            } else if var == "trigger_type" {
                ctx.trigger_type.as_deref().unwrap_or("")
            } else {
                ctx.get(var).unwrap_or("")
            };
            match op {
                CmpOp::Eq => Ok(actual == value),
                CmpOp::Ne => Ok(actual != value),
                CmpOp::Contains => Ok(actual.contains(value.as_str())),
                CmpOp::Matches => Ok(glob_match(value, actual)),
                CmpOp::Regex => {
                    let re = Regex::new(value).map_err(|e| TriggerError::Regex(e.to_string()))?;
                    Ok(re.is_match(actual))
                }
            }
        }
    }
}

/// A simple glob matcher supporting the `*` wildcard (matches any sequence,
/// including the empty sequence). Other characters match literally.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match_inner(&p, &t)
}

fn glob_match_inner(pattern: &[char], text: &[char]) -> bool {
    // Iterative two-pointer glob with backtracking on the last '*'.
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_idx: Option<usize> = None;
    let mut star_match = 0usize;

    while ti < text.len() {
        if pi < pattern.len() && pattern[pi] == '*' {
            star_idx = Some(pi);
            star_match = ti;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == text[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star_idx {
            // Backtrack: let the star consume one more character.
            pi = s + 1;
            star_match += 1;
            ti = star_match;
        } else {
            return false;
        }
    }

    // Consume any trailing stars in the pattern.
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }

    pi == pattern.len()
}
