//! The versioned verdict record. Readers check `schema` and `version` before
//! any typed parse, so a newer or foreign record is refused rather than
//! partly understood.

use serde::{Deserialize, Serialize};

use crate::binding::PolicyBinding;
use crate::judge::JudgeRecord;
use crate::repair::StopReason;
use crate::rules::RuleSet;
use crate::verdict::{Disposition, Finding};
use crate::ReviewError;

pub const VERDICT_RECORD_SCHEMA: &str = "rightkit-review.verdict";
/// Schema version `major.minor.patch`. This build reads major 1, minor 1 or older. 1.1.0 added `binding`; a 1.0.0 record has none and is never current.
pub const VERDICT_RECORD_VERSION: &str = "1.1.0";

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
    /// Policy hash and check versions the verdict was produced under (1.1.0). `None` for a
    /// 1.0.0 record, which therefore never verifies as current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<PolicyBinding>,
    pub attempts: Vec<Attempt>,
    /// Equal to the disposition of the last attempt.
    pub disposition: Disposition,
    pub stop: StopReason,
    /// Advisory only. Never read when computing `disposition`.
    pub advisory: Vec<JudgeRecord>,
}

impl VerdictRecord {
    /// Accepts this record only for the rule set in force now: same id and version, same policy
    /// hash, same check versions. A record without a binding, or bound to anything else, is
    /// stale. Callers reuse a stored pass only after this returns `Ok`; matching a preset name
    /// is not enough.
    pub fn verify_current(&self, rules: &RuleSet) -> Result<(), ReviewError> {
        let stale = |reason: &str| Err(ReviewError::StaleVerdict(reason.to_string()));
        let Some(bound) = &self.binding else {
            return stale("the record carries no policy binding");
        };
        if self.rule_set_id != rules.id || self.rule_set_version != rules.version {
            return stale("the rule set id or version differs");
        }
        let now = rules.policy_binding()?;
        if bound.policy_hash != now.policy_hash {
            return stale("the policy hash differs");
        }
        if bound.check_versions != now.check_versions {
            return stale("a check version differs");
        }
        Ok(())
    }

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
        if parts.len() != 3 || parts[0] != 1 || parts[1] > 1 {
            return Err(refuse());
        }
        Ok(serde_json::from_value(raw)?)
    }
}
