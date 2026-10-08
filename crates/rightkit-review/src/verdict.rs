//! Verdicts, findings, and the disposition rule that turns findings into an
//! outcome. Only deterministic findings reach `disposition`; judge scores are
//! recorded beside it and never feed into it.

use serde::{Deserialize, Serialize};

/// The result of one rule over one subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Verdict {
    /// The rule measured the subject and the subject satisfies it.
    Verified,
    /// The rule does not apply to this kind of artifact. Not counted.
    NotApplicable { reason: String },
    /// The subject violates the rule. A defect, so the item blocks.
    Failed { reason: String },
    /// The rule could not measure the subject. Blocks when the rule is required.
    Unverified { reason: String },
    /// The rule ran but its inputs cannot support a verdict. Blocks when required.
    Inconclusive { reason: String },
    /// The rule cannot decide alone and routes the item to human review.
    Held { reason: String },
}

/// One rule's verdict, with the rule identity needed to audit it later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub rule_id: String,
    pub required: bool,
    pub verdict: Verdict,
}

/// What happens to an item. Ordered from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Pass,
    Hold,
    Block,
}

/// Failed findings block. A required finding that is unverified or
/// inconclusive blocks. Any held finding routes to a human. Anything else,
/// including an advisory unverified finding, passes.
pub fn disposition(findings: &[Finding]) -> Disposition {
    let blocks = findings.iter().any(|finding| match &finding.verdict {
        Verdict::Failed { .. } => true,
        Verdict::Unverified { .. } | Verdict::Inconclusive { .. } => finding.required,
        _ => false,
    });
    if blocks {
        return Disposition::Block;
    }
    if findings
        .iter()
        .any(|finding| matches!(finding.verdict, Verdict::Held { .. }))
    {
        return Disposition::Hold;
    }
    Disposition::Pass
}
