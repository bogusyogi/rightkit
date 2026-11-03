//! Payload shapes for manual metric import: a platform export that the account owner downloads by hand and
//! hands to the engine as CSV text with an explicit column mapping. Nothing here logs in, fetches a URL or
//! scrapes: the engine only parses the text it is given.
//!
//! `metric_import.preview` reads and writes nothing. `metric_import.commit` stores every value or none.
//! `metric_import.list` reads the imports in scope. A value is an exact non-negative decimal string, never
//! a float.

use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;
use std::collections::BTreeMap;

/// Which CSV columns carry what. `metric_columns` maps a metric name (lowercase letters, digits,
/// underscores or dots) to the header of its column. `observed_at_column`, when set, names a column of UTC
/// instants (`YYYY-MM-DDTHH:MM:SSZ`); when absent every row is observed at the request's `now`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportMapping {
    pub post_id_column: String,
    pub metric_columns: BTreeMap<String, String>,
    #[serde(default)]
    pub observed_at_column: Option<String>,
}

/// `metric_import.preview` input. Writes nothing. `now` is the observation instant when no column gives one.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricImportPreviewInput {
    pub scope: Scope,
    pub csv: String,
    pub mapping: MetricImportMapping,
    pub platform: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `metric_import.commit` input. `import_id` is the caller's idempotency key: the same id with the same
/// content returns the first result, and the same id with different content is `import_mismatch`.
/// `source_note` says where the file came from and is required.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricImportCommitInput {
    pub scope: Scope,
    pub import_id: String,
    pub source_note: String,
    pub csv: String,
    pub mapping: MetricImportMapping,
    pub platform: String,
    /// Pass it on every commit: a replay compares the request, and an absent `now` is the wall clock.
    #[serde(default)]
    pub now: Option<String>,
}

/// `metric_import.list` input: every import in one scope.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricImportListInput {
    pub scope: Scope,
}

/// One stored numeric value of one metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportValue {
    pub metric: String,
    /// An exact non-negative decimal string, as written in the file (trimmed).
    pub value: String,
}

/// One data row. `job_id` and `observed_at` are null when the row has a problem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportRow {
    /// The 1-based data row. The header is not counted.
    pub row: i64,
    pub post_id: String,
    /// The published job whose remote post id is this post id, on the requested platform.
    pub job_id: Option<String>,
    pub observed_at: Option<String>,
    pub values: Vec<MetricImportValue>,
}

/// One problem. `row` 0 names the header or the mapping (for example a column the header lacks).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportProblem {
    pub row: i64,
    pub column: Option<String>,
    pub code: String,
    pub message: String,
}

/// Row and value counts. `observations` counts the numeric values read; commit stores them only when the
/// whole import has no problem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportCounts {
    pub rows: i64,
    pub rows_with_problems: i64,
    pub problems: i64,
    pub observations: i64,
}

/// `metric_import.preview` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportPreviewOutput {
    pub scope: Scope,
    pub platform: String,
    pub rows: Vec<MetricImportRow>,
    pub problems: Vec<MetricImportProblem>,
    pub counts: MetricImportCounts,
}

/// One stored import, as commit and list return it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportRecord {
    pub scope: Scope,
    pub import_id: String,
    pub platform: String,
    pub source_note: String,
    /// Always `manual_export`.
    pub source: String,
    pub created_at: String,
    pub rows_written: i64,
    pub observations_written: i64,
}

/// `metric_import.commit` output. `created` is false when the same import already existed; the stored
/// import is returned and nothing is written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportCommitOutput {
    pub import: MetricImportRecord,
    pub created: bool,
}

/// `metric_import.list` output, ordered by created_at, then import id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricImportListOutput {
    pub scope: Scope,
    pub imports: Vec<MetricImportRecord>,
}
