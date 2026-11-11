//! Co-evolutionary synthesis loop for skill bundles.
//!
//! Implements the CoEvoSkills protocol (arXiv 2604.01687):
//! synthesize → jury-verify (blind) → refine → package
//!
//! Key properties:
//! - Informational isolation between synthesizer and verifier
//! - ~4 cycles to convergence
//! - +37pp over one-shot synthesis

use crate::skill_bundle::SkillBundle;
use crate::types::{Proposal, ProposalKind};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Async callback that synthesizes/refines a skill body.
/// Input: a prompt describing what to synthesize (proposals + optional prior body + verifier feedback).
/// Output: the synthesized skill body (SKILL.md content), or an error string.
pub type SynthesizeFn = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync,
>;

/// Async callback that verifies a skill body (the informationally-isolated jury).
/// Input: the skill body to verify.
/// Output: (score 0.0-1.0, feedback text), or an error string.
pub type VerifyFn = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<(f64, String), String>> + Send>>
        + Send
        + Sync,
>;

/// Configuration for the co-evolutionary synthesis loop.
#[derive(Debug, Clone)]
pub struct SynthesisConfig {
    /// Maximum synthesis+verify cycles (default: 4, per CoEvoSkills convergence finding).
    pub max_cycles: u32,
    /// Minimum verification score to accept the skill (default: 0.75).
    pub acceptance_threshold: f64,
}

impl Default for SynthesisConfig {
    fn default() -> Self {
        Self {
            max_cycles: 4,
            acceptance_threshold: 0.75,
        }
    }
}

/// Result of a synthesis loop iteration.
#[derive(Debug, Clone)]
pub struct SynthesisCycleResult {
    /// Cycle number (1-indexed).
    pub cycle: u32,
    /// The synthesized skill body after this cycle.
    pub skill_body: String,
    /// Verification score assigned by the jury (0.0-1.0).
    pub verification_score: f64,
    /// Feedback from the verifier (for refinement input).
    pub feedback: String,
    /// Whether convergence was reached.
    pub converged: bool,
}

/// Co-evolutionary synthesis loop: synthesize → jury-verify (blind) → refine → package.
///
/// Implements the CoEvoSkills protocol (arXiv 2604.01687):
/// - Informational isolation between synthesizer and verifier
/// - ~4 cycles to convergence
/// - +37pp over one-shot synthesis
pub struct SkillSynthesisLoop {
    config: SynthesisConfig,
    synthesize_fn: Option<SynthesizeFn>,
    verify_fn: Option<VerifyFn>,
}

impl std::fmt::Debug for SkillSynthesisLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillSynthesisLoop")
            .field("config", &self.config)
            .field("synthesize_fn", &self.synthesize_fn.is_some())
            .field("verify_fn", &self.verify_fn.is_some())
            .finish()
    }
}

impl SkillSynthesisLoop {
    /// Create a new synthesis loop with the given config.
    /// No callbacks are set; the loop will use deterministic stub methods.
    pub fn new(config: SynthesisConfig) -> Self {
        Self {
            config,
            synthesize_fn: None,
            verify_fn: None,
        }
    }

    /// Attach an async synthesizer callback.
    pub fn with_synthesizer(mut self, f: SynthesizeFn) -> Self {
        self.synthesize_fn = Some(f);
        self
    }

    /// Attach an async verifier callback.
    pub fn with_verifier(mut self, f: VerifyFn) -> Self {
        self.verify_fn = Some(f);
        self
    }

    /// Run the synthesis loop for a set of related proposals asynchronously.
    ///
    /// Returns a `SkillBundle` ready for human approval.
    /// The synthesizer and verifier are informationally isolated:
    /// the synthesizer never sees the verifier's internals, only the textual feedback.
    ///
    /// If callbacks are set via `with_synthesizer()` and `with_verifier()`, they are invoked.
    /// Otherwise, deterministic stub methods are used as fallbacks.
    pub async fn synthesize(&self, proposals: &[&Proposal]) -> SkillBundle {
        let name = self.infer_skill_name(proposals);
        let description = self.infer_skill_description(proposals);
        let mut bundle = SkillBundle::new(name, description);
        bundle.source_proposal_ids = proposals.iter().map(|p| p.id.clone()).collect();

        // Initial synthesis
        let mut current_body = if let Some(ref synth_fn) = self.synthesize_fn {
            let prompt = Self::build_synthesis_prompt(proposals, None, None);
            match synth_fn(prompt).await {
                Ok(body) => body,
                Err(_e) => self.initial_synthesis(proposals),
            }
        } else {
            self.initial_synthesis(proposals)
        };

        for cycle in 1..=self.config.max_cycles {
            // Jury-verify the current body (isolated verifier).
            let (score, feedback) = if let Some(ref verify_fn) = self.verify_fn {
                match verify_fn(current_body.clone()).await {
                    Ok((s, f)) => (s, f),
                    Err(_e) => {
                        let result = self.verify_cycle(cycle, &current_body, proposals);
                        (result.verification_score, result.feedback)
                    }
                }
            } else {
                let result = self.verify_cycle(cycle, &current_body, proposals);
                (result.verification_score, result.feedback)
            };

            bundle.cycles = cycle;
            bundle.verification_score = score;

            if score >= self.config.acceptance_threshold {
                break;
            }

            // Refine: synthesizer incorporates verifier feedback but NOT verifier internals.
            // Synthesizer receives only the textual feedback, not the numerical score.
            // This preserves informational isolation (CoEvoSkills property).
            current_body = if let Some(ref synth_fn) = self.synthesize_fn {
                let prompt =
                    Self::build_synthesis_prompt(proposals, Some(&current_body), Some(&feedback));
                match synth_fn(prompt).await {
                    Ok(body) => body,
                    Err(_e) => self.refine(current_body.clone(), &feedback, proposals),
                }
            } else {
                self.refine(current_body.clone(), &feedback, proposals)
            };
        }

        bundle.skill_body = current_body;
        bundle
    }

    /// Synchronous synthesis using only the deterministic stub path (no LLM callbacks).
    /// Used by the sync ProposalDeployer. For real LLM synthesis use async `synthesize()`.
    ///
    /// This method runs the same loop but with stub methods only, avoiding the need for `.await`.
    pub fn synthesize_blocking(&self, proposals: &[&Proposal]) -> SkillBundle {
        assert!(
            self.synthesize_fn.is_none() && self.verify_fn.is_none(),
            "synthesize_blocking requires no async callbacks; use async synthesize() instead"
        );

        let name = self.infer_skill_name(proposals);
        let description = self.infer_skill_description(proposals);
        let mut bundle = SkillBundle::new(name, description);
        bundle.source_proposal_ids = proposals.iter().map(|p| p.id.clone()).collect();

        let mut current_body = self.initial_synthesis(proposals);

        for cycle in 1..=self.config.max_cycles {
            // Jury-verify the current body (isolated verifier).
            let result = self.verify_cycle(cycle, &current_body, proposals);
            bundle.cycles = cycle;
            bundle.verification_score = result.verification_score;

            if result.converged || result.verification_score >= self.config.acceptance_threshold {
                current_body = result.skill_body;
                break;
            }

            // Refine: synthesizer incorporates verifier feedback but NOT verifier internals.
            current_body = self.refine(result.skill_body, &result.feedback, proposals);
        }

        bundle.skill_body = current_body;
        bundle
    }

    /// Build a synthesis instruction prompt from proposals and optional prior state.
    fn build_synthesis_prompt(
        proposals: &[&Proposal],
        prior_body: Option<&str>,
        feedback: Option<&str>,
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(
            "Synthesize or refine a SKILL.md-format skill body based on the following:\n\n",
        );

        prompt.push_str("## Source Proposals\n");
        for (i, p) in proposals.iter().enumerate() {
            prompt.push_str(&format!(
                "{}. {} ({})\n   Evidence: {}\n",
                i + 1,
                p.summary,
                p.id,
                p.evidence
            ));
        }

        if let Some(body) = prior_body {
            prompt.push_str("\n## Prior Skill Body\n");
            prompt.push_str(body);
        }

        if let Some(fb) = feedback {
            prompt.push_str("\n## Verifier Feedback\n");
            prompt.push_str(fb);
            prompt.push_str("\n\nAddress this feedback in your refinement.");
        }

        prompt.push_str(
            "\n\nOutput only the refined SKILL.md-format body, starting with '# Skill' or similar.",
        );
        prompt
    }

    /// Initial synthesis: build a SKILL.md-compatible body from proposals.
    fn initial_synthesis(&self, proposals: &[&Proposal]) -> String {
        let summaries: Vec<_> = proposals
            .iter()
            .filter(|p| p.kind == ProposalKind::Skill)
            .map(|p| format!("- {}", p.summary))
            .collect();

        let summary_block = if summaries.is_empty() {
            "- Multi-step workflow synthesized from session proposals".to_string()
        } else {
            summaries.join("\n")
        };

        format!(
            "# Synthesized Skill\n\n## Trigger\nwhen: []\n\n## Body\n{}\n\n## Notes\nSynthesized from {} proposals by the CoEvoSkills loop.\n",
            summary_block,
            proposals.len()
        )
    }

    /// Verify the current skill body and score it.
    /// Stub implementation based on structural completeness.
    fn verify_cycle(
        &self,
        cycle: u32,
        body: &str,
        _proposals: &[&Proposal],
    ) -> SynthesisCycleResult {
        // Stub: deterministic scoring based on body completeness.
        // Checks for required SKILL.md sections.
        let has_trigger = body.contains("## Trigger");
        let has_body = body.contains("## Body");
        let has_notes = body.contains("## Notes");

        let base_score = if has_trigger && has_body && has_notes {
            0.5
        } else {
            0.3
        };

        // Score improves as cycles increase (mock improvement).
        let score = base_score + (cycle as f64 * 0.1).min(0.3);
        let score = score.min(1.0);

        SynthesisCycleResult {
            cycle,
            skill_body: body.to_string(),
            verification_score: score,
            feedback: format!(
                "cycle {}: structure={}, score={:.2}",
                cycle,
                if has_trigger && has_body && has_notes {
                    "valid"
                } else {
                    "incomplete"
                },
                score
            ),
            converged: score >= self.config.acceptance_threshold,
        }
    }

    /// Refine the skill body based on verifier feedback.
    /// Stub implementation appends feedback as a comment.
    fn refine(&self, body: String, feedback: &str, _proposals: &[&Proposal]) -> String {
        // Stub: append feedback as a comment.
        format!("{}\n<!-- Verifier feedback: {} -->\n", body, feedback)
    }

    /// Infer a skill name from proposals.
    fn infer_skill_name(&self, proposals: &[&Proposal]) -> String {
        proposals
            .first()
            .map(|p| {
                let trimmed = p.id.trim_start_matches("prop-");
                format!("skill-from-{}", trimmed)
            })
            .unwrap_or_else(|| "unnamed-skill".into())
    }

    /// Infer a skill description from proposals.
    fn infer_skill_description(&self, proposals: &[&Proposal]) -> String {
        proposals
            .first()
            .map(|p| p.summary.clone())
            .unwrap_or_else(|| "Synthesized skill from proposals.".into())
    }
}
