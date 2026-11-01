//! Deployment of promoted proposals to a target sink.

use crate::error::LearningError;
use crate::skill_bundle::SkillBundle;
use crate::skill_synthesis::SkillSynthesisLoop;
use crate::types::{Proposal, ProposalKind, ProposalStatus};

/// A target that promoted proposals are applied to.
pub trait DeploymentSink {
    /// Deploy a memory item.
    fn deploy_memory(&mut self, summary: &str);
    /// Deploy a harness rule.
    fn deploy_harness_rule(&mut self, rule: &str);
    /// Deploy a workflow.
    fn deploy_workflow(&mut self, wf: &str);
}

/// Applies promoted proposals to a [`DeploymentSink`].
#[derive(Debug, Default, Clone)]
pub struct ProposalDeployer;

impl ProposalDeployer {
    /// Create a new deployer.
    pub fn new() -> Self {
        Self
    }

    /// Deploy a promoted proposal to the given sink.
    ///
    /// `SkillTweak` proposals are routed to the harness-rule sink, since a skill
    /// tweak is applied as a harness-level adjustment. Returns
    /// [`LearningError::NotPromoted`] if the proposal is not `Promoted`.
    ///
    /// Skill proposals require `deploy_with_synthesis()` so production callers do not
    /// accidentally use the deterministic test/evaluation synthesis path.
    pub fn deploy(
        &self,
        proposal: &Proposal,
        sink: &mut dyn DeploymentSink,
    ) -> Result<Option<SkillBundle>, LearningError> {
        if proposal.status != ProposalStatus::Promoted {
            return Err(LearningError::NotPromoted(proposal.id.clone()));
        }
        match proposal.kind {
            ProposalKind::Memory => {
                sink.deploy_memory(&proposal.summary);
                Ok(None)
            }
            ProposalKind::HarnessRule | ProposalKind::SkillTweak => {
                sink.deploy_harness_rule(&proposal.summary);
                Ok(None)
            }
            ProposalKind::Workflow => {
                sink.deploy_workflow(&proposal.summary);
                Ok(None)
            }
            ProposalKind::Skill => Err(LearningError::SkillRequiresAsyncSynthesis(
                proposal.id.clone(),
            )),
        }
    }

    /// Deploy a promoted proposal with an explicit async skill synthesis loop.
    ///
    /// `ProposalKind::Skill` is packaged into a `SkillBundle` for operator review.
    /// The bundle is returned but never auto-deployed to the sink. Other proposal
    /// kinds keep the normal synchronous deployment behavior.
    pub async fn deploy_with_synthesis(
        &self,
        proposal: &Proposal,
        sink: &mut dyn DeploymentSink,
        synthesis_loop: &SkillSynthesisLoop,
    ) -> Result<Option<SkillBundle>, LearningError> {
        if proposal.kind != ProposalKind::Skill {
            return self.deploy(proposal, sink);
        }
        if proposal.status != ProposalStatus::Promoted {
            return Err(LearningError::NotPromoted(proposal.id.clone()));
        }

        Ok(Some(synthesis_loop.synthesize(&[proposal]).await))
    }
}

/// A [`DeploymentSink`] that records what was deployed, for testing.
#[derive(Debug, Default, Clone)]
pub struct RecordingSink {
    /// Memory summaries deployed.
    pub memories: Vec<String>,
    /// Harness rules deployed.
    pub harness_rules: Vec<String>,
    /// Workflows deployed.
    pub workflows: Vec<String>,
}

impl RecordingSink {
    /// Create an empty recording sink.
    pub fn new() -> Self {
        Self::default()
    }
}

impl DeploymentSink for RecordingSink {
    fn deploy_memory(&mut self, summary: &str) {
        self.memories.push(summary.to_string());
    }
    fn deploy_harness_rule(&mut self, rule: &str) {
        self.harness_rules.push(rule.to_string());
    }
    fn deploy_workflow(&mut self, wf: &str) {
        self.workflows.push(wf.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_synthesis::SynthesisConfig;

    fn proposal(kind: ProposalKind, status: ProposalStatus) -> Proposal {
        Proposal {
            id: "p".to_string(),
            kind,
            summary: "do the thing".to_string(),
            evidence: "e".to_string(),
            score: 0.9,
            status,
            created_at: String::new(),
            age_sessions: 0,
            rejection_reason: None,
        }
    }

    #[test]
    fn deploy_promoted_memory() {
        let deployer = ProposalDeployer::new();
        let mut sink = RecordingSink::new();
        let p = proposal(ProposalKind::Memory, ProposalStatus::Promoted);
        let bundle = deployer.deploy(&p, &mut sink).unwrap();
        assert!(bundle.is_none());
        assert_eq!(sink.memories, vec!["do the thing".to_string()]);
        assert!(sink.harness_rules.is_empty());
        assert!(sink.workflows.is_empty());
    }

    #[test]
    fn deploy_promoted_harness() {
        let deployer = ProposalDeployer::new();
        let mut sink = RecordingSink::new();
        let p = proposal(ProposalKind::HarnessRule, ProposalStatus::Promoted);
        let bundle = deployer.deploy(&p, &mut sink).unwrap();
        assert!(bundle.is_none());
        assert_eq!(sink.harness_rules, vec!["do the thing".to_string()]);
    }

    #[test]
    fn deploy_routes_by_kind() {
        let deployer = ProposalDeployer::new();
        let mut sink = RecordingSink::new();
        deployer
            .deploy(
                &proposal(ProposalKind::Memory, ProposalStatus::Promoted),
                &mut sink,
            )
            .unwrap();
        deployer
            .deploy(
                &proposal(ProposalKind::HarnessRule, ProposalStatus::Promoted),
                &mut sink,
            )
            .unwrap();
        deployer
            .deploy(
                &proposal(ProposalKind::Workflow, ProposalStatus::Promoted),
                &mut sink,
            )
            .unwrap();
        assert_eq!(sink.memories.len(), 1);
        assert_eq!(sink.harness_rules.len(), 1);
        assert_eq!(sink.workflows.len(), 1);
    }

    #[test]
    fn deploy_non_promoted_errors() {
        let deployer = ProposalDeployer::new();
        let mut sink = RecordingSink::new();
        let p = proposal(ProposalKind::Memory, ProposalStatus::Evaluated);
        let err = deployer.deploy(&p, &mut sink).unwrap_err();
        assert_eq!(err, LearningError::NotPromoted("p".to_string()));
        assert!(sink.memories.is_empty());
    }

    #[test]
    fn deploy_skill_requires_async_synthesis() {
        let deployer = ProposalDeployer::new();
        let mut sink = RecordingSink::new();
        let p = proposal(ProposalKind::Skill, ProposalStatus::Promoted);
        let err = deployer.deploy(&p, &mut sink).unwrap_err();
        assert_eq!(err, LearningError::SkillRequiresAsyncSynthesis("p".into()));
        assert!(sink.memories.is_empty());
        assert!(sink.harness_rules.is_empty());
        assert!(sink.workflows.is_empty());
    }

    #[tokio::test]
    async fn deploy_with_synthesis_uses_async_skill_loop() {
        let deployer = ProposalDeployer::new();
        let mut sink = RecordingSink::new();
        let p = proposal(ProposalKind::Skill, ProposalStatus::Promoted);
        let synthesize_fn = std::sync::Arc::new(|_prompt: String| {
            Box::pin(async { Ok("# Skill\n\nLive synthesized body.".to_string()) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
                >
        });
        let verify_fn = std::sync::Arc::new(|body: String| {
            Box::pin(async move {
                assert!(body.contains("Live synthesized body"));
                Ok((0.95, "approved".to_string()))
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<(f64, String), String>> + Send>,
                >
        });
        let synthesis_loop = SkillSynthesisLoop::new(SynthesisConfig::default())
            .with_synthesizer(synthesize_fn)
            .with_verifier(verify_fn);

        let bundle = deployer
            .deploy_with_synthesis(&p, &mut sink, &synthesis_loop)
            .await
            .unwrap()
            .unwrap();

        assert!(bundle.skill_body.contains("Live synthesized body"));
        assert_eq!(bundle.source_proposal_ids, vec!["p"]);
        assert!(sink.memories.is_empty());
        assert!(sink.harness_rules.is_empty());
        assert!(sink.workflows.is_empty());
    }
}
