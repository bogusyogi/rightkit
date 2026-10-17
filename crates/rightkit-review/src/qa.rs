//! Product QA reports. Only error-severity failures block delivery.
//!
//! These types preserve CutRight's snake_case report wire format. They are
//! independent of the content pipeline's hold/unknown semantics in [`crate::Verdict`].

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::ReviewError;

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub status: CheckStatus,
    pub severity: Severity,
    pub message: String,
    pub evidence: Value,
}

impl Check {
    /// Builds a measured pass/fail check, keeping caller-owned copy & evidence.
    pub fn measured(
        id: &str,
        severity: Severity,
        ok: bool,
        ok_message: impl Into<String>,
        fail_message: impl Into<String>,
        evidence: Value,
    ) -> Self {
        Self {
            id: id.to_string(),
            status: if ok {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            },
            severity,
            message: if ok {
                ok_message.into()
            } else {
                fail_message.into()
            },
            evidence,
        }
    }

    pub fn not_applicable(id: &str, reason: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            status: CheckStatus::NotApplicable,
            severity: Severity::Warning,
            message: reason.into(),
            evidence: Value::Null,
        }
    }

    pub fn blocking(&self) -> bool {
        self.severity == Severity::Error && self.status == CheckStatus::Fail
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallStatus {
    Pass,
    Fail,
}

pub fn overall_status(checks: &[Check]) -> OverallStatus {
    if checks.iter().any(Check::blocking) {
        OverallStatus::Fail
    } else {
        OverallStatus::Pass
    }
}

fn legacy_check_version() -> u32 {
    1
}

const QA_POLICY_HASH_DOMAIN: &str = "rightkit-review.qa-policy.v1\n";

/// SHA-256 (lowercase hex) over the canonical JSON of a consumer's QA policy, such as the
/// `spec` of a report. Keys are sorted, so the hash ignores field order. Store it with
/// `check_version` and reuse a stored report only when both match the policy in force now
/// (Orthic-Labs/rightkit#179). Matching a preset name is not enough.
pub fn policy_hash<T: Serialize>(policy: &T) -> Result<String, ReviewError> {
    let value = serde_json::to_value(policy)?;
    let mut canonical = String::from(QA_POLICY_HASH_DOMAIN);
    crate::binding::write_canonical(&value, &mut canonical)?;
    Ok(crate::binding::sha256_hex(canonical.as_bytes()))
}

/// Persistable product report. Policy, render identity & media facts stay
/// consumer-owned; their serde shapes survive adoption without receipt migration.
///
/// `schema_version` versions this envelope; `check_version` versions consumer QA
/// code. Receipt verification must still bind render bytes, captions & policy.
/// Use [`Self::from_json`] at trust boundaries to validate schema & aggregate status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerdictRecord<Spec, Render, Media> {
    pub schema_version: u32,
    pub preset: String,
    pub status: OverallStatus,
    pub render: Render,
    pub media: Media,
    pub captions_blake3: Option<String>,
    pub spec: Spec,
    #[serde(default = "legacy_check_version")]
    pub check_version: u32,
    pub checks: Vec<Check>,
    pub ffmpeg_identity: String,
    pub generated_at: String,
}

impl<S, R, M> VerdictRecord<S, R, M> {
    /// Returns the stored status without verifying policy or check version. Do not reuse a
    /// stored report with this alone; use [`Self::passed_under`].
    pub fn passed(&self) -> bool {
        self.status == OverallStatus::Pass
    }

    pub fn blocking_ids(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| c.blocking())
            .map(|c| c.id.clone())
            .collect()
    }

    pub fn check(&self, id: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.id == id)
    }

    /// Replaces any prior check with this id & recomputes report status.
    pub fn with_check(mut self, check: Check) -> Self {
        self.checks.retain(|c| c.id != check.id);
        self.checks.push(check);
        self.status = overall_status(&self.checks);
        self
    }

    pub fn validate(&self) -> Result<(), ReviewError> {
        if self.schema_version != REPORT_SCHEMA_VERSION {
            return Err(ReviewError::UnsupportedRecord {
                schema: "rightkit-review.qa".to_string(),
                version: self.schema_version.to_string(),
            });
        }
        let mut ids = std::collections::BTreeSet::new();
        if self.check_version == 0
            || self
                .checks
                .iter()
                .any(|c| c.id.trim().is_empty() || !ids.insert(&c.id))
        {
            return Err(ReviewError::InvalidSubject(
                "QA record needs nonempty unique check ids & nonzero check_version".to_string(),
            ));
        }
        if self.status != overall_status(&self.checks) {
            return Err(ReviewError::InvalidSubject(
                "QA status disagrees with checks".to_string(),
            ));
        }
        Ok(())
    }
}

impl<S: Serialize, R, M> VerdictRecord<S, R, M> {
    /// Accepts this report only when it is structurally valid, was produced under
    /// `expected_check_version`, and its stored `spec` hashes to `expected_policy_hash`
    /// (see [`policy_hash`]). A tightened policy or newer check version refuses old reports.
    pub fn verify_current(
        &self,
        expected_policy_hash: &str,
        expected_check_version: u32,
    ) -> Result<(), ReviewError> {
        self.validate()?;
        if self.check_version != expected_check_version {
            return Err(ReviewError::StaleVerdict(format!(
                "check version {} differs from current {}",
                self.check_version, expected_check_version
            )));
        }
        if policy_hash(&self.spec)? != expected_policy_hash {
            return Err(ReviewError::StaleVerdict(
                "the stored policy hash differs from the current policy".to_string(),
            ));
        }
        Ok(())
    }

    /// The documented way to reuse a stored verdict: true only when the stored status passed
    /// and [`Self::verify_current`] accepts the report under the current policy and check version.
    pub fn passed_under(&self, policy_hash: &str, check_version: u32) -> bool {
        self.passed() && self.verify_current(policy_hash, check_version).is_ok()
    }
}

impl<S: Serialize, R: Serialize, M: Serialize> VerdictRecord<S, R, M> {
    pub fn to_json(&self) -> Result<String, ReviewError> {
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)?)
    }
}

impl<S: DeserializeOwned, R: DeserializeOwned, M: DeserializeOwned> VerdictRecord<S, R, M> {
    pub fn from_json(text: &str) -> Result<Self, ReviewError> {
        let record: Self = serde_json::from_str(text)?;
        record.validate()?;
        Ok(record)
    }
}

/// A measured half-open span in milliseconds. Signed times preserve probe offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interval {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// One explicit exemption scoped to one check id, with inclusive containment
/// at both boundaries & configurable slack. Adjacent windows are never joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowedWindow {
    pub check_id: String,
    pub interval: Interval,
    pub slack_ms: u64,
    pub reason: String,
}

/// Removes only intervals wholly contained by one matching allowed window.
/// Reversed/empty spans & declarations without identity or reason are refused.
/// Arithmetic widens to i128 so slack cannot overflow probe timestamps.
pub fn unexpected_intervals(
    check_id: &str,
    found: &[Interval],
    allowed: &[AllowedWindow],
) -> Result<Vec<Interval>, ReviewError> {
    if check_id.trim().is_empty()
        || found.iter().any(|i| i.start_ms >= i.end_ms)
        || allowed.iter().any(|w| {
            w.check_id.trim().is_empty()
                || w.reason.trim().is_empty()
                || w.interval.start_ms >= w.interval.end_ms
        })
    {
        return Err(ReviewError::InvalidSubject(
            "invalid QA interval or allowed window".to_string(),
        ));
    }
    Ok(found
        .iter()
        .copied()
        .filter(|i| {
            !allowed.iter().any(|w| {
                w.check_id == check_id
                    && i.start_ms as i128 >= w.interval.start_ms as i128 - w.slack_ms as i128
                    && i.end_ms as i128 <= w.interval.end_ms as i128 + w.slack_ms as i128
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_hash_is_stable_and_changes_with_any_policy_value() {
        let a = serde_json::json!({"max": 10, "name": "youtube"});
        let reordered = serde_json::json!({"name": "youtube", "max": 10});
        let tighter = serde_json::json!({"max": 9, "name": "youtube"});
        let hash = policy_hash(&a).unwrap();
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, policy_hash(&reordered).unwrap());
        assert_ne!(hash, policy_hash(&tighter).unwrap());
    }

    fn bound_report(policy: &Value, check_version: u32) -> VerdictRecord<Value, Value, Value> {
        VerdictRecord {
            schema_version: REPORT_SCHEMA_VERSION,
            preset: "youtube".into(),
            status: OverallStatus::Pass,
            render: Value::Null,
            media: Value::Null,
            captions_blake3: None,
            spec: policy.clone(),
            check_version,
            checks: Vec::new(),
            ffmpeg_identity: "ffmpeg".into(),
            generated_at: "2026-10-18T00:00:00Z".into(),
        }
    }

    #[test]
    fn stale_policy_hash_is_rejected() {
        let report = bound_report(&serde_json::json!({"max": 10}), 3);
        let tightened = policy_hash(&serde_json::json!({"max": 9})).unwrap();
        assert!(report.verify_current(&tightened, 3).is_err());
        assert!(!report.passed_under(&tightened, 3));
        assert!(report.passed());
    }

    #[test]
    fn stale_check_version_is_rejected() {
        let policy = serde_json::json!({"max": 10});
        let hash = policy_hash(&policy).unwrap();
        let report = bound_report(&policy, 3);
        assert!(report.verify_current(&hash, 4).is_err());
        assert!(!report.passed_under(&hash, 4));
    }

    #[test]
    fn matching_policy_and_check_version_are_accepted() {
        let policy = serde_json::json!({"max": 10});
        let hash = policy_hash(&policy).unwrap();
        let report = bound_report(&policy, 3);
        assert!(report.verify_current(&hash, 3).is_ok());
        assert!(report.passed_under(&hash, 3));
    }

    #[test]
    fn only_error_failures_block() {
        for status in [
            CheckStatus::Pass,
            CheckStatus::Fail,
            CheckStatus::NotApplicable,
        ] {
            for severity in [Severity::Error, Severity::Warning] {
                let check = Check {
                    id: "test".into(),
                    status,
                    severity,
                    message: String::new(),
                    evidence: Value::Null,
                };
                assert_eq!(
                    check.blocking(),
                    status == CheckStatus::Fail && severity == Severity::Error
                );
                assert_eq!(
                    overall_status(std::slice::from_ref(&check)) == OverallStatus::Fail,
                    check.blocking()
                );
            }
        }
    }

    #[test]
    fn windows_are_scoped_whole_span_exemptions_with_slack() {
        let allowed = vec![AllowedWindow {
            check_id: "black".into(),
            interval: Interval {
                start_ms: 1000,
                end_ms: 2000,
            },
            slack_ms: 100,
            reason: "title".into(),
        }];
        let edge = Interval {
            start_ms: 900,
            end_ms: 2100,
        };
        assert!(unexpected_intervals("black", &[edge], &allowed)
            .unwrap()
            .is_empty());
        assert_eq!(
            unexpected_intervals("silence", &[edge], &allowed).unwrap(),
            vec![edge]
        );
        let partial = Interval {
            start_ms: 899,
            ..edge
        };
        assert_eq!(
            unexpected_intervals("black", &[partial], &allowed).unwrap(),
            vec![partial]
        );
        assert!(unexpected_intervals(
            "black",
            &[Interval {
                start_ms: 1,
                end_ms: 1
            }],
            &allowed
        )
        .is_err());
        let huge = AllowedWindow {
            slack_ms: u64::MAX,
            ..allowed[0].clone()
        };
        assert!(unexpected_intervals(
            "black",
            &[Interval {
                start_ms: i64::MIN,
                end_ms: i64::MAX
            }],
            &[huge]
        )
        .unwrap()
        .is_empty());
    }
}
