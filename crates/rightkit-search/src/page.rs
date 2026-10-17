//! Keyset (seek) paging over compiled predicates, and an opaque cursor that
//! carries the sort-key values of the last row a page returned.
//!
//! Offset paging skips or repeats rows when rows are inserted or deleted between
//! pages. A keyset cursor names the last row's sort values instead, so the next
//! page starts strictly after that row in the sort order.

use crate::sql::{Compiled, Param};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Asc,
    Desc,
}

impl Dir {
    fn keyword(self) -> &'static str {
        match self {
            Dir::Asc => "ASC",
            Dir::Desc => "DESC",
        }
    }

    /// Comparison that selects rows strictly after a cursor in this direction.
    fn after_op(self) -> &'static str {
        match self {
            Dir::Asc => ">",
            Dir::Desc => "<",
        }
    }
}

/// Sort columns for keyset paging, most significant first.
///
/// The last column must be unique (for example the primary key), or rows that
/// share every earlier sort value can be skipped or repeated. Every column must be
/// non-null, because a `NULL` never compares as strictly after a cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keyset {
    cols: Vec<(&'static str, Dir)>,
    range_bound: bool,
    /// Cursor-side replacements, as `(column index, expression)`. A column without one
    /// compares its sort expression with a plain `?` bound to the cursor value.
    cursor_exprs: Vec<(usize, CursorExpr)>,
}

/// Cursor-side right-hand side for one sort column: a SQL fragment whose last `?`
/// receives the cursor value for that column, with `params` bound before it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CursorExpr {
    sql: &'static str,
    params: Vec<Param>,
}

impl Keyset {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn desc(mut self, expr: &'static str) -> Self {
        self.cols.push((expr, Dir::Desc));
        self
    }

    pub fn asc(mut self, expr: &'static str) -> Self {
        self.cols.push((expr, Dir::Asc));
        self
    }

    /// Add a redundant bound on the first sort column (`<=` for `desc`, `>=` for
    /// `asc`) next to the cursor predicate, so an index on that column can seek to the
    /// page instead of scanning. It selects no row the cursor predicate would not, so
    /// output is unchanged. Off by default; with no cursor it adds nothing.
    pub fn range_bound(mut self) -> Self {
        self.range_bound = true;
        self
    }

    /// Replace the cursor-side value of sort column `column` (0-based, in the order
    /// the columns were added) with a SQL expression. The sort order and the `ORDER
    /// BY` term are unchanged; only the value the cursor predicate compares against
    /// changes.
    ///
    /// `sql` is the right-hand side of the comparison and may hold its own
    /// placeholders. `params` bind those placeholders in order; the cursor value for
    /// this column binds the final `?`, so `sql` must hold `params.len() + 1`
    /// placeholders. A `?` inside a SQL string literal is not supported.
    ///
    /// Example (MailRight thread tiebreak, a row whose rowid is looked up per account):
    /// `Keyset::new().asc("received_at").asc("rowid").cursor_expr(1,
    /// "COALESCE((SELECT rowid FROM messages WHERE account_id = ? AND id = ?), -1)",
    /// vec![Param::Int(account_id)])`. A cursor row that was deleted resolves to the
    /// fallback in `sql`, which the caller chooses, and the fallback decides what is
    /// lost or repeated: a fallback that matches no row skips the rest of that tie
    /// group, and one that matches every row repeats the part of it already shown.
    pub fn cursor_expr(mut self, column: usize, sql: &'static str, params: Vec<Param>) -> Self {
        self.cursor_exprs.push((column, CursorExpr { sql, params }));
        self
    }
}

/// Opaque position in a keyset ordering: the sort-key values of the last row a page
/// returned, in [`Keyset`] column order.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor(Vec<Param>);

/// Failure to decode a cursor, or a cursor whose arity does not match the keyset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CursorError {
    Malformed,
    Arity {
        expected: usize,
        got: usize,
    },
    /// The [`Keyset`] has no sort columns, so no page can be ordered.
    EmptyKeyset,
    /// A [`Keyset::cursor_expr`] names a column the keyset does not have.
    ColumnOutOfRange {
        column: usize,
        columns: usize,
    },
    /// A [`Keyset::cursor_expr`] has a placeholder count other than its params plus one.
    ExprBindCount {
        column: usize,
        expected: usize,
        got: usize,
    },
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => write!(f, "malformed cursor"),
            Self::Arity { expected, got } => {
                write!(
                    f,
                    "cursor has {got} values but the keyset has {expected} columns"
                )
            }
            Self::EmptyKeyset => write!(f, "keyset has no sort columns"),
            Self::ColumnOutOfRange { column, columns } => {
                write!(
                    f,
                    "cursor expression names column {column} but the keyset has {columns} columns"
                )
            }
            Self::ExprBindCount {
                column,
                expected,
                got,
            } => write!(
                f,
                "cursor expression for column {column} has {got} placeholders, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for CursorError {}

impl Cursor {
    pub fn new(v: Vec<Param>) -> Self {
        Cursor(v)
    }

    pub fn values(&self) -> &[Param] {
        &self.0
    }

    /// Text form: `k1`, then for each value `i<digits>;` (integer) or
    /// `t<byte_len>:<text>` (text). Length-prefixed text needs no escaping.
    pub fn encode(&self) -> String {
        let mut out = String::from("k1");
        for p in &self.0 {
            match p {
                Param::Int(i) => {
                    out.push('i');
                    out.push_str(&i.to_string());
                    out.push(';');
                }
                Param::Text(s) => {
                    out.push('t');
                    out.push_str(&s.len().to_string());
                    out.push(':');
                    out.push_str(s);
                }
            }
        }
        out
    }

    /// Inverse of [`Cursor::encode`]. Arity is not checked here because the keyset is
    /// unknown; [`Compiled::page`] checks it.
    pub fn decode(s: &str) -> Result<Self, CursorError> {
        let rest = s.strip_prefix("k1").ok_or(CursorError::Malformed)?;
        let bytes = rest.as_bytes();
        let mut values = Vec::new();
        let mut pos = 0usize;
        while pos < bytes.len() {
            match bytes[pos] {
                b'i' => {
                    let end = find_byte(bytes, pos + 1, b';').ok_or(CursorError::Malformed)?;
                    let n = parse_int(&rest[pos + 1..end]).ok_or(CursorError::Malformed)?;
                    values.push(Param::Int(n));
                    pos = end + 1;
                }
                b't' => {
                    let colon = find_byte(bytes, pos + 1, b':').ok_or(CursorError::Malformed)?;
                    let len_str = &rest[pos + 1..colon];
                    if len_str.is_empty() || !len_str.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(CursorError::Malformed);
                    }
                    let len: usize = len_str.parse().map_err(|_| CursorError::Malformed)?;
                    let start = colon + 1;
                    let end = start.checked_add(len).ok_or(CursorError::Malformed)?;
                    // `get` rejects a range that ends inside a multi-byte character.
                    let text = rest.get(start..end).ok_or(CursorError::Malformed)?;
                    values.push(Param::Text(text.to_string()));
                    pos = end;
                }
                _ => return Err(CursorError::Malformed),
            }
        }
        Ok(Cursor(values))
    }
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&b| b == needle)
        .map(|i| i + from)
}

/// Optional sign followed by one or more ASCII digits.
fn parse_int(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A keyset-paged statement and its parameters, in placeholder order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PagedSql {
    pub sql: String,
    pub params: Vec<Param>,
}

/// The predicate and ordering of one keyset page, for callers that build their own
/// statement (a CTE chain, or a `LIMIT` they substitute). The caller emits
/// `WHERE {where_sql} ORDER BY {order_by} LIMIT ?` and binds `params` first, then the
/// limit (`limit + 1`, as [`Compiled::page`] does).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PageClauses {
    /// Predicate without the `WHERE` keyword.
    pub where_sql: String,
    /// Parameters for `where_sql`, in placeholder order.
    pub params: Vec<Param>,
    /// `ORDER BY` terms without the keyword.
    pub order_by: String,
}

impl Keyset {
    /// One optional cursor-side expression per column, after checking that each names
    /// a real column and has one placeholder per param plus the cursor value. A later
    /// [`Keyset::cursor_expr`] for the same column replaces an earlier one.
    fn checked_cursor_exprs(&self) -> Result<Vec<Option<&CursorExpr>>, CursorError> {
        let n = self.cols.len();
        let mut bounds: Vec<Option<&CursorExpr>> = vec![None; n];
        for (column, expr) in &self.cursor_exprs {
            if *column >= n {
                return Err(CursorError::ColumnOutOfRange {
                    column: *column,
                    columns: n,
                });
            }
            let expected = expr.params.len() + 1;
            let got = expr.sql.bytes().filter(|&b| b == b'?').count();
            if got != expected {
                return Err(CursorError::ExprBindCount {
                    column: *column,
                    expected,
                    got,
                });
            }
            bounds[*column] = Some(expr);
        }
        Ok(bounds)
    }
}

/// The SQL for one column's cursor value, pushing its bound parameters in order: the
/// expression's own params, then the cursor value, or just the value for a plain `?`.
fn cursor_rhs(bound: Option<&CursorExpr>, value: &Param, params: &mut Vec<Param>) -> String {
    match bound {
        Some(expr) => {
            params.extend(expr.params.iter().cloned());
            params.push(value.clone());
            expr.sql.to_string()
        }
        None => {
            params.push(value.clone());
            "?".to_string()
        }
    }
}

impl Compiled {
    /// Build one keyset page over `select_from` (which carries no placeholders).
    ///
    /// The statement is `WHERE (<where_sql>) AND (<OR-chain>) ORDER BY … LIMIT ?`. The
    /// OR-chain is present only when `after` is `Some`. Parameters are bound in order:
    /// the compiled predicate's parameters, then the cursor values, then `limit + 1`.
    /// The caller keeps the first `limit` rows; a row beyond them means another page
    /// exists, and the cursor for it is built from the last kept row.
    ///
    /// # Errors
    ///
    /// [`CursorError::EmptyKeyset`] when `keyset` has no columns, and
    /// [`CursorError::Arity`] when `after` does not carry one value per column.
    pub fn page(
        &self,
        select_from: &str,
        keyset: &Keyset,
        after: Option<&Cursor>,
        limit: usize,
    ) -> Result<PagedSql, CursorError> {
        let clauses = self.page_clauses(keyset, after)?;
        let mut params = clauses.params;
        params.push(Param::Int(
            i64::try_from(limit).unwrap_or(i64::MAX).saturating_add(1),
        ));
        let sql = format!(
            "{select_from} WHERE {} ORDER BY {} LIMIT ?",
            clauses.where_sql, clauses.order_by
        );
        Ok(PagedSql { sql, params })
    }

    /// The predicate, parameters and ordering of one keyset page, without a `LIMIT`.
    ///
    /// Use this when the statement is not a plain `SELECT`, for example a CTE chain
    /// that substitutes its own `LIMIT`. The sort-key expressions in [`Keyset`] are
    /// inserted verbatim, so a scalar subquery (a lookup that resolves a tiebreak
    /// value per row) is a valid column expression; it is evaluated per row like any
    /// other.
    ///
    /// # Errors
    ///
    /// Same as [`Compiled::page`].
    pub fn page_clauses(
        &self,
        keyset: &Keyset,
        after: Option<&Cursor>,
    ) -> Result<PageClauses, CursorError> {
        if keyset.cols.is_empty() {
            return Err(CursorError::EmptyKeyset);
        }
        let n = keyset.cols.len();
        let bounds = keyset.checked_cursor_exprs()?;
        let mut params = self.params.clone();
        let mut where_sql = format!("({})", self.where_sql);

        if let Some(cursor) = after {
            let values = cursor.values();
            if values.len() != n {
                return Err(CursorError::Arity {
                    expected: n,
                    got: values.len(),
                });
            }
            if keyset.range_bound {
                let (expr, dir) = keyset.cols[0];
                let op = match dir {
                    Dir::Asc => ">=",
                    Dir::Desc => "<=",
                };
                let rhs = cursor_rhs(bounds[0], &values[0], &mut params);
                where_sql.push_str(&format!(" AND ({expr} {op} {rhs})"));
            }
            // Row i is after the cursor when columns 0..i equal the cursor and column i
            // is strictly after it. Placeholders and params are emitted in the same
            // left-to-right order, so each cursor value binds in its own position.
            let mut groups = Vec::with_capacity(n);
            for i in 0..n {
                let mut parts: Vec<String> = Vec::with_capacity(i + 1);
                for j in 0..i {
                    let rhs = cursor_rhs(bounds[j], &values[j], &mut params);
                    parts.push(format!("{} = {rhs}", keyset.cols[j].0));
                }
                let (expr, dir) = keyset.cols[i];
                let rhs = cursor_rhs(bounds[i], &values[i], &mut params);
                parts.push(format!("{expr} {} {rhs}", dir.after_op()));
                groups.push(format!("({})", parts.join(" AND ")));
            }
            where_sql.push_str(&format!(" AND ({})", groups.join(" OR ")));
        }

        let order_by: Vec<String> = keyset
            .cols
            .iter()
            .map(|(expr, dir)| format!("{expr} {}", dir.keyword()))
            .collect();

        Ok(PageClauses {
            where_sql,
            params,
            order_by: order_by.join(", "),
        })
    }
}
