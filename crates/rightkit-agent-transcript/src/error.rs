//! Typed fail-closed transcript omission/degradation errors.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TranscriptError {
    /// The transcript path does not exist (or is not a regular file).
    #[error("transcript missing: {path}")]
    Missing { path: PathBuf },

    /// The transcript exists but cannot be read.
    #[error("transcript inaccessible: {path}: {detail}")]
    Inaccessible { path: PathBuf, detail: String },

    /// The requested/derived host has no adapter.
    #[error("unsupported host: {host}")]
    UnsupportedHost { host: String },

    /// The file parsed to zero complete JSONL rows; no prefix can be bound.
    #[error("transcript contains no complete JSONL row: {path}")]
    NoCompleteRow { path: PathBuf },

    /// A source row is not valid UTF-8 JSON object input. Parsing fails closed
    /// with its exact source location instead of silently dropping evidence.
    #[error(
        "malformed transcript row {row_index} at bytes {byte_start}..{byte_end} in {path}: {detail}"
    )]
    MalformedRow {
        path: PathBuf,
        row_index: u64,
        byte_start: u64,
        byte_end: u64,
        detail: String,
    },

    /// A valid source row has a shape unknown to its selected host adapter.
    #[error("unsupported {host} transcript row {row_index} in {path}: {detail}")]
    UnsupportedRow {
        path: PathBuf,
        host: String,
        row_index: u64,
        detail: String,
    },

    /// A readable source produced no usable events. This is degradation, not
    /// successful empty evidence.
    #[error("{host} transcript produced no events: {path}")]
    NoEvents { path: PathBuf, host: String },
}

pub type Result<T> = std::result::Result<T, TranscriptError>;
