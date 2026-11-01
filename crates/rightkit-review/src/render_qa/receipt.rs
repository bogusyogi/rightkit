//! QA receipts: the report and a content-addressed receipt written into the
//! project, and their verification.
//!
//! Layout inside the project directory:
//! `qa/<render-blake3>.report.json` and `qa/<render-blake3>.receipt.json`.
//! The receipt binds the render hash, the report file hash and itself (its own
//! hash is computed with `receipt_blake3` empty), so a report edited after the
//! fact, or one belonging to a different render, does not verify.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::render_qa::error::QaError;
use crate::render_qa::fsutil::atomic_write;
use crate::render_qa::probe::QaTools;
use crate::render_qa::report::{OverallStatus, QaReport, CHECK_VERSION};
use crate::render_qa::run::run_qa;
use crate::render_qa::spec::{QaExpect, QaSpec};
use rightkit_fs::digest::{blake3_bytes, blake3_file};

pub const RECEIPT_KIND: &str = "cutright.qa.receipt";
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaReceipt {
    pub schema_version: u32,
    pub kind: String,
    pub render_blake3: String,
    pub report_file: String,
    pub report_blake3: String,
    pub status: OverallStatus,
    pub preset: String,
    pub captions_blake3: Option<String>,
    pub ffmpeg_identity: String,
    pub created_at: String,
    /// Check version of the report. Absent (not serialised) in receipts written before it
    /// existed, so their hash still verifies and they read as version 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_version: Option<u32>,
    /// The out-of-process critic's attempts on this render, derived from the report's
    /// `critic.*` checks. Empty (and not serialised) for reports without critic checks, so
    /// receipts written before the critic existed still verify.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critic_attempts: Vec<CriticAttemptRecord>,
    /// Id of the QA policy (preset) the report was judged under. Absent (not serialised) in
    /// receipts written before policies were recorded: those read as unknown-policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
    /// BLAKE3 of that policy's thresholds (`QaSpec::policy_blake3`). Absent with `policy_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_blake3: Option<String>,
    /// BLAKE3 of this receipt serialised with this field empty.
    pub receipt_blake3: String,
}

/// One critic evaluation as the receipt records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriticAttemptRecord {
    /// 0 for the only evaluation `run_qa` makes; the critic is never repaired on the render.
    pub attempt: u32,
    /// `pass` when no critic check blocks, else `block`.
    pub disposition: String,
    pub verdicts: Vec<CriticVerdictRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriticVerdictRecord {
    pub rule_id: String,
    /// `verified`, `failed`, `unverified`, `inconclusive` or `not_applicable`.
    pub verdict: String,
    pub required: bool,
    pub reason: String,
}

/// The critic attempts a report implies: one attempt holding every `critic.*` check, or none.
pub fn derive_critic_attempts(report: &QaReport) -> Vec<CriticAttemptRecord> {
    let verdicts: Vec<CriticVerdictRecord> = report
        .checks
        .iter()
        .filter_map(|c| {
            let critic = c.evidence.get("critic")?;
            Some(CriticVerdictRecord {
                rule_id: c.id.clone(),
                verdict: critic.get("verdict")?.as_str()?.to_string(),
                required: critic.get("required")?.as_bool()?,
                reason: critic.get("reason")?.as_str()?.to_string(),
            })
        })
        .collect();
    if verdicts.is_empty() {
        return Vec::new();
    }
    let blocked = report
        .checks
        .iter()
        .any(|c| c.id.starts_with("critic.") && c.blocking());
    vec![CriticAttemptRecord {
        attempt: 0,
        disposition: if blocked { "block" } else { "pass" }.into(),
        verdicts,
    }]
}

impl QaReceipt {
    fn compute_hash(&self) -> String {
        let mut c = self.clone();
        c.receipt_blake3.clear();
        blake3_bytes(&serde_json::to_vec(&c).expect("receipt serialises"))
    }
}

pub fn qa_dir(project_dir: &Path) -> PathBuf {
    project_dir.join("qa")
}

pub fn report_path(project_dir: &Path, render_blake3: &str) -> PathBuf {
    qa_dir(project_dir).join(format!("{render_blake3}.report.json"))
}

pub fn receipt_path(project_dir: &Path, render_blake3: &str) -> PathBuf {
    qa_dir(project_dir).join(format!("{render_blake3}.receipt.json"))
}

#[derive(Debug, Clone)]
pub struct QaOutcome {
    pub report: QaReport,
    pub receipt: QaReceipt,
    pub report_path: PathBuf,
    pub receipt_path: PathBuf,
}

/// Runs the QA gate on `render`, writes the report and receipt into the project
/// (a failing report is recorded too, so the failure is on file), and returns them.
pub fn qa_gate(
    project_dir: &Path,
    tools: &QaTools,
    render: &Path,
    spec: &QaSpec,
    expect: &QaExpect,
) -> Result<QaOutcome, QaError> {
    let report = run_qa(tools, render, spec, expect)?;
    record_report(project_dir, report)
}

/// Writes `report` and its receipt into the project, as [`qa_gate`] does after it runs the
/// probes. Used when extra checks (speech.retranscription) were added to the report first.
pub fn record_report(project_dir: &Path, report: QaReport) -> Result<QaOutcome, QaError> {
    let hash = report.render.blake3.clone();
    let mut text = report
        .to_json()
        .map_err(|e| QaError::BadInput(format!("serialising report: {e}")))?;
    text.push('\n');
    let report_path = report_path(project_dir, &hash);
    atomic_write(&report_path, text.as_bytes())?;
    let mut receipt = QaReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        kind: RECEIPT_KIND.into(),
        render_blake3: hash.clone(),
        report_file: format!("{hash}.report.json"),
        report_blake3: blake3_bytes(text.as_bytes()),
        status: report.status,
        preset: report.preset.clone(),
        captions_blake3: report.captions_blake3.clone(),
        ffmpeg_identity: report.ffmpeg_identity.clone(),
        created_at: report.generated_at.clone(),
        check_version: Some(report.check_version),
        critic_attempts: derive_critic_attempts(&report),
        policy_id: Some(report.spec.policy_id()),
        policy_blake3: Some(report.spec.policy_blake3()),
        receipt_blake3: String::new(),
    };
    receipt.receipt_blake3 = receipt.compute_hash();
    let receipt_path = receipt_path(project_dir, &hash);
    let mut rtext = serde_json::to_string_pretty(&receipt)
        .map_err(|e| QaError::BadInput(format!("serialising receipt: {e}")))?;
    rtext.push('\n');
    atomic_write(&receipt_path, rtext.as_bytes())?;
    Ok(QaOutcome {
        report,
        receipt,
        report_path,
        receipt_path,
    })
}

/// A report and receipt that verified against each other and a render hash.
#[derive(Debug, Clone)]
pub struct VerifiedQa {
    pub report: QaReport,
    pub receipt: QaReceipt,
    pub report_blake3: String,
    /// Digest of the exact verified file bytes, distinct from the receipt's content hash.
    pub receipt_file_blake3: String,
}

/// Verifies a report/receipt pair for `render_blake3` (does not require a pass).
pub fn verify_receipt_files(
    report_path: &Path,
    receipt_path: &Path,
    render_blake3: &str,
) -> Result<VerifiedQa, QaError> {
    let bad = |m: String| QaError::ReceiptInvalid(m);
    let receipt_bytes = std::fs::read(receipt_path)
        .map_err(|e| QaError::io(format!("reading {}", receipt_path.display()), e))?;
    let receipt: QaReceipt = serde_json::from_slice(&receipt_bytes)
        .map_err(|e| bad(format!("receipt unreadable: {e}")))?;
    if receipt.kind != RECEIPT_KIND || receipt.schema_version != RECEIPT_SCHEMA_VERSION {
        return Err(bad("not a QA receipt of a known version".into()));
    }
    if receipt.receipt_blake3 != receipt.compute_hash() {
        return Err(bad("receipt hash does not match its content".into()));
    }
    if receipt.render_blake3 != render_blake3 {
        return Err(bad(format!(
            "receipt is for render blake3:{} not blake3:{render_blake3}",
            receipt.render_blake3
        )));
    }
    let report_hash = blake3_file(report_path)
        .map_err(|e| QaError::io(format!("hashing {}", report_path.display()), e))?;
    if report_hash != receipt.report_blake3 {
        return Err(bad(
            "report was modified after the receipt was written".into()
        ));
    }
    let report_text = std::fs::read_to_string(report_path)
        .map_err(|e| QaError::io(format!("reading {}", report_path.display()), e))?;
    let report =
        QaReport::from_json(&report_text).map_err(|e| bad(format!("report unreadable: {e}")))?;
    if report.render.blake3 != render_blake3 {
        return Err(bad("report describes a different render".into()));
    }
    if report.status != receipt.status {
        return Err(bad("report and receipt disagree on the status".into()));
    }
    if receipt
        .check_version
        .is_some_and(|v| v != report.check_version)
    {
        return Err(bad(
            "report and receipt disagree on the check version".into()
        ));
    }
    // The status must follow from the checks, so a flipped flag alone cannot pass.
    let derived = if report.checks.iter().any(|c| c.blocking()) {
        OverallStatus::Fail
    } else {
        OverallStatus::Pass
    };
    if derived != report.status {
        return Err(bad("report status does not follow from its checks".into()));
    }
    if report.captions_blake3 != receipt.captions_blake3 {
        return Err(bad("report and receipt disagree on the captions".into()));
    }
    if receipt.critic_attempts != derive_critic_attempts(&report) {
        return Err(bad(
            "receipt critic attempts do not match the report's critic checks".into(),
        ));
    }
    // The recorded policy must be the one the report was judged under, so a forged or
    // swapped policy record cannot pass.
    match (&receipt.policy_id, &receipt.policy_blake3) {
        (None, None) => {}
        (Some(id), Some(hash)) => {
            if *id != report.spec.policy_id() || *hash != report.spec.policy_blake3() {
                return Err(bad(
                    "recorded QA policy does not match the thresholds the report was judged under"
                        .into(),
                ));
            }
        }
        _ => return Err(bad("receipt records an incomplete QA policy".into())),
    }
    Ok(VerifiedQa {
        report,
        receipt,
        report_blake3: report_hash,
        receipt_file_blake3: blake3_bytes(&receipt_bytes),
    })
}

/// Looks the render's receipt up in the project and verifies it.
/// [`QaError::QaMissing`] when the project holds no receipt for these exact bytes.
pub fn verify_receipt(project_dir: &Path, render_blake3: &str) -> Result<VerifiedQa, QaError> {
    let receipt = receipt_path(project_dir, render_blake3);
    let report = report_path(project_dir, render_blake3);
    if !receipt.is_file() || !report.is_file() {
        return Err(QaError::QaMissing {
            render_blake3: render_blake3.into(),
        });
    }
    let verified = verify_receipt_files(&report, &receipt, render_blake3)?;
    judged_under_current_policy(&verified.report)?;
    Ok(verified)
}

/// Why a report was not judged under the current policy and check code, if it was not.
/// Used as a hard error by [`verify_receipt`] and as a warning by package verification.
pub(crate) fn policy_drift(report: &QaReport) -> Option<String> {
    if report.check_version != CHECK_VERSION {
        return Some(format!(
            "the report was judged by check version {} and this build runs version {CHECK_VERSION}",
            report.check_version
        ));
    }
    let Ok(current) = QaSpec::preset(&report.preset) else {
        return Some(format!("preset {} has no QA thresholds", report.preset));
    };
    if current != report.spec {
        return Some(format!(
            "the QA thresholds for preset {} changed since the report was made",
            report.preset
        ));
    }
    None
}

/// A pass counts only under the policy and check code that produced it. A preset whose
/// thresholds changed under the same name, or a build with other check code, makes the
/// stored result stale, so the render must be QA'd again. The engine's verified-reuse path
/// relies on this: it reuses a receipt only when this returns `Ok`.
pub(crate) fn judged_under_current_policy(report: &QaReport) -> Result<(), QaError> {
    match policy_drift(report) {
        None => Ok(()),
        Some(why) => Err(QaError::ReceiptInvalid(format!(
            "{why}; run the QA gate on this render again"
        ))),
    }
}
