//! Recursive-descent parser: tokens -> flat AND list of [`Node`]s.
//!
//! Grammar: `query := term*`, `term := '-'? (group | or_chain | leaf)`,
//! `group := '(' term* ')'` (spliced into the parent AND list),
//! `or_chain := '{' leaf+ '}' | leaf ('OR' leaf)+`, `leaf := Word | Phrase`.
//!
//! Unlike the donor, the set of known operators is supplied by the caller
//! ([`Syntax`]), and an unknown `word:value` is a typed error instead of
//! being degraded to free text.

use crate::lexer::{tokenize, Token};
use std::fmt;

/// The operator vocabulary a product accepts (lowercase names, no colon).
#[derive(Debug, Clone, Default)]
pub struct Syntax {
    operators: Vec<String>,
}

impl Syntax {
    pub fn new<I, S>(operators: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            operators: operators
                .into_iter()
                .map(|s| s.as_ref().to_ascii_lowercase())
                .collect(),
        }
    }

    pub fn knows(&self, op: &str) -> bool {
        self.operators.iter().any(|o| o == op)
    }

    pub fn operators(&self) -> &[String] {
        &self.operators
    }
}

/// One leaf: operator (or bare text) with negation and phrase flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub op: Option<String>,
    pub value: String,
    pub negated: bool,
    pub is_phrase: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Leaf(Leaf),
    Or(Vec<Leaf>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnsupportedOperator { operator: String, raw: String },
    UnbalancedGroup { raw: String },
    EmptyGroup,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOperator { operator, raw } => {
                write!(
                    f,
                    "unsupported search operator \"{operator}:\" in \"{raw}\""
                )
            }
            Self::UnbalancedGroup { raw } => write!(f, "unbalanced grouping near \"{raw}\""),
            Self::EmptyGroup => write!(f, "empty group"),
        }
    }
}

impl std::error::Error for ParseError {}

fn known_split(word: &str, syntax: &Syntax) -> Option<(String, String)> {
    let (op, rest) = word.split_once(':')?;
    let op = op.to_ascii_lowercase();
    syntax.knows(&op).then(|| (op, rest.to_string()))
}

fn unknown_operator(word: &str, syntax: &Syntax) -> Option<String> {
    let (op, rest) = word.split_once(':')?;
    if op.is_empty() || rest.is_empty() || rest.starts_with("//") {
        return None;
    }
    if !op.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let op = op.to_ascii_lowercase();
    (!syntax.knows(&op)).then_some(op)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    syntax: &'a Syntax,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn query(&mut self) -> Result<Vec<Node>, ParseError> {
        let mut out = Vec::new();
        while let Some(t) = self.peek() {
            if matches!(t, Token::RParen | Token::RBrace) {
                return Err(ParseError::UnbalancedGroup {
                    raw: format!("{t:?}"),
                });
            }
            self.term(&mut out)?;
        }
        Ok(out)
    }

    fn term(&mut self, out: &mut Vec<Node>) -> Result<(), ParseError> {
        match self.peek().cloned() {
            Some(Token::LParen) => {
                self.next();
                out.extend(self.until(Token::RParen)?);
            }
            Some(Token::LBrace) => {
                self.next();
                out.push(Node::Or(self.leaves_until(Token::RBrace)?));
            }
            Some(Token::Or) => {
                self.next();
            }
            Some(_) => {
                let leaf = self.leaf()?;
                if matches!(self.peek(), Some(Token::Or)) {
                    let mut leaves = vec![leaf];
                    while matches!(self.peek(), Some(Token::Or)) {
                        self.next();
                        match self.peek() {
                            Some(Token::LParen | Token::LBrace) | None => break,
                            _ => leaves.push(self.leaf()?),
                        }
                    }
                    out.push(Node::Or(leaves));
                } else {
                    out.push(Node::Leaf(leaf));
                }
            }
            None => {}
        }
        Ok(())
    }

    fn until(&mut self, closer: Token) -> Result<Vec<Node>, ParseError> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Some(t) if *t == closer => {
                    self.next();
                    break;
                }
                None => {
                    return Err(ParseError::UnbalancedGroup {
                        raw: "end of query".into(),
                    })
                }
                _ => self.term(&mut out)?,
            }
        }
        if out.is_empty() {
            return Err(ParseError::EmptyGroup);
        }
        Ok(out)
    }

    fn leaves_until(&mut self, closer: Token) -> Result<Vec<Leaf>, ParseError> {
        let mut leaves = Vec::new();
        loop {
            match self.peek() {
                Some(t) if *t == closer => {
                    self.next();
                    break;
                }
                None => {
                    return Err(ParseError::UnbalancedGroup {
                        raw: "end of query".into(),
                    })
                }
                Some(Token::Or) => {
                    self.next();
                }
                _ => leaves.push(self.leaf()?),
            }
        }
        if leaves.is_empty() {
            return Err(ParseError::EmptyGroup);
        }
        Ok(leaves)
    }

    fn leaf(&mut self) -> Result<Leaf, ParseError> {
        match self.next() {
            Some(Token::Word(w)) => {
                let (negated, w) = match w.strip_prefix('-') {
                    Some(rest) => (true, rest.to_string()),
                    None => (false, w),
                };
                if let Some((op, value)) = known_split(&w, self.syntax) {
                    return Ok(Leaf {
                        op: Some(op),
                        value,
                        negated,
                        is_phrase: false,
                    });
                }
                if let Some(operator) = unknown_operator(&w, self.syntax) {
                    return Err(ParseError::UnsupportedOperator { operator, raw: w });
                }
                let value: String = w.chars().filter(|c| !"\"*()".contains(*c)).collect();
                Ok(Leaf {
                    op: None,
                    value,
                    negated,
                    is_phrase: false,
                })
            }
            Some(Token::Phrase { op_prefix, body }) => {
                let mut negated_bare = false;
                if let Some(p) = op_prefix {
                    negated_bare = p == "-";
                    let negated = p.starts_with('-');
                    let trimmed = p.strip_prefix('-').unwrap_or(&p);
                    let op = trimmed.trim_end_matches(':').to_ascii_lowercase();
                    if self.syntax.knows(&op) {
                        return Ok(Leaf {
                            op: Some(op),
                            value: body,
                            negated,
                            is_phrase: true,
                        });
                    }
                    if !op.is_empty() {
                        return Err(ParseError::UnsupportedOperator {
                            operator: op,
                            raw: format!("{trimmed}\"{body}\""),
                        });
                    }
                }
                // `-"phrase"` arrives with prefix "-"; the donor dropped that negation.
                Ok(Leaf {
                    op: None,
                    value: body,
                    negated: negated_bare,
                    is_phrase: true,
                })
            }
            other => Err(ParseError::UnbalancedGroup {
                raw: format!("{other:?}"),
            }),
        }
    }
}

pub fn parse(input: &str, syntax: &Syntax) -> Result<Vec<Node>, ParseError> {
    Parser {
        tokens: tokenize(input),
        pos: 0,
        syntax,
    }
    .query()
}
