//! FTS5 helpers for ranked, exact-term search over a table the caller owns: an exact-term MATCH
//! builder, a weighted BM25 score, and the statements that create, rebuild, drop and keep an
//! external-content FTS5 table in step with its source table.
//!
//! Everything here builds SQL text only. Values are never interpolated: the MATCH expression is
//! bound as a parameter, and every name that is interpolated is checked to be a plain identifier.

use std::fmt;

/// Most column weights one [`Bm25Weights`] accepts.
pub const MAX_BM25_COLUMNS: usize = 32;
/// Largest row limit [`Compiled::ranked_bm25_select`](crate::sql::Compiled::ranked_bm25_select) accepts.
pub const MAX_RANK_LIMIT: usize = 100_000;
/// Longest identifier accepted for a table, column, trigger or rowid name.
const MAX_IDENT_BYTES: usize = 128;

/// Why an FTS5 statement could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FtsError {
    /// A name is not a plain SQL identifier (`[A-Za-z_][A-Za-z0-9_]*`, at most 128 bytes).
    BadIdentifier(String),
    /// Column weights are empty, too many, or not finite and non-negative.
    BadWeights(String),
    /// A row limit outside `1..=MAX_RANK_LIMIT`.
    BadLimit(usize),
    /// A tokenizer specification contains a character outside `[A-Za-z0-9_ .-]`.
    BadTokenizer(String),
    /// The table has no external content table, so triggers cannot keep it in step.
    NotExternalContent,
    /// An FTS5 table needs at least one indexed column.
    NoColumns,
}

impl fmt::Display for FtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadIdentifier(name) => write!(f, "not a plain SQL identifier: {name:?}"),
            Self::BadWeights(why) => write!(f, "invalid BM25 column weights: {why}"),
            Self::BadLimit(limit) => write!(f, "row limit {limit} is outside 1..={MAX_RANK_LIMIT}"),
            Self::BadTokenizer(spec) => {
                write!(f, "tokenizer spec has a disallowed character: {spec:?}")
            }
            Self::NotExternalContent => {
                write!(f, "triggers need an external content table (content_table)")
            }
            Self::NoColumns => write!(f, "an FTS5 table needs at least one column"),
        }
    }
}

impl std::error::Error for FtsError {}

/// Accept only a plain SQL identifier: the only names interpolated into generated SQL.
pub fn check_identifier(name: &str) -> Result<(), FtsError> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let ok = first_ok
        && name.len() <= MAX_IDENT_BYTES
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(FtsError::BadIdentifier(name.to_string()))
    }
}

/// One exact FTS5 string: the term in double quotes, with inner quotes doubled, so no operator,
/// `*`, `:` or `OR` in the term can change the query. `None` when the term has no letter or digit,
/// because it has no tokens to match.
fn quote_exact(term: &str) -> Option<String> {
    term.chars()
        .any(char::is_alphanumeric)
        .then(|| format!("\"{}\"", term.replace('"', "\"\"")))
}

/// FTS5 MATCH expression for exact terms joined by `OR`, for use as the bound MATCH parameter.
/// Each term is one exact string (no prefix matching). Blank and punctuation-only terms are
/// skipped. `None` when no term is left, so the caller can skip the free-text predicate.
pub fn exact_or_match<S: AsRef<str>>(terms: &[S]) -> Option<String> {
    let quoted: Vec<String> = terms
        .iter()
        .filter_map(|t| quote_exact(t.as_ref()))
        .collect();
    (!quoted.is_empty()).then(|| quoted.join(" OR "))
}

/// Per-column weights for FTS5's `bm25()`, in the column order of the FTS table. FTS5 ranks a
/// lower score as a better match; callers that want the best match first sort ascending.
#[derive(Debug, Clone, PartialEq)]
pub struct Bm25Weights {
    weights: Vec<f64>,
}

impl Bm25Weights {
    /// Weights for `weights.len()` columns, one per column in table order. Each must be finite
    /// and non-negative; there must be between 1 and [`MAX_BM25_COLUMNS`].
    pub fn new(weights: &[f64]) -> Result<Self, FtsError> {
        if weights.is_empty() || weights.len() > MAX_BM25_COLUMNS {
            return Err(FtsError::BadWeights(format!(
                "1 to {MAX_BM25_COLUMNS} weights are needed, got {}",
                weights.len()
            )));
        }
        if weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
            return Err(FtsError::BadWeights(
                "weights must be finite and non-negative".into(),
            ));
        }
        Ok(Self {
            weights: weights.to_vec(),
        })
    }

    /// Number of columns these weights cover.
    pub fn len(&self) -> usize {
        self.weights.len()
    }

    /// Always false: [`Bm25Weights::new`] refuses an empty list.
    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    /// The `bm25(<table>, w1, ..., wN)` expression. Weights are written with `f64`'s debug form,
    /// which is always a SQL numeric literal.
    pub fn score_sql(&self, table: &str) -> Result<String, FtsError> {
        check_identifier(table)?;
        let weights: Vec<String> = self.weights.iter().map(|w| format!("{w:?}")).collect();
        Ok(format!("bm25({table}, {})", weights.join(", ")))
    }
}

/// Validate a row limit for a bounded ranked select.
pub fn check_limit(limit: usize) -> Result<(), FtsError> {
    if (1..=MAX_RANK_LIMIT).contains(&limit) {
        Ok(())
    } else {
        Err(FtsError::BadLimit(limit))
    }
}

/// An external-content FTS5 table: the index holds no copy of the text, and reads it from
/// `content_table` by `content_rowid`. `columns` must name columns of `content_table`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fts5Table<'a> {
    /// The FTS5 table name.
    pub name: &'a str,
    /// Indexed columns, in order; these are the columns `bm25()` weights apply to.
    pub columns: &'a [&'a str],
    /// Tokenizer specification, for example `unicode61 remove_diacritics 2`. `None` is FTS5's
    /// default. Allowed characters: ASCII letters, digits, `_`, space, `.` and `-`.
    pub tokenizer: Option<&'a str>,
    /// External content table. `None` makes an FTS5 table that stores its own copy of the text.
    pub content_table: Option<&'a str>,
    /// Rowid column of `content_table` that the index rows point at. Defaults to `rowid`.
    pub content_rowid: Option<&'a str>,
}

impl Fts5Table<'_> {
    fn validate(&self) -> Result<(), FtsError> {
        check_identifier(self.name)?;
        if self.columns.is_empty() {
            return Err(FtsError::NoColumns);
        }
        for column in self.columns {
            check_identifier(column)?;
        }
        if let Some(content) = self.content_table {
            check_identifier(content)?;
        }
        if let Some(rowid) = self.content_rowid {
            check_identifier(rowid)?;
        }
        if let Some(spec) = self.tokenizer {
            let ok = spec
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ' ' | '.' | '-'));
            if !ok || spec.trim().is_empty() {
                return Err(FtsError::BadTokenizer(spec.to_string()));
            }
        }
        Ok(())
    }

    /// `CREATE VIRTUAL TABLE IF NOT EXISTS` for this table, with its options.
    pub fn create_sql(&self) -> Result<String, FtsError> {
        self.validate()?;
        let mut options = Vec::new();
        if let Some(spec) = self.tokenizer {
            options.push(format!("tokenize = '{spec}'"));
        }
        if let Some(content) = self.content_table {
            options.push(format!("content = '{content}'"));
            options.push(format!(
                "content_rowid = '{}'",
                self.content_rowid.unwrap_or("rowid")
            ));
        }
        let columns = self.columns.join(", ");
        let tail = if options.is_empty() {
            String::new()
        } else {
            format!(", {}", options.join(", "))
        };
        Ok(format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS {} USING fts5({columns}{tail})",
            self.name
        ))
    }

    /// Statement that rebuilds the index from the content table (`'rebuild'`). Run it after a
    /// bulk load, or to repair an index that no longer matches its content.
    pub fn rebuild_sql(&self) -> Result<String, FtsError> {
        self.validate()?;
        Ok(format!("INSERT INTO {0}({0}) VALUES('rebuild')", self.name))
    }

    /// Statements that drop the three sync triggers and the table, in that order. Safe to run
    /// when some do not exist.
    pub fn drop_sql(&self) -> Result<Vec<String>, FtsError> {
        self.validate()?;
        let mut out: Vec<String> = ["ai", "ad", "au"]
            .iter()
            .map(|suffix| format!("DROP TRIGGER IF EXISTS {}_{suffix}", self.name))
            .collect();
        out.push(format!("DROP TABLE IF EXISTS {}", self.name));
        Ok(out)
    }

    /// The three triggers that keep an external-content table in step with its content table:
    /// one on insert, one on delete and one on update, following FTS5's documented pattern. The
    /// content table must have every indexed column under the same name. Each statement is
    /// one `CREATE TRIGGER IF NOT EXISTS` and is run as-is.
    pub fn sync_triggers_sql(&self) -> Result<Vec<String>, FtsError> {
        self.validate()?;
        let Some(source) = self.content_table else {
            return Err(FtsError::NotExternalContent);
        };
        let rowid = self.content_rowid.unwrap_or("rowid");
        let name = self.name;
        let cols = self.columns.join(", ");
        let new_vals = self
            .columns
            .iter()
            .map(|c| format!("new.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let old_vals = self
            .columns
            .iter()
            .map(|c| format!("old.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let insert = format!(
            "CREATE TRIGGER IF NOT EXISTS {name}_ai AFTER INSERT ON {source} BEGIN \
             INSERT INTO {name}(rowid, {cols}) VALUES (new.{rowid}, {new_vals}); END"
        );
        let delete = format!(
            "CREATE TRIGGER IF NOT EXISTS {name}_ad AFTER DELETE ON {source} BEGIN \
             INSERT INTO {name}({name}, rowid, {cols}) VALUES ('delete', old.{rowid}, {old_vals}); END"
        );
        let update = format!(
            "CREATE TRIGGER IF NOT EXISTS {name}_au AFTER UPDATE ON {source} BEGIN \
             INSERT INTO {name}({name}, rowid, {cols}) VALUES ('delete', old.{rowid}, {old_vals}); \
             INSERT INTO {name}(rowid, {cols}) VALUES (new.{rowid}, {new_vals}); END"
        );
        Ok(vec![insert, delete, update])
    }
}
