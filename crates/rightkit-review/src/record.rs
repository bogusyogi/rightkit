//! The versioned verdict record. Readers check `schema` and `version` before
//! any typed parse, so a newer or foreign record is refused rather than
//! partly understood.

use serde::{Deserialize, Serialize};

use crate::judge::JudgeRecord;
use crate::repair::StopReason;
use crate::verdict::{Disposition, Finding};
use crate::ReviewError;

pub const VERDICT_RECORD_SCHEMA: &str = "rightkit-review.verdict";
/// Schema version `major.minor.patch`. This build reads major 1, minor 0 or older.
pub const VERDICT_RECORD_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    /// Zero for the original candidate, then one per repair.
    pub attempt: u32,
    pub findings: Vec<Finding>,
    pub disposition: Disposition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerdictRecord {
    pub schema: String,
    pub version: String,
    pub rule_set_id: String,
    pub rule_set_version: u32,
    pub artifact_id: String,
    pub attempts: Vec<Attempt>,
    /// Equal to the disposition of the last attempt.
    pub disposition: Disposition,
    pub stop: StopReason,
    /// Advisory only. Never read when computing `disposition`.
    pub advisory: Vec<JudgeRecord>,
}

impl VerdictRecord {
    pub fn to_json(&self) -> Result<String, ReviewError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parses a record, refusing any other schema or a version this build cannot read.
    pub fn from_json(text: &str) -> Result<Self, ReviewError> {
        let raw: serde_json::Value = serde_json::from_str(text)?;
        let schema = raw
            .get("schema")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let version = raw
            .get("version")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let refuse = || ReviewError::UnsupportedRecord {
            schema: schema.to_string(),
            version: version.to_string(),
        };
        if schema != VERDICT_RECORD_SCHEMA {
            return Err(refuse());
        }
        let parts: Vec<u64> = version
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect();
        if parts.len() != 3 || parts[0] != 1 || parts[1] > 0 {
            return Err(refuse());
        }
        Ok(serde_json::from_value(raw)?)
    }
}
