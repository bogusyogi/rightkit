//! Compiled query accumulator: the shape SQL/FTS builders consume.
//! Generalises MailRight's mail-specific `SearchTerms`: operators are kept as
//! data ([`OpTerm`]) so each product binds them to its own columns.

use crate::ast::{self, Leaf, Node, ParseError, Syntax};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpTerm {
    pub op: String,
    pub value: String,
    pub negated: bool,
    pub is_phrase: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// ANDed operator terms (`from:alice`, `-label:x`, ...).
    pub ops: Vec<OpTerm>,
    /// `a OR b` / `{a b}` groups that contain at least one operator leaf. Each group is a disjunction.
    pub op_or_groups: Vec<Vec<OpTerm>>,
    pub text: Vec<String>,
    pub phrases: Vec<String>,
    /// Free-text disjunctions: `(a OR b)`.
    pub or_groups: Vec<Vec<String>>,
    pub negated_text: Vec<String>,
}

fn clean(value: &str) -> String {
    value.chars().filter(|c| !"\"*()".contains(*c)).collect()
}

impl Query {
    pub fn parse(input: &str, syntax: &Syntax) -> Result<Self, ParseError> {
        Ok(Self::from_nodes(ast::parse(input, syntax)?))
    }

    /// Infallible variant: a malformed query matches nothing (empty query).
    pub fn parse_lenient(input: &str, syntax: &Syntax) -> Self {
        Self::parse(input, syntax).unwrap_or_default()
    }

    pub fn from_nodes(nodes: Vec<Node>) -> Self {
        let mut q = Self::default();
        for node in nodes {
            match node {
                Node::Leaf(leaf) => q.apply(&leaf),
                Node::Or(leaves) => {
                    let all_text = leaves.iter().all(|l| l.op.is_none() && !l.negated);
                    if all_text {
                        let group: Vec<String> =
                            leaves.iter().map(|l| clean(&l.value)).filter(|v| !v.is_empty()).collect();
                        if !group.is_empty() {
                            q.or_groups.push(group);
                        }
                    } else {
                        // Mixed/operator disjunctions stay disjunctions (donor flattened them to AND).
                        let group: Vec<OpTerm> = leaves.iter().filter_map(Self::op_term).collect();
                        if !group.is_empty() {
                            q.op_or_groups.push(group);
                        }
                    }
                }
            }
        }
        q
    }

    /// Operator leaf, or a bare-text leaf represented with op "" (only inside mixed OR groups).
    fn op_term(leaf: &Leaf) -> Option<OpTerm> {
        let value = leaf.value.trim().to_string();
        if value.is_empty() {
            return None;
        }
        Some(OpTerm {
            op: leaf.op.clone().unwrap_or_default(),
            value,
            negated: leaf.negated,
            is_phrase: leaf.is_phrase,
        })
    }

    fn apply(&mut self, leaf: &Leaf) {
        if leaf.op.is_some() {
            if let Some(t) = Self::op_term(leaf) {
                self.ops.push(t);
            }
            return;
        }
        let value = if leaf.is_phrase { leaf.value.trim().to_string() } else { clean(&leaf.value) };
        if value.is_empty() {
            return;
        }
        match (leaf.negated, leaf.is_phrase) {
            (true, _) => self.negated_text.push(value),
            (false, true) => self.phrases.push(value),
            (false, false) => self.text.push(value),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
            && self.op_or_groups.is_empty()
            && self.text.is_empty()
            && self.phrases.is_empty()
            && self.or_groups.is_empty()
            && self.negated_text.is_empty()
    }

    pub fn op_values<'a>(&'a self, op: &'a str) -> impl Iterator<Item = &'a OpTerm> {
        self.ops.iter().filter(move |t| t.op == op)
    }

    /// FTS5 MATCH expression for positive free text: bare words are quoted
    /// prefix terms (so `appeal/x` cannot raise an FTS syntax error), phrases
    /// are exact FTS phrases, OR groups are parenthesised.
    pub fn fts(&self) -> Option<String> {
        let alnum = |t: &String| t.chars().any(char::is_alphanumeric);
        let prefix = |t: &str| format!("\"{}\"*", t.replace('"', "\"\""));
        let phrase = |t: &str| format!("\"{}\"", t.replace('"', "\"\""));
        let mut parts: Vec<String> = self.text.iter().filter(|t| alnum(t)).map(|t| prefix(t)).collect();
        parts.extend(self.phrases.iter().filter(|t| alnum(t)).map(|t| phrase(t)));
        for g in &self.or_groups {
            let terms: Vec<String> = g.iter().filter(|t| alnum(t)).map(|t| prefix(t)).collect();
            match terms.len() {
                0 => {}
                1 => parts.push(terms[0].clone()),
                _ => parts.push(format!("({})", terms.join(" OR "))),
            }
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// FTS5 expression for one negated text term (use as `NOT IN (... MATCH ?)`).
    pub fn fts_negated(&self) -> Vec<String> {
        self.negated_text
            .iter()
            .filter(|t| t.chars().any(char::is_alphanumeric))
            .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
            .collect()
    }

    pub fn positive_text_terms(&self) -> Vec<String> {
        let mut t = self.text.clone();
        t.extend(self.phrases.clone());
        t.extend(self.or_groups.iter().flatten().cloned());
        t
    }
}
