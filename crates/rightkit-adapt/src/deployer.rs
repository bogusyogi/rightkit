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
