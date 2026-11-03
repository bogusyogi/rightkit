//! Deterministic critic checks, run out of process.
//!
//! The critic judges measurements that the parent has already taken (silence and
//! black spans from the probe). It never calls a model. Each required check returns
//! one `crate::Verdict`: `Verified`, `Failed`, `Unverified` (not measured)
//! or `Inconclusive` (measured, but the inputs cannot place the result). A required
//! `Unverified` or `Inconclusive` blocks unattended delivery through
//! `crate::disposition`.
//!
//! [`CriticRunner`] sends a [`CriticInput`] as JSON to the `rightkit-render-critic` binary and
//! reads back `Vec<Finding>`. [`run_critic_loop`] repairs a blocked input at most
//! `RepairCap` times and stops at `StopReason::RepairCapReached`, keeping the last input
//! for inspection.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::{disposition, Disposition, Finding, RepairCap, StopReason, Verdict};
use rightkit_child_process::{run_with_stdin, RunOptions};
use serde::{Deserialize, Serialize};

use crate::render_qa::report::{CheckStatus, QaCheck, Severity};

pub const RULE_SILENCE: &str = "critic.silence_gap";
pub const RULE_BLACK: &str = "critic.black_frame";
pub const RULE_LOUDNESS: &str = "critic.loudness_true_peak";
pub const RULE_FLASH: &str = "critic.flash_rate";
pub const RULE_CONTRAST: &str = "critic.burned_text_contrast";
pub const RULE_CAPTION_FACE: &str = "critic.caption_over_face";
pub const RULE_CAPTION_STATE: &str = "critic.caption_outlives_state";

/// Check id for a critic child that could not be run, timed out, or returned bad output.
/// Always required: an unrun critic leaves its measurements unjudged.
pub const CHECK_RUN: &str = "critic.run";

/// Reason attached to critic checks that are not implemented yet. They are judged
/// `required = false`, so they never block packaging.
pub const NOT_IMPLEMENTED: &str =
    "not implemented in the critic yet; not judged, does not block packaging";

/// Silence and black limits are judged by the critic from the spans the parent measured.
/// Allowed ranges get the same 100 ms slack that `run.rs` gives them.
pub const HOLD_SLACK_MS: i64 = 100;

/// Critic binary for `run_qa`: the `rightkit-render-critic` binary next to the current
/// executable. `None` when it does not exist.
pub fn default_critic_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(format!(
        "rightkit-render-critic{}",
        std::env::consts::EXE_SUFFIX
    ));
    candidate.is_file().then_some(candidate)
}

/// A time range in milliseconds from the start of the render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Span {
    fn len(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    fn inside(&self, outer: &Span) -> bool {
        outer.start_ms <= self.start_ms && self.end_ms <= outer.end_ms
    }
}

/// Measurements and declarations for one critic pass. `None` means "not measured".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticInput {
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub silence: Option<Vec<Span>>,
    #[serde(default)]
    pub black: Option<Vec<Span>>,
    /// Declared deliberate holds for both silence and black. A span wholly inside one passes.
    #[serde(default)]
    pub holds: Vec<Span>,
    /// Holds that apply to silence only (`QaExpect::allowed_silence`).
    #[serde(default)]
    pub silence_holds: Vec<Span>,
    /// Holds that apply to black only (`QaExpect::allowed_black`).
    #[serde(default)]
    pub black_holds: Vec<Span>,
    /// Silence longer than this, outside a declared hold, fails.
    pub max_silence_ms: u64,
    /// Black longer than this, outside a declared hold, fails.
    pub max_black_ms: u64,
}

/// Judge the input. Pure: no process, no media, no model.
pub fn judge(input: &CriticInput) -> Vec<Finding> {
    let silence_holds: Vec<&Span> = input.holds.iter().chain(&input.silence_holds).collect();
    let black_holds: Vec<&Span> = input.holds.iter().chain(&input.black_holds).collect();
    let mut findings = vec![
        gap_check(
            RULE_SILENCE,
            input.silence.as_deref(),
            input,
            &silence_holds,
            input.max_silence_ms,
        ),
        gap_check(
            RULE_BLACK,
            input.black.as_deref(),
            input,
            &black_holds,
            input.max_black_ms,
        ),
    ];
    for rule in [
        RULE_LOUDNESS,
        RULE_FLASH,
        RULE_CONTRAST,
        RULE_CAPTION_FACE,
        RULE_CAPTION_STATE,
    ] {
        findings.push(Finding {
            rule_id: rule.to_string(),
            required: false,
            verdict: Verdict::Unverified {
                reason: format!("{rule}: {NOT_IMPLEMENTED}"),
            },
        });
    }
    findings
}

fn gap_check(
    rule: &str,
    spans: Option<&[Span]>,
    input: &CriticInput,
    holds: &[&Span],
    limit_ms: u64,
) -> Finding {
    let verdict = match spans {
        None => Verdict::Unverified {
            reason: format!("{rule}: not measured"),
        },
        Some(spans) => match input.duration_ms {
            None | Some(0) => Verdict::Inconclusive {
                reason: format!("{rule}: duration unknown or zero, spans cannot be placed"),
            },
            Some(duration) => {
                if spans
                    .iter()
                    .any(|s| s.end_ms > duration || s.start_ms > s.end_ms)
                {
                    Verdict::Inconclusive {
                        reason: format!("{rule}: a span lies outside the measured duration"),
                    }
                } else if let Some(bad) = spans
                    .iter()
                    .find(|s| s.len() > limit_ms && !holds.iter().any(|h| s.inside(h)))
                {
                    Verdict::Failed {
                        reason: format!(
                            "{rule}: {}ms at {}-{}ms exceeds the {limit_ms}ms limit",
                            bad.len(),
                            bad.start_ms,
                            bad.end_ms
                        ),
                    }
                } else {
                    Verdict::Verified
                }
            }
        },
    };
    Finding {
        rule_id: rule.to_string(),
        required: true,
        verdict,
    }
}

/// Maps critic findings into QA checks. `crate::Verdict` has four states but
/// `CheckStatus` has three, so the exact state is kept in `evidence.critic.verdict`.
///
/// - `Verified` passes.
/// - `Failed` fails; blocking only when the finding is required.
/// - `Unverified` and `Inconclusive` fail (blocking) when required. When not required they
///   are not applicable with a warning, so they do not block.
pub fn checks_from_findings(findings: &[Finding]) -> Vec<QaCheck> {
    findings.iter().map(check_from_finding).collect()
}

fn check_from_finding(f: &Finding) -> QaCheck {
    let (kind, reason, verdict_fail) = match &f.verdict {
        Verdict::Verified => ("verified", String::new(), false),
        Verdict::Failed { reason } => ("failed", reason.clone(), true),
        Verdict::Unverified { reason } => ("unverified", reason.clone(), true),
        Verdict::Inconclusive { reason } => ("inconclusive", reason.clone(), true),
        Verdict::NotApplicable { reason } => ("not_applicable", reason.clone(), false),
        Verdict::Held { reason } => ("held", reason.clone(), true),
    };
    let evidence = serde_json::json!({
        "critic": {"verdict": kind, "required": f.required, "reason": reason}
    });
    let (status, severity, message) = if !verdict_fail {
        (
            CheckStatus::Pass,
            Severity::Error,
            format!("{}: verified", f.rule_id),
        )
    } else if f.required {
        let sev = Severity::Error;
        (
            CheckStatus::Fail,
            sev,
            format!("{}: {kind}: {reason}", f.rule_id),
        )
    } else if kind == "failed" {
        (
            CheckStatus::Fail,
            Severity::Warning,
            format!("{}: {reason}", f.rule_id),
        )
    } else {
        (
            CheckStatus::NotApplicable,
            Severity::Warning,
            format!("{}: {reason}", f.rule_id),
        )
    };
    QaCheck {
        id: f.rule_id.clone(),
        status,
        severity,
        message,
        evidence,
    }
}

/// A check for a critic child that could not produce findings. Always blocking.
pub fn critic_unavailable_check(why: &str) -> QaCheck {
    QaCheck {
        id: CHECK_RUN.into(),
        status: CheckStatus::Fail,
        severity: Severity::Error,
        message: format!("critic did not judge the render: {why}"),
        evidence: serde_json::json!({"critic": {"verdict": "unverified", "required": true, "reason": why}}),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CriticError {
    #[error("cannot start the critic at {path}: {source}")]
    Spawn { path: PathBuf, source: io::Error },
    #[error("critic did not finish within {seconds}s")]
    Timeout { seconds: u64 },
    #[error("critic exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
    #[error("critic I/O failed: {0}")]
    Io(#[source] io::Error),
    #[error("critic payload is not the expected JSON: {0}")]
    Protocol(#[source] serde_json::Error),
}

/// Runs the critic binary as a separate process, bounded by `timeout`.
#[derive(Debug, Clone)]
pub struct CriticRunner {
    binary: PathBuf,
    timeout: Duration,
}

impl CriticRunner {
    pub fn new(binary: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            timeout,
        }
    }

    pub fn run(&self, input: &CriticInput) -> Result<Vec<Finding>, CriticError> {
        let payload = serde_json::to_vec(input).map_err(CriticError::Protocol)?;
        let output = run_with_stdin(
            &self.binary,
            std::iter::empty::<&str>(),
            &payload,
            &RunOptions::new(self.timeout),
        )
        .map_err(CriticError::Io)?;
        if output.timed_out {
            return Err(CriticError::Timeout {
                seconds: self.timeout.as_secs(),
            });
        }
        if !output.status.success() {
            return Err(CriticError::Exit {
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr.bytes)
                    .trim()
                    .to_string(),
            });
        }
        if output.stdout.truncated {
            return Err(CriticError::Io(io::Error::other(
                "critic findings exceeded the output capture limit",
            )));
        }
        serde_json::from_slice(&output.stdout.bytes).map_err(CriticError::Protocol)
    }
}

/// One evaluation of the input. `attempt` is 0 for the original input.
#[derive(Debug, Clone, PartialEq)]
pub struct CriticAttempt {
    pub attempt: u32,
    pub findings: Vec<Finding>,
    pub disposition: Disposition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CriticLoop {
    pub stop: StopReason,
    /// The last input evaluated. On a block it is kept for inspection and never published.
    pub final_input: CriticInput,
    pub attempts: Vec<CriticAttempt>,
    pub disposition: Disposition,
}

/// Evaluate out of process, repair while blocked, stop on pass, hold, cap or refusal.
/// The repairer is called at most `cap.max_repairs()` times.
pub fn run_critic_loop(
    runner: &CriticRunner,
    initial: CriticInput,
    cap: RepairCap,
    mut repairer: impl FnMut(&CriticInput, &[Finding], u32) -> Option<CriticInput>,
) -> Result<CriticLoop, CriticError> {
    let mut candidate = initial;
    let mut attempts: Vec<CriticAttempt> = Vec::new();
    let mut repairs_used: u32 = 0;
    let stop = loop {
        let findings = runner.run(&candidate)?;
        let current = disposition(&findings);
        attempts.push(CriticAttempt {
            attempt: repairs_used,
            findings: findings.clone(),
            disposition: current,
        });
        match current {
            Disposition::Pass => break StopReason::Passed,
            Disposition::Hold => break StopReason::Held,
            Disposition::Block => {}
        }
        if repairs_used >= cap.max_repairs() {
            break StopReason::RepairCapReached;
        }
        match repairer(&candidate, &findings, repairs_used + 1) {
            Some(next) => {
                repairs_used += 1;
                candidate = next;
            }
            None => break StopReason::RepairUnavailable,
        }
    };
    let disposition = attempts
        .last()
        .map(|a| a.disposition)
        .unwrap_or(Disposition::Block);
    Ok(CriticLoop {
        stop,
        final_input: candidate,
        attempts,
        disposition,
    })
}
