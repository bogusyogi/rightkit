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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TriggerContext {
        let mut c = TriggerContext::new();
        c.set("event", "PreToolUse");
        c.set("tool", "git_diff");
        c.set("file_path", "src/main.rs");
        c
    }

    // ---- Lexer ----

    #[test]
    fn lex_simple_ident() {
        let toks = tokenize("event").unwrap();
        assert_eq!(toks, vec![Token::Ident("event".into())]);
    }

    #[test]
    fn lex_string_literal() {
        let toks = tokenize("\"PreToolUse\"").unwrap();
        assert_eq!(toks, vec![Token::Str("PreToolUse".into())]);
    }

    #[test]
    fn lex_operators() {
        let toks = tokenize("and or not == != contains matches").unwrap();
        assert_eq!(
            toks,
            vec![
                Token::And,
                Token::Or,
                Token::Not,
                Token::Eq,
                Token::Ne,
                Token::Contains,
                Token::Matches,
            ]
        );
    }

    #[test]
    fn lex_parens() {
        let toks = tokenize("( event )").unwrap();
        assert_eq!(
            toks,
            vec![Token::LParen, Token::Ident("event".into()), Token::RParen]
        );
    }

    #[test]
    fn lex_unterminated_string_errors() {
        let err = tokenize("\"oops").unwrap_err();
        assert!(matches!(err, TriggerError::Lex(_)));
    }

    // ---- Parser ----

    #[test]
    fn parse_single_ident() {
        let expr = parse(tokenize("event").unwrap()).unwrap();
        assert_eq!(expr, Expr::Var("event".into()));
    }

    #[test]
    fn parse_and() {
        let expr = parse(tokenize("a and b").unwrap()).unwrap();
        assert_eq!(
            expr,
            Expr::And(
                Box::new(Expr::Var("a".into())),
                Box::new(Expr::Var("b".into()))
            )
        );
    }

    #[test]
    fn parse_or() {
        let expr = parse(tokenize("a or b").unwrap()).unwrap();
        assert_eq!(
            expr,
            Expr::Or(
                Box::new(Expr::Var("a".into())),
                Box::new(Expr::Var("b".into()))
            )
        );
    }

    #[test]
    fn parse_not() {
        let expr = parse(tokenize("not a").unwrap()).unwrap();
        assert_eq!(expr, Expr::Not(Box::new(Expr::Var("a".into()))));
    }

    #[test]
    fn parse_comparison_eq() {
        let expr = parse(tokenize("event == \"PreToolUse\"").unwrap()).unwrap();
        assert_eq!(
            expr,
            Expr::Cmp {
                var: "event".into(),
                op: CmpOp::Eq,
                value: "PreToolUse".into(),
            }
        );
    }

    #[test]
    fn parse_parenthesized() {
        let expr = parse(tokenize("(a or b) and c").unwrap()).unwrap();
        assert_eq!(
            expr,
            Expr::And(
                Box::new(Expr::Or(
                    Box::new(Expr::Var("a".into())),
                    Box::new(Expr::Var("b".into()))
                )),
                Box::new(Expr::Var("c".into()))
            )
        );
    }

    #[test]
    fn parse_precedence_and_binds_tighter_than_or() {
        // a or b and c  ==  a or (b and c)
        let expr = parse(tokenize("a or b and c").unwrap()).unwrap();
        assert_eq!(
            expr,
            Expr::Or(
                Box::new(Expr::Var("a".into())),
                Box::new(Expr::And(
                    Box::new(Expr::Var("b".into())),
                    Box::new(Expr::Var("c".into()))
                ))
            )
        );
    }

    #[test]
    fn parse_unexpected_eof_errors() {
        let err = parse(tokenize("not").unwrap()).unwrap_err();
        assert_eq!(err, TriggerError::UnexpectedEof);
    }

    // ---- Eval ----

    #[test]
    fn eval_bare_truthy_var_true() {
        assert!(evaluate_trigger("event", &ctx()).unwrap());
    }

    #[test]
    fn eval_bare_missing_var_false() {
        assert!(!evaluate_trigger("missing", &ctx()).unwrap());
    }

    #[test]
    fn eval_eq_match() {
        assert!(evaluate_trigger("event == \"PreToolUse\"", &ctx()).unwrap());
    }

    #[test]
    fn eval_eq_no_match() {
        assert!(!evaluate_trigger("event == \"Stop\"", &ctx()).unwrap());
    }

    #[test]
    fn eval_ne() {
        assert!(evaluate_trigger("event != \"Stop\"", &ctx()).unwrap());
        assert!(!evaluate_trigger("event != \"PreToolUse\"", &ctx()).unwrap());
    }

    #[test]
    fn eval_contains_substring() {
        assert!(evaluate_trigger("tool contains \"git\"", &ctx()).unwrap());
        assert!(!evaluate_trigger("tool contains \"svn\"", &ctx()).unwrap());
    }

    #[test]
    fn eval_matches_glob_star() {
        assert!(evaluate_trigger("file_path matches \"*.rs\"", &ctx()).unwrap());
        assert!(!evaluate_trigger("file_path matches \"*.py\"", &ctx()).unwrap());
        assert!(evaluate_trigger("file_path matches \"src/*\"", &ctx()).unwrap());
    }

    #[test]
    fn eval_and_both_true() {
        assert!(evaluate_trigger("event and tool", &ctx()).unwrap());
    }

    #[test]
    fn eval_and_one_false() {
        assert!(!evaluate_trigger("event and missing", &ctx()).unwrap());
    }

    #[test]
    fn eval_or_one_true() {
        assert!(evaluate_trigger("missing or event", &ctx()).unwrap());
    }

    #[test]
    fn eval_not() {
        assert!(evaluate_trigger("not missing", &ctx()).unwrap());
        assert!(!evaluate_trigger("not event", &ctx()).unwrap());
    }

    #[test]
    fn eval_complex_precedence() {
        // event == "PreToolUse" and tool contains "git" or event == "Stop"
        // == (event=="PreToolUse" and tool contains "git") or event=="Stop"
        assert!(evaluate_trigger(
            "event == \"PreToolUse\" and tool contains \"git\" or event == \"Stop\"",
            &ctx()
        )
        .unwrap());

        let mut c2 = TriggerContext::new();
        c2.set("event", "Stop");
        c2.set("tool", "");
        assert!(evaluate_trigger(
            "event == \"PreToolUse\" and tool contains \"git\" or event == \"Stop\"",
            &c2
        )
        .unwrap());

        let mut c3 = TriggerContext::new();
        c3.set("event", "PostToolUse");
        c3.set("tool", "svn");
        assert!(!evaluate_trigger(
            "event == \"PreToolUse\" and tool contains \"git\" or event == \"Stop\"",
            &c3
        )
        .unwrap());
    }

    // ---- Binder ----

    #[test]
    fn compile_and_fire() {
        let binder = TriggerBinder::new();
        let trigger = binder.compile("event == \"PreToolUse\"").unwrap();
        assert!(trigger.fires(&ctx()));

        let mut other = TriggerContext::new();
        other.set("event", "Stop");
        assert!(!trigger.fires(&other));
    }

    #[test]
    fn compile_invalid_errors() {
        let binder = TriggerBinder::new();
        assert!(binder.compile("event ==").is_err());
        assert!(binder.compile("and or").is_err());
    }

    #[test]
    fn evaluate_trigger_convenience() {
        assert!(evaluate_trigger("event == \"PreToolUse\"", &ctx()).unwrap());
    }

    // ---- Regex operator =~ ----

    #[test]
    fn lex_regex_operator() {
        let toks = tokenize("trigger_payload =~ \"shell|bash\"").unwrap();
        assert_eq!(
            toks,
            vec![
                Token::Ident("trigger_payload".into()),
                Token::Regex,
                Token::Str("shell|bash".into()),
            ]
        );
    }

    #[test]
    fn parse_regex_comparison() {
        let expr = parse(tokenize("trigger_payload =~ \"shell|bash\"").unwrap()).unwrap();
        assert_eq!(
            expr,
            Expr::Cmp {
                var: "trigger_payload".into(),
                op: CmpOp::Regex,
                value: "shell|bash".into(),
            }
        );
    }

    #[test]
    fn eval_regex_matches_trigger_payload() {
        let mut ctx = TriggerContext::new();
        ctx.trigger_type = Some("tool_call".into());
        ctx.trigger_payload = Some("shell".into());

        assert!(evaluate_with_regex(
            &parse(tokenize("trigger_payload =~ \"shell|bash\"").unwrap()).unwrap(),
            &ctx
        )
        .unwrap());
    }

    #[test]
    fn eval_regex_no_match() {
        let mut ctx = TriggerContext::new();
        ctx.trigger_type = Some("tool_call".into());
        ctx.trigger_payload = Some("read_file".into());

        assert!(!evaluate_with_regex(
            &parse(tokenize("trigger_payload =~ \"shell|bash\"").unwrap()).unwrap(),
            &ctx
        )
        .unwrap());
    }

    #[test]
    fn eval_trigger_type_eq_combined_with_regex() {
        let mut ctx = TriggerContext::new();
        ctx.trigger_type = Some("tool_call".into());
        ctx.trigger_payload = Some("bash".into());

        // trigger_type == "tool_call" and trigger_payload =~ "shell|bash"
        assert!(evaluate_trigger(
            "trigger_type == \"tool_call\" and trigger_payload =~ \"shell|bash\"",
            &ctx
        )
        .unwrap());
    }

    #[test]
    fn eval_regex_bad_pattern_errors() {
        let mut ctx = TriggerContext::new();
        ctx.trigger_payload = Some("foo".into());
        let expr = parse(tokenize("trigger_payload =~ \"[invalid\"").unwrap()).unwrap();
        assert!(matches!(
            evaluate_with_regex(&expr, &ctx),
            Err(TriggerError::Regex(_))
        ));
    }
}
