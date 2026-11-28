//! Recursive-descent parser for the trigger DSL.
//!
//! Grammar:
//! ```text
//! expr     := or_expr
//! or_expr  := and_expr ( "or" and_expr )*
//! and_expr := not_expr ( "and" not_expr )*
//! not_expr := "not" not_expr | primary
//! primary  := "(" expr ")" | comparison | ident
//! comparison := ident op value
//! op       := "==" | "!=" | "contains" | "matches" | "=~"
//! value    := string_literal | ident
//! ```

use super::types::{CmpOp, Expr, Token, TriggerError};

/// Most tokens one trigger may hold. Bounds the AST size, and with it the depth of a
/// left-deep `or`/`and` chain that evaluation and drop walk recursively (audit 2026-11-05
/// skills H1: an unbounded trigger overflowed the worker stack and aborted the daemon).
pub const MAX_TRIGGER_TOKENS: usize = 1024;

/// Deepest `(` / `not` nesting the recursive-descent parser accepts.
pub const MAX_TRIGGER_NESTING: usize = 64;

/// Parse a token stream into an [`Expr`] AST.
///
/// Returns [`TriggerError::Parse`] instead of recursing without bound when the
/// stream exceeds [`MAX_TRIGGER_TOKENS`] or nests deeper than [`MAX_TRIGGER_NESTING`].
pub fn parse(tokens: Vec<Token>) -> Result<Expr, TriggerError> {
    if tokens.len() > MAX_TRIGGER_TOKENS {
        return Err(TriggerError::Parse(format!(
            "trigger has {} tokens; the limit is {MAX_TRIGGER_TOKENS}",
            tokens.len()
        )));
    }
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    let expr = parser.parse_expr()?;
    if parser.pos != parser.tokens.len() {
        return Err(TriggerError::UnexpectedToken(format!(
            "{:?}",
            parser.tokens[parser.pos]
        )));
    }
    Ok(expr)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// Enter one level of `(` or `not` nesting, failing past [`MAX_TRIGGER_NESTING`].
    fn descend(&mut self) -> Result<(), TriggerError> {
        self.depth += 1;
        if self.depth > MAX_TRIGGER_NESTING {
            return Err(TriggerError::Parse(format!(
                "trigger nests deeper than {MAX_TRIGGER_NESTING} levels"
            )));
        }
        Ok(())
    }

    fn advance(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_expr(&mut self) -> Result<Expr, TriggerError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, TriggerError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, TriggerError> {
        let mut left = self.parse_not()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, TriggerError> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.advance();
            self.descend()?;
            let inner = self.parse_not()?;
            self.depth -= 1;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, TriggerError> {
        match self.peek() {
            None => Err(TriggerError::UnexpectedEof),
            Some(Token::LParen) => {
                self.advance(); // consume '('
                self.descend()?;
                let inner = self.parse_expr()?;
                self.depth -= 1;
                match self.advance() {
                    Some(Token::RParen) => Ok(inner),
                    Some(other) => Err(TriggerError::UnexpectedToken(format!(
                        "{other:?}, expected ')'"
                    ))),
                    None => Err(TriggerError::UnexpectedEof),
                }
            }
            Some(Token::Ident(_)) => {
                // Either a bare var or a comparison.
                let name = match self.advance() {
                    Some(Token::Ident(n)) => n,
                    _ => unreachable!(),
                };
                match self.peek() {
                    Some(
                        Token::Eq | Token::Ne | Token::Contains | Token::Matches | Token::Regex,
                    ) => {
                        let op = match self.advance().expect("checked by peek") {
                            Token::Eq => CmpOp::Eq,
                            Token::Ne => CmpOp::Ne,
                            Token::Contains => CmpOp::Contains,
                            Token::Matches => CmpOp::Matches,
                            Token::Regex => CmpOp::Regex,
                            _ => unreachable!(),
                        };
                        let value = match self.advance() {
                            Some(Token::Str(s)) => s,
                            Some(Token::Ident(s)) => s,
                            Some(other) => {
                                return Err(TriggerError::UnexpectedToken(format!(
                                    "{other:?}, expected value"
                                )))
                            }
                            None => return Err(TriggerError::UnexpectedEof),
                        };
                        Ok(Expr::Cmp {
                            var: name,
                            op,
                            value,
                        })
                    }
                    _ => Ok(Expr::Var(name)),
                }
            }
            Some(other) => Err(TriggerError::UnexpectedToken(format!("{other:?}"))),
        }
    }
}
