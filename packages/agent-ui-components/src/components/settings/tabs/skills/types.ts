import type {
  BenchmarkGateKind,
  SkillGovernanceResponse,
  SkillItem,
  SkillOptimizationRun,
  SkillProposalRecord,
  SkillProvenanceReviewResponse,
} from "@rightkit/agent-ui-core/lib/types";
import type {
  SkillInstallDraft,
  SkillTrainingEpochDraft,
  SkillTrainingRunDraft,
} from "../../types.js";

export type SkillLifecycleAction = "quarantine" | "activate" | "retire";

export type SkillsView = "installed" | "proposals" | "training";

export interface SkillProposalStat {
  label: string;
  value: number;
}

export interface SkillProposalMetaProps {
  label: string;
  value: string;
}

export interface SkillListPanelProps {
  skills: SkillItem[];
  selectedSlug: string | null;
  onSelect: (slug: string) => void;
}

export interface SkillDetailPanelProps {
  selected: SkillItem | null;
  selectedContent: string;
  pendingId: string | null;
  onContent: (content: string) => void;
  onSave: () => void;
  onToggle: () => void;
  onDelete: () => void;
  onTransitionSkill: (action: SkillLifecycleAction, operatorOverride?: boolean) => void;
}

export interface SkillInstallFormPanelProps {
  draft: SkillInstallDraft;
  pendingId: string | null;
  draftProvenanceReview: SkillProvenanceReviewResponse | null;
  onDraft: (draft: SkillInstallDraft) => void;
  onInstall: () => void;
  onReviewDraftProvenance: () => void;
}

export interface SkillProposalActionsProps {
  selectedProposal: SkillProposalRecord | null;
  installedSkill: SkillItem | null;
  pendingId: string | null;
  onSelectInstalledSkill: (slug: string) => void;
  onQuarantineProposal: () => void;
  onMarkBenchmark: (passed: boolean) => void;
}

export interface SkillGovernancePanelProps {
  proposals: SkillProposalRecord[];
  versions: SkillGovernanceResponse["versions"];
  verdicts: SkillGovernanceResponse["benchmarkVerdicts"];
  selectedProposal: SkillProposalRecord | null;
  installedSkill: SkillItem | null;
  installedContent: string;
  pendingId: string | null;
  onSelectProposal: (id: string) => void;
  onSelectInstalledSkill: (slug: string) => void;
  onQuarantineProposal: () => void;
  onMarkBenchmark: (passed: boolean) => void;
}

export interface SkillProvenanceReviewPanelProps {
  review: SkillProvenanceReviewResponse;
}

export interface SkillTrainingPanelProps {
  runs: SkillOptimizationRun[];
  selectedRunId: string | null;
  runDraft: SkillTrainingRunDraft;
  epochDraft: SkillTrainingEpochDraft;
  pendingId: string | null;
  skills: SkillItem[];
  onSelectRun: (id: string) => void;
  onRunDraft: (draft: SkillTrainingRunDraft) => void;
  onEpochDraft: (draft: SkillTrainingEpochDraft) => void;
  onCreateRun: () => void;
  onProposeCandidate: () => void;
  onExecuteEpoch: () => void;
}

export interface SkillTrainingRunStats {
  totalRuns: number;
  acceptedEdits: number;
  rejectedEdits: number;
}

export type SkillTrainingGateKind = BenchmarkGateKind;
