//! SQL/FTS5 predicate compiler. A product describes how each operator maps to
//! its schema ([`Schema`]); [`Schema::compile`] turns a [`Query`] into a
//! parameterised `WHERE` body. Generalises MailRight's `predicates.rs`, and
//! adds negation for every operator and real OR groups over operators.

use crate::ast::Syntax;
use crate::terms::{OpTerm, Query};
use crate::values::{now_ms, parse_date_ms, parse_relative_ms, parse_size_bytes};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// `value%` for `LIKE` (wildcards in the value are escaped; SQL uses `ESCAPE`).
    Prefix,
    /// `%value` for `LIKE` (wildcards in the value are escaped; SQL uses `ESCAPE`).
    Suffix,
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
    Cmp {
        column: &'static str,
        cmp: &'static str,
        kind: ValueKind,
    },
    /// Arbitrary fragment containing exactly one `?` (e.g. a subquery).
    Sql { sql: &'static str, kind: ValueKind },
    /// Arbitrary fragment with one `?` per entry of `kinds`, in order. The same
    /// operator value is converted by each kind and bound to its `?`, so one
    /// predicate can match a prefix and a `@domain` suffix, for example
    /// `EXISTS (SELECT 1 FROM r WHERE r.m = m.id AND (r.addr LIKE ? ESCAPE '\' OR r.addr LIKE ? ESCAPE '\'))`
    /// with `kinds: &[ValueKind::Prefix, ValueKind::Suffix]`. A `?` inside a SQL
    /// string literal is not allowed (the count is checked textually); a count
    /// mismatch is [`CompileError::BindCount`].
    SqlMulti {
        sql: &'static str,
        kinds: &'static [ValueKind],
    },
    /// Like [`Binding::SqlMulti`], but the fragment is chosen from the shape of
    /// the (trimmed) operator value, e.g. a recipient `@domain`, a full
    /// `user@domain` address or a bare name. See [`ShapeSql`].
    ///
    /// Shape rules, applied to the trimmed value: starts with `@` -> `domain`;
    /// otherwise contains `@` -> `address`; otherwise `bare`. The value is always
    /// trimmed; with `lowercase` it is also lowercased (`str::to_lowercase`)
    /// before conversion. The value keeps its `@` when converted, so
    /// [`ValueKind::Suffix`] on a domain matches `%@corp.com`.
    SqlByShape {
        domain: ShapeSql,
        address: ShapeSql,
        bare: ShapeSql,
        lowercase: bool,
    },
    /// `value -> fragment` table without parameters (`is:unread`).
    Flags(&'static [(&'static str, &'static str)]),
}

/// One SQL fragment of a [`Binding::SqlByShape`] with one `?` per entry of
/// `kinds` (same rules as [`Binding::SqlMulti`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeSql {
    pub sql: &'static str,
    pub kinds: &'static [ValueKind],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    UnboundOperator(String),
    BadValue {
        op: String,
        value: String,
    },
    NoFtsTable,
    /// A [`Binding::SqlMulti`] fragment has a different number of `?` than `kinds`.
    BindCount {
        op: String,
        expected: usize,
        found: usize,
    },
    /// An operator with its own [`OpSource`] was used inside an OR group, which
    /// cannot be expressed across different child sources.
    OpSourceInOrGroup(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnboundOperator(op) => write!(f, "operator \"{op}\" has no column binding"),
            Self::BadValue { op, value } => write!(f, "invalid value \"{value}\" for \"{op}:\""),
            Self::NoFtsTable => write!(f, "free-text search requires an FTS table in the schema"),
            Self::BindCount {
                op,
                expected,
                found,
            } => write!(
                f,
                "operator \"{op}\" binding has {found} placeholders but {expected} value kinds"
            ),
            Self::OpSourceInOrGroup(op) => write!(
                f,
                "operator \"{op}\" has its own child source and cannot appear in an OR group"
            ),
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

/// Parent/child match scope for a search unit that groups rows (mail threads,
/// chat sessions, transcripts). Operator bindings and the FTS `rowid_expr`
/// describe the **child** row; the compiled `WHERE` body filters the **parent**.
///
/// With a scope set, [`Schema::compile`] emits:
/// - one `EXISTS (SELECT 1 FROM <from> WHERE <link> AND <positive terms>)` for all
///   positive terms, so they must co-occur in a single child row; and
/// - one `NOT EXISTS (SELECT 1 FROM <from> WHERE <link> AND <term>)` per negated
///   term, so a parent is dropped when any child matches the un-negated term.
///
/// Real OR groups stay inside the positive `EXISTS` (one child satisfies the group).
///
/// Optional refinements (builder methods, all default to the behaviour above):
/// - [`ChildScope::negated_text_like`]: a negated free-text term excludes a parent
///   when any listed column `LIKE`s the term **or** the FTS table matches it
///   (one `?` per column plus one for FTS).
/// - [`ChildScope::op_source`]: an operator whose columns live in a different child
///   table (`-label:` over `thread_labels`) gets its own `EXISTS`/`NOT EXISTS`
///   over that source. Positive terms of that operator share one `EXISTS` per source.
///   Several sources may be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildScope {
    /// Child row source with its alias, e.g. `"messages m"`.
    pub from: &'static str,
    /// Correlation predicate to the parent row, e.g. `"m.thread_id = t.id"`.
    pub link: &'static str,
    /// Columns a negated free-text term is `LIKE`-matched against in addition to FTS.
    pub negated_text_like: Vec<&'static str>,
    /// Negated free text is matched verbatim: see [`ChildScope::negated_text_unescaped`].
    pub negated_text_unescaped: bool,
    /// Operators evaluated over a different child source than `from`.
    pub op_sources: Vec<OpSource>,
}

/// A child source for one operator; see [`ChildScope::op_source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSource {
    /// Operator name as bound with [`Schema::bind`].
    pub op: &'static str,
    /// Child row source with its alias, e.g. `"thread_labels l"`.
    pub from: &'static str,
    /// Correlation predicate to the parent row, e.g. `"l.thread_id = t.id"`.
    pub link: &'static str,
}

impl ChildScope {
    pub fn new(from: &'static str, link: &'static str) -> Self {
        Self {
            from,
            link,
            negated_text_like: Vec::new(),
            negated_text_unescaped: false,
            op_sources: Vec::new(),
        }
    }

    /// Match negated free text against the [`negated_text_like`](Self::negated_text_like())
    /// columns verbatim: the pattern is `%term%` with `%`, `_` and `\` in the term
    /// left as `LIKE` metacharacters, and the SQL has no `ESCAPE` clause (a `\`
    /// is then a plain character). A term with no alphanumeric character is kept
    /// (matched by the LIKE columns only; it has no FTS form) instead of skipped.
    /// Only has an effect together with `negated_text_like`; the term is still
    /// a bound parameter. This reproduces an app's own unescaped contains match;
    /// the default (escaped, alphanumeric terms only) is the intended product
    /// behaviour.
    pub fn negated_text_unescaped(mut self) -> Self {
        self.negated_text_unescaped = true;
        self
    }

    /// Negated free text also excludes a parent when any of `columns` `LIKE`s the
    /// term (contains, wildcards escaped), besides the FTS match.
    pub fn negated_text_like(mut self, columns: &[&'static str]) -> Self {
        self.negated_text_like = columns.to_vec();
        self
    }

    /// Evaluate operator `op` over its own child source (its binding's columns
    /// must belong to `from`).
    pub fn op_source(mut self, op: &'static str, from: &'static str, link: &'static str) -> Self {
        self.op_sources.push(OpSource { op, from, link });
        self
    }

    fn source_index(&self, op: &str) -> Option<usize> {
        self.op_sources.iter().position(|s| s.op == op)
    }
}

fn exists_over(from: &str, link: &str, negated: bool, conds: &[String]) -> String {
    let not = if negated { "NOT " } else { "" };
    format!(
        "{not}EXISTS (SELECT 1 FROM {from} WHERE {link} AND {})",
        conds.join(" AND ")
    )
}

#[derive(Debug, Clone, Default)]
pub struct Schema {
    fts: Option<Fts>,
    bindings: Vec<(&'static str, Binding)>,
    scope: Option<ChildScope>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    /// `WHERE` body; `1=1` when the query is empty and `0=1` when it matches nothing.
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

    /// Group child rows under a parent; see [`ChildScope`]. Without it the
    /// compiler emits one flat `AND` list over a single row source.
    pub fn scope(mut self, scope: ChildScope) -> Self {
        self.scope = Some(scope);
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
        if q.matches_nothing() {
            return Ok(Compiled {
                where_sql: "0=1".to_string(),
                params: Vec::new(),
                fts_match: None,
            });
        }
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Param> = Vec::new();
        // Scoped mode only: each negated term as `(un-negated clause, params)`.
        // The source index is `Some(i)` for an operator with its own [`OpSource`].
        let mut scoped_neg: Vec<(Option<usize>, String, Vec<Param>)> = Vec::new();
        // Scoped mode only: positive terms of operators with their own source, per source.
        let mut source_pos: Vec<(Vec<String>, Vec<Param>)> = self
            .scope
            .as_ref()
            .map(|s| {
                s.op_sources
                    .iter()
                    .map(|_| (Vec::new(), Vec::new()))
                    .collect()
            })
            .unwrap_or_default();
        let scoped = self.scope.is_some();

        let fts_match = q.fts();
        if let Some(expr) = &fts_match {
            let fts = self.fts.as_ref().ok_or(CompileError::NoFtsTable)?;
            clauses.push(fts_clause(fts, false));
            params.push(Param::Text(expr.clone()));
        }
        // `fts_negated` keeps exactly the terms with an alphanumeric character, in order.
        let mut fts_neg = q.fts_negated().into_iter();
        for raw in &q.negated_text {
            let neg = if raw.chars().any(char::is_alphanumeric) {
                fts_neg.next()
            } else {
                None
            };
            if let Some(scope) = &self.scope {
                // A term without an alphanumeric character has no FTS form. It is
                // skipped (0.4.0 default) unless the scope keeps it for its LIKE columns.
                if neg.is_none()
                    && (!scope.negated_text_unescaped || scope.negated_text_like.is_empty())
                {
                    continue;
                }
                let mut parts: Vec<String> = Vec::new();
                let mut p: Vec<Param> = Vec::new();
                for col in &scope.negated_text_like {
                    if scope.negated_text_unescaped {
                        parts.push(format!("{col} LIKE ?"));
                        p.push(Param::Text(format!("%{raw}%")));
                    } else {
                        parts.push(format!("{col} LIKE ? ESCAPE '\\'"));
                        p.push(Param::Text(format!("%{}%", escape_like(raw))));
                    }
                }
                if let Some(neg) = neg {
                    let fts = self.fts.as_ref().ok_or(CompileError::NoFtsTable)?;
                    parts.push(fts_clause(fts, false));
                    p.push(Param::Text(neg));
                }
                scoped_neg.push((None, parts.join(" OR "), p));
            } else if let Some(neg) = neg {
                let fts = self.fts.as_ref().ok_or(CompileError::NoFtsTable)?;
                clauses.push(fts_clause(fts, true));
                params.push(Param::Text(neg));
            }
        }
        for term in &q.ops {
            if let Some(scope) = &self.scope {
                if let Some(i) = scope.source_index(&term.op) {
                    let (sql, p) = self.term_core(term, now)?;
                    if term.negated {
                        scoped_neg.push((Some(i), sql, p));
                    } else {
                        source_pos[i].0.push(format!("({sql})"));
                        source_pos[i].1.extend(p);
                    }
                    continue;
                }
            }
            if scoped && term.negated {
                let (sql, p) = self.term_core(term, now)?;
                scoped_neg.push((None, sql, p));
                continue;
            }
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
                    (
                        sql,
                        vec![Param::Text(fts_term(&term.value, term.is_phrase))],
                    )
                } else {
                    if let Some(scope) = &self.scope {
                        if scope.source_index(&term.op).is_some() {
                            return Err(CompileError::OpSourceInOrGroup(term.op.clone()));
                        }
                    }
                    self.term_sql(term, now)?
                };
                parts.push(sql);
                params.extend(p);
            }
            clauses.push(format!("({})", parts.join(" OR ")));
        }
        if let Some(scope) = &self.scope {
            let mut parent: Vec<String> = Vec::new();
            let mut parent_params: Vec<Param> = Vec::new();
            if !clauses.is_empty() {
                parent.push(exists_over(scope.from, scope.link, false, &clauses));
                parent_params.extend(params);
            }
            for (src, (conds, p)) in scope.op_sources.iter().zip(source_pos) {
                if !conds.is_empty() {
                    parent.push(exists_over(src.from, src.link, false, &conds));
                    parent_params.extend(p);
                }
            }
            for (src, sql, p) in scoped_neg {
                let (from, link) = match src {
                    Some(i) => (scope.op_sources[i].from, scope.op_sources[i].link),
                    None => (scope.from, scope.link),
                };
                parent.push(exists_over(from, link, true, &[format!("({sql})")]));
                parent_params.extend(p);
            }
            params = parent_params;
            clauses = parent;
        }
        let where_sql = if clauses.is_empty() {
            "1=1".to_string()
        } else {
            clauses.join(" AND ")
        };
        Ok(Compiled {
            where_sql,
            params,
            fts_match,
        })
    }

    fn term_sql(&self, term: &OpTerm, now: i64) -> Result<(String, Vec<Param>), CompileError> {
        let (sql, params) = self.term_core(term, now)?;
        // NULL-safe negation: an unknown column value must not satisfy `-op:x` by accident.
        let sql = if term.negated {
            format!("NOT COALESCE(({sql}), 0)")
        } else {
            format!("({sql})")
        };
        Ok((sql, params))
    }

    /// The un-negated SQL fragment and params for a term.
    fn term_core(&self, term: &OpTerm, now: i64) -> Result<(String, Vec<Param>), CompileError> {
        let binding = self
            .bindings
            .iter()
            .find(|(op, _)| *op == term.op)
            .map(|(_, b)| b)
            .ok_or_else(|| CompileError::UnboundOperator(term.op.clone()))?;
        let bad = || CompileError::BadValue {
            op: term.op.clone(),
            value: term.value.clone(),
        };
        let (sql, params) = match binding {
            Binding::Like(col) => (
                format!("{col} LIKE ? ESCAPE '\\'"),
                vec![convert(ValueKind::Contains, &term.value, now).ok_or_else(bad)?],
            ),
            Binding::Eq(col) => (format!("{col} = ?"), vec![Param::Text(term.value.clone())]),
            Binding::Cmp { column, cmp, kind } => (
                format!("{column} {cmp} ?"),
                vec![convert(*kind, &term.value, now).ok_or_else(bad)?],
            ),
            Binding::Sql { sql, kind } => (
                (*sql).to_string(),
                vec![convert(*kind, &term.value, now).ok_or_else(bad)?],
            ),
            Binding::SqlMulti { sql, kinds } => bind_multi(term, sql, kinds, &term.value, now)?,
            Binding::SqlByShape {
                domain,
                address,
                bare,
                lowercase,
            } => {
                let trimmed = term.value.trim();
                let value = if *lowercase {
                    trimmed.to_lowercase()
                } else {
                    trimmed.to_string()
                };
                let shape = if value.starts_with('@') {
                    domain
                } else if value.contains('@') {
                    address
                } else {
                    bare
                };
                bind_multi(term, shape.sql, shape.kinds, &value, now)?
            }
            Binding::Flags(table) => {
                let lower = term.value.to_ascii_lowercase();
                let frag = table
                    .iter()
                    .find(|(v, _)| *v == lower)
                    .map(|(_, f)| *f)
                    .ok_or_else(bad)?;
                ((*frag).to_string(), Vec::new())
            }
        };
        Ok((sql, params))
    }
}

/// One `?` per kind (checked textually), each bound to `value` converted by that kind.
fn bind_multi(
    term: &OpTerm,
    sql: &str,
    kinds: &[ValueKind],
    value: &str,
    now: i64,
) -> Result<(String, Vec<Param>), CompileError> {
    let found = sql.matches('?').count();
    if found != kinds.len() {
        return Err(CompileError::BindCount {
            op: term.op.clone(),
            expected: kinds.len(),
            found,
        });
    }
    let mut ps = Vec::with_capacity(kinds.len());
    for kind in kinds {
        ps.push(
            convert(*kind, value, now).ok_or_else(|| CompileError::BadValue {
                op: term.op.clone(),
                value: term.value.clone(),
            })?,
        );
    }
    Ok((sql.to_string(), ps))
}

fn fts_clause(fts: &Fts, negated: bool) -> String {
    let not = if negated { "NOT " } else { "" };
    format!(
        "{} {not}IN (SELECT rowid FROM {t} WHERE {t} MATCH ?)",
        fts.rowid_expr,
        t = fts.table
    )
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
        ValueKind::Prefix => Param::Text(format!("{}%", escape_like(value))),
        ValueKind::Suffix => Param::Text(format!("%{}", escape_like(value))),
        ValueKind::Int => Param::Int(value.trim().parse().ok()?),
        ValueKind::SizeBytes => Param::Int(parse_size_bytes(value)?),
        ValueKind::DateMs => Param::Int(parse_date_ms(value)?),
        ValueKind::RelativeMs => Param::Int(parse_relative_ms(value, now)?),
        ValueKind::BareId => Param::Text(value.trim().trim_matches(['<', '>']).to_string()),
    })
}
