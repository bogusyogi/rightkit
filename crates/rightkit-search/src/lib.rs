//! `rightkit-search` - product-neutral search query language.
//!
//! - [`lexer`] / [`ast`]: tokenizer and parser with a caller-supplied operator vocabulary ([`Syntax`]).
//! - [`terms::Query`]: parsed query as data (operators, text, phrases, OR groups, negations) and FTS5 expressions.
//! - [`values`]: date / relative / size operator values (no chrono).
//! - [`sql`]: [`Schema`] maps operators to columns and compiles a [`Query`] to a parameterised WHERE body.
//! - [`snippet`]: FTS5 snippet helper, highlight spans, plain-text excerpts.
//! - [`fold`] / [`matcher`]: case-folded substring matching over explicit haystacks (no index).
//! - [`page`]: keyset paging over compiled predicates, with opaque cursors.
//! - [`scan`]: budgeted, resumable scans over sources that cannot be filtered in SQL.
//! - `filename` (feature `filename-index`): Cockpit-derived in-memory filename index.

pub mod ast;
pub mod fold;
pub mod lexer;
pub mod matcher;
pub mod page;
pub mod scan;
pub mod snippet;
pub mod sql;
pub mod terms;
pub mod values;

#[cfg(feature = "filename-index")]
pub mod filename;

pub use ast::{Leaf, Node, ParseError, Syntax};
pub use fold::{check_fold_version, fold, fold_with, FoldOptions, RebuildNeeded, FOLD_VERSION};
pub use matcher::{MatchError, Snippet, TextMatcher};
pub use page::{Cursor, CursorError, Keyset, PagedSql};
pub use scan::{read_text_prefix, scan_page, ScanBudget, ScanPage};
pub use sql::{
    Binding, ChildScope, CompileError, Compiled, OpSource, Param, Schema, ShapeSql, ValueKind,
};
pub use terms::{OpTerm, Query};
