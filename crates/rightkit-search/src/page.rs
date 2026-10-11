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
        if keyset.cols.is_empty() {
            return Err(CursorError::EmptyKeyset);
        }
        let n = keyset.cols.len();
        let mut params = self.params.clone();
        let mut sql = format!("{select_from} WHERE ({})", self.where_sql);

        if let Some(cursor) = after {
            let values = cursor.values();
            if values.len() != n {
                return Err(CursorError::Arity {
                    expected: n,
                    got: values.len(),
                });
            }
            // Row i is after the cursor when columns 0..i equal the cursor and column i
            // is strictly after it. Groups are emitted and bound in the same order.
            let mut groups = Vec::with_capacity(n);
            for i in 0..n {
                let mut parts: Vec<String> = keyset.cols[..i]
                    .iter()
                    .map(|(expr, _)| format!("{expr} = ?"))
                    .collect();
                let (expr, dir) = keyset.cols[i];
                parts.push(format!("{expr} {} ?", dir.after_op()));
                groups.push(format!("({})", parts.join(" AND ")));
                params.extend(values[..=i].iter().cloned());
            }
            sql.push_str(&format!(" AND ({})", groups.join(" OR ")));
        }

        let order: Vec<String> = keyset
            .cols
            .iter()
            .map(|(expr, dir)| format!("{expr} {}", dir.keyword()))
            .collect();
        sql.push_str(&format!(" ORDER BY {} LIMIT ?", order.join(", ")));
        let fetch = i64::try_from(limit).unwrap_or(i64::MAX).saturating_add(1);
        params.push(Param::Int(fetch));

        Ok(PagedSql { sql, params })
    }
}
