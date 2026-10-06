//! `rightkit-search` - product-neutral search query language.
//!
//! - [`lexer`] / [`ast`]: tokenizer and parser with a caller-supplied operator vocabulary ([`Syntax`]).
//! - [`terms::Query`]: parsed query as data (operators, text, phrases, OR groups, negations) and FTS5 expressions.
//! - [`values`]: date / relative / size operator values (no chrono).
//! - [`sql`]: [`Schema`] maps operators to columns and compiles a [`Query`] to a parameterised WHERE body.
//! - [`snippet`]: FTS5 snippet helper, highlight spans, plain-text excerpts.
//! - `filename` (feature `filename-index`): Cockpit-derived in-memory filename index.

pub mod ast;
pub mod lexer;
pub mod snippet;
pub mod sql;
pub mod terms;
pub mod values;

#[cfg(feature = "filename-index")]
pub mod filename;

pub use ast::{Leaf, Node, ParseError, Syntax};
pub use sql::{Binding, CompileError, Compiled, Param, Schema, ValueKind};
pub use terms::{OpTerm, Query};
