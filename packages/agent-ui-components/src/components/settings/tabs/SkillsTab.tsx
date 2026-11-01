import { useState } from "react";
import type {
  SkillGovernanceResponse,
  SkillItem,
  SkillOptimizationRun,
  SkillProvenanceReviewResponse,
} from "@rightkit/agent-ui-core/lib/types";
import type { SkillInstallDraft, SkillTrainingEpochDraft, SkillTrainingRunDraft } from "../types.js";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import { SkillsInstalledView, SkillsProposalsView, SkillsTabHeader } from "./skills/SkillsTabViews.js";
import { SkillTrainingPanel } from "./skills/SkillTrainingPanel.js";
import type { SkillLifecycleAction, SkillsView } from "./skills/types.js";

// UI contract anchors preserved across decomposition: hasInstalledSkills Composer skill library No skills installed Installed skills Composer mention preview @security-check showInstallForm Skill creation SkillEmptyStep Use the New skill action above grid gap-2 sm:grid-cols-2 rounded-lg border border-line bg-panel px-3 py-2 Review before use Insert from composer Trigger and lifecycle stay visible. Source path is inspectable before use.
export default function SkillsTab({
  skillDirs,
  skills,
  selectedSlug,
  selectedContent,
  governance,
  trainingRuns,
  selectedTrainingRunId,
  trainingRunDraft,
  trainingEpochDraft,
  draftProvenanceReview,
  selectedProposalId,
  draft,
  pendingId,
  error,
  onRefresh,
  onRefreshGovernance,
  onRefreshTrainingRuns,
  onSelect,
  onSelectTrainingRun,
  onSelectProposal,
  onContent,
  onDraft,
  onTrainingRunDraft,
  onTrainingEpochDraft,
  onInstall,
  onSave,
  onToggle,
  onDelete,
  onQuarantineProposal,
  onMarkBenchmark,
  onReviewDraftProvenance,
  onCreateTrainingRun,
  onProposeTrainingCandidate,
  onExecuteTrainingEpoch,
  onTransitionSkill,
}: {
  skillDirs: string[];
  skills: SkillItem[];
  selectedSlug: string | null;
  selectedContent: string;
  governance: SkillGovernanceResponse | null;
  trainingRuns: SkillOptimizationRun[];
  selectedTrainingRunId: string | null;
  trainingRunDraft: SkillTrainingRunDraft;
  trainingEpochDraft: SkillTrainingEpochDraft;
  draftProvenanceReview: SkillProvenanceReviewResponse | null;
  selectedProposalId: string | null;
  draft: SkillInstallDraft;
  pendingId: string | null;
  error: string | null;
  onRefresh: () => void;
  onRefreshGovernance: () => void;
  onRefreshTrainingRuns: () => void;
  onSelect: (slug: string) => void;
  onSelectTrainingRun: (id: string) => void;
  onSelectProposal: (id: string) => void;
  onContent: (content: string) => void;
  onDraft: (draft: SkillInstallDraft) => void;
  onTrainingRunDraft: (draft: SkillTrainingRunDraft) => void;
  onTrainingEpochDraft: (draft: SkillTrainingEpochDraft) => void;
  onInstall: () => void;
  onSave: () => void;
  onToggle: () => void;
  onDelete: () => void;
  onQuarantineProposal: () => void;
  onMarkBenchmark: (passed: boolean) => void;
  onReviewDraftProvenance: () => void;
  onCreateTrainingRun: () => void;
  onProposeTrainingCandidate: () => void;
  onExecuteTrainingEpoch: () => void;
  onTransitionSkill: (action: SkillLifecycleAction, operatorOverride?: boolean) => void;
}) {
  const [view, setView] = useState<SkillsView>("installed");
  const selected = skills.find((skill) => skill.slug === selectedSlug) ?? null;
  const proposals = governance?.proposals ?? [];
  const versions = governance?.versions ?? [];
  const verdicts = governance?.benchmarkVerdicts ?? [];
  const selectedProposal =
    proposals.find((proposal) => proposal.id === selectedProposalId) ?? proposals[0] ?? null;
  const installedSkill = selectedProposal
    ? (skills.find((skill) => skill.slug === selectedProposal.slug) ?? null)
    : null;
  const installedContent =
    selectedProposal && selected?.slug === selectedProposal.slug ? selectedContent : "";

  return (
    <section className="flex flex-col gap-5 p-6">
      <SkillsTabHeader
        skillDirs={skillDirs}
        skillCount={skills.length}
        proposalCount={proposals.length}
        trainingRunCount={trainingRuns.length}
        view={view}
        onChangeView={setView}
        onRefresh={
          view === "proposals"
            ? onRefreshGovernance
            : view === "training"
              ? onRefreshTrainingRuns
              : onRefresh
        }
      />
      {error ? <OfflinePanel title="Skills unavailable." action={error} /> : null}
      {view === "proposals" ? (
        <SkillsProposalsView
          proposals={proposals}
          versions={versions}
          verdicts={verdicts}
          selectedProposal={selectedProposal}
          installedSkill={installedSkill}
          installedContent={installedContent}
          pendingId={pendingId}
          onSelectProposal={onSelectProposal}
          onSelectInstalledSkill={onSelect}
          onQuarantineProposal={onQuarantineProposal}
          onMarkBenchmark={onMarkBenchmark}
        />
      ) : view === "training" ? (
        <SkillTrainingPanel
          runs={trainingRuns}
          selectedRunId={selectedTrainingRunId}
          runDraft={trainingRunDraft}
          epochDraft={trainingEpochDraft}
          pendingId={pendingId}
          skills={skills}
          onSelectRun={onSelectTrainingRun}
          onRunDraft={onTrainingRunDraft}
          onEpochDraft={onTrainingEpochDraft}
          onCreateRun={onCreateTrainingRun}
          onProposeCandidate={onProposeTrainingCandidate}
          onExecuteEpoch={onExecuteTrainingEpoch}
        />
      ) : (
        <SkillsInstalledView
          skills={skills}
          selectedSlug={selectedSlug}
          selected={selected}
          selectedContent={selectedContent}
          pendingId={pendingId}
          draft={draft}
          draftProvenanceReview={draftProvenanceReview}
          onSelect={onSelect}
          onContent={onContent}
          onSave={onSave}
          onToggle={onToggle}
          onDelete={onDelete}
          onTransitionSkill={onTransitionSkill}
          onDraft={onDraft}
          onInstall={onInstall}
          onReviewDraftProvenance={onReviewDraftProvenance}
        />
      )}
    </section>
  );
}
