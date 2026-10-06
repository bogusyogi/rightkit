//! SQL/FTS5 predicate compiler. A product describes how each operator maps to
//! its schema ([`Schema`]); [`Schema::compile`] turns a [`Query`] into a
//! parameterised `WHERE` body. Generalises MailRight's `predicates.rs`, and
//! adds negation for every operator and real OR groups over operators.

use crate::ast::Syntax;
use crate::terms::{OpTerm, Query};
use crate::values::{now_ms, parse_date_ms, parse_relative_ms, parse_size_bytes};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Text(String),
    Int(i64),
}

#[cfg(feature = "sqlite")]
impl rusqlite::ToSql for Param {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Param::Text(s) => s.to_sql(),
            Param::Int(i) => i.to_sql(),
        }
    }
}

/// How an operator value is converted before binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// Bound verbatim.
    Text,
    /// `%value%` for `LIKE` (wildcards in the value are escaped; SQL uses `ESCAPE`).
    Contains,
    /// Plain integer.
    Int,
    /// `5k`/`2m`/`1g` bytes.
    SizeBytes,
    /// `YYYY-MM-DD` -> epoch ms.
    DateMs,
    /// `7d`/`2m`/`1y` -> cutoff epoch ms relative to now.
    RelativeMs,
    /// Message-Id style: angle brackets stripped.
    BareId,
}

#[derive(Debug, Clone)]
pub enum Binding {
    /// `column LIKE ? ESCAPE` with a contains pattern.
    Like(&'static str),
    /// `column = ?`.
    Eq(&'static str),
    /// `column <cmp> ?` with a converted value (`cmp` is one of `= < <= > >=`).
    Cmp { column: &'static str, cmp: &'static str, kind: ValueKind },
    /// Arbitrary fragment containing exactly one `?` (e.g. a subquery).
    Sql { sql: &'static str, kind: ValueKind },
    /// `value -> fragment` table without parameters (`is:unread`).
    Flags(&'static [(&'static str, &'static str)]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    UnboundOperator(String),
    BadValue { op: String, value: String },
    NoFtsTable,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnboundOperator(op) => write!(f, "operator \"{op}\" has no column binding"),
            Self::BadValue { op, value } => write!(f, "invalid value \"{value}\" for \"{op}:\""),
            Self::NoFtsTable => write!(f, "free-text search requires an FTS table in the schema"),
        }
    }
}

impl std::error::Error for CompileError {}

#[derive(Debug, Clone)]
pub struct Fts {
    pub table: &'static str,
    /// Expression on the main table matching the FTS rowid (e.g. `m.rowid`).
    pub rowid_expr: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Schema {
    fts: Option<Fts>,
    bindings: Vec<(&'static str, Binding)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    /// `WHERE` body; `1=1` when the query is empty.
    pub where_sql: String,
    pub params: Vec<Param>,
    /// The positive FTS5 MATCH expression, for snippet/rank queries.
    pub fts_match: Option<String>,
}

impl Compiled {
    pub fn select(&self, select_from: &str) -> String {
        format!("{select_from} WHERE {}", self.where_sql)
    }
}

impl Schema {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fts(mut self, table: &'static str, rowid_expr: &'static str) -> Self {
        self.fts = Some(Fts { table, rowid_expr });
        self
    }

    pub fn bind(mut self, op: &'static str, binding: Binding) -> Self {
        self.bindings.push((op, binding));
        self
    }

    pub fn syntax(&self) -> Syntax {
        Syntax::new(self.bindings.iter().map(|(op, _)| *op))
    }

    pub fn parse(&self, input: &str) -> Result<Query, crate::ast::ParseError> {
        Query::parse(input, &self.syntax())
    }

    pub fn compile(&self, q: &Query) -> Result<Compiled, CompileError> {
        self.compile_at(q, now_ms())
    }

    /// Deterministic variant with an explicit "now" for relative dates.
    pub fn compile_at(&self, q: &Query, now: i64) -> Result<Compiled, CompileError> {
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Param> = Vec::new();

        let fts_match = q.fts();
        if let Some(expr) = &fts_match {
            let fts = self.fts.as_ref().ok_or(CompileError::NoFtsTable)?;
            clauses.push(fts_clause(fts, false));
            params.push(Param::Text(expr.clone()));
        }
        for neg in q.fts_negated() {
            let fts = self.fts.as_ref().ok_or(CompileError::NoFtsTable)?;
            clauses.push(fts_clause(fts, true));
            params.push(Param::Text(neg));
        }
        for term in &q.ops {
            let (sql, p) = self.term_sql(term, now)?;
            clauses.push(sql);
            params.extend(p);
        }
        for group in &q.op_or_groups {
            let mut parts = Vec::new();
            for term in group {
                let (sql, p) = if term.op.is_empty() {
                    let fts = self.fts.as_ref().ok_or(CompileError::NoFtsTable)?;
                    let sql = fts_clause(fts, term.negated);
                    (sql, vec![Param::Text(fts_term(&term.value, term.is_phrase))])
                } else {
                    self.term_sql(term, now)?
                };
                parts.push(sql);
                params.extend(p);
            }
            clauses.push(format!("({})", parts.join(" OR ")));
        }
        let where_sql = if clauses.is_empty() { "1=1".to_string() } else { clauses.join(" AND ") };
        Ok(Compiled { where_sql, params, fts_match })
    }

    fn term_sql(&self, term: &OpTerm, now: i64) -> Result<(String, Vec<Param>), CompileError> {
        let binding = self
            .bindings
            .iter()
            .find(|(op, _)| *op == term.op)
            .map(|(_, b)| b)
            .ok_or_else(|| CompileError::UnboundOperator(term.op.clone()))?;
        let bad = || CompileError::BadValue { op: term.op.clone(), value: term.value.clone() };
        let (sql, params) = match binding {
            Binding::Like(col) => (
                format!("{col} LIKE ? ESCAPE '\\'"),
                vec![convert(ValueKind::Contains, &term.value, now).ok_or_else(bad)?],
            ),
            Binding::Eq(col) => (format!("{col} = ?"), vec![Param::Text(term.value.clone())]),
            Binding::Cmp { column, cmp, kind } => {
                (format!("{column} {cmp} ?"), vec![convert(*kind, &term.value, now).ok_or_else(bad)?])
            }
            Binding::Sql { sql, kind } => {
                ((*sql).to_string(), vec![convert(*kind, &term.value, now).ok_or_else(bad)?])
            }
            Binding::Flags(table) => {
                let lower = term.value.to_ascii_lowercase();
                let frag = table.iter().find(|(v, _)| *v == lower).map(|(_, f)| *f).ok_or_else(bad)?;
                ((*frag).to_string(), Vec::new())
            }
        };
        // NULL-safe negation: an unknown column value must not satisfy `-op:x` by accident.
        let sql = if term.negated { format!("NOT COALESCE(({sql}), 0)") } else { format!("({sql})") };
        Ok((sql, params))
    }
}

fn fts_clause(fts: &Fts, negated: bool) -> String {
    let not = if negated { "NOT " } else { "" };
    format!("{} {not}IN (SELECT rowid FROM {t} WHERE {t} MATCH ?)", fts.rowid_expr, t = fts.table)
}

fn fts_term(value: &str, phrase: bool) -> String {
    let q = value.replace('"', "\"\"");
    if phrase {
        format!("\"{q}\"")
    } else {
        format!("\"{q}\"*")
    }
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn convert(kind: ValueKind, value: &str, now: i64) -> Option<Param> {
    Some(match kind {
        ValueKind::Text => Param::Text(value.to_string()),
        ValueKind::Contains => Param::Text(format!("%{}%", escape_like(value))),
        ValueKind::Int => Param::Int(value.trim().parse().ok()?),
        ValueKind::SizeBytes => Param::Int(parse_size_bytes(value)?),
        ValueKind::DateMs => Param::Int(parse_date_ms(value)?),
        ValueKind::RelativeMs => Param::Int(parse_relative_ms(value, now)?),
        ValueKind::BareId => Param::Text(value.trim().trim_matches(['<', '>']).to_string()),
    })
}
