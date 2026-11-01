//! Ledger — rebuildable document navigation, projection, & hash-bound resolution.
//!
//! Ledger owns document indexes and source-bound section references. It does not own
//! canonical repository truth (Blueprint) or durable learned memory (Cortex).

pub mod db;
pub(crate) mod diagnostics;
pub mod doc_projection;
pub mod doc_spine;
pub mod document_conversion;
pub(crate) mod erasure;
pub mod identifier;
pub mod index;
pub mod limits;
pub mod link_projection;
pub mod outline;
pub mod policy;
pub(crate) mod provider;
pub mod query;
pub mod query_alias;
pub(crate) mod reconcile;
pub mod resolve;
pub(crate) mod service;
pub(crate) mod skill_documents;
pub mod transcript_parse;
pub mod transcripts;

pub use db::LedgerDb;
