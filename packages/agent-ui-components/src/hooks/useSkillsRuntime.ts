import { useCallback, useRef, useState } from "react";
import type { SkillInstallDraft } from "../components/settings/types.js";
import {
  createSkillOptimizationRun,
  deleteSkill,
  executeSkillOptimizationEpoch,
  installSkill,
  listSkillGovernance,
  listSkillOptimizationRuns,
  listSkills,
  proposeSkillOptimizationCandidate,
  quarantineSkillProposal,
  readSkill,
  recordSkillProposalBenchmark,
  reviewSkillProvenance,
  saveSkill,
  transitionSkillLifecycle,
} from "@rightkit/agent-ui-core/lib/daemon";
import type {
  BenchmarkGateKind,
  DaemonConfig,
  SkillGovernanceResponse,
  SkillItem,
  SkillOptimizationRun,
  SkillProvenanceReviewResponse,
} from "@rightkit/agent-ui-core/lib/types";

interface UseSkillsRuntimeOptions {
  config: DaemonConfig | null;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
}

const EMPTY_SKILL_DRAFT: SkillInstallDraft = {
  slug: "",
  name: "",
  description: "",
  trigger: "",
  sourceUrl: "",
  content: "",
  enabled: true,
  tags: "",
};

export interface SkillTrainingEpochDraft {
  proposedBody: string;
  validationScore: string;
  operation: string;
  summary: string;
  gateMode: "" | BenchmarkGateKind;
  threshold: string;
}

export interface SkillTrainingRunDraft {
  name: string;
  skillId: string;
  objective: string;
  optimizer: string;
  targetModel: string;
  gateName: string;
  gateKind: BenchmarkGateKind;
  gateThreshold: string;
}

const EMPTY_TRAINING_EPOCH_DRAFT: SkillTrainingEpochDraft = {
  proposedBody: "",
  validationScore: "",
  operation: "replace",
  summary: "",
  gateMode: "",
  threshold: "",
};

const EMPTY_TRAINING_RUN_DRAFT: SkillTrainingRunDraft = {
  name: "",
  skillId: "",
  objective: "",
  optimizer: "skillopt-local",
  targetModel: "",
  gateName: "held-out validation",
  gateKind: "hard",
  gateThreshold: "0.8",
};

export function useSkillsRuntime({ config, formatError, onStatus }: UseSkillsRuntimeOptions) {
  const skillLoadSeqRef = useRef(0);
  const [skillDirs, setSkillDirs] = useState<string[]>([]);
  const [skills, setSkills] = useState<SkillItem[]>([]);
  const [selectedSkillSlug, setSelectedSkillSlug] = useState<string | null>(null);
  const [selectedSkillContent, setSelectedSkillContent] = useState("");
  const [skillGovernance, setSkillGovernance] = useState<SkillGovernanceResponse | null>(null);
  const [skillOptimizationRuns, setSkillOptimizationRuns] = useState<SkillOptimizationRun[]>([]);
  const [skillDraftProvenanceReview, setSkillDraftProvenanceReview] =
    useState<SkillProvenanceReviewResponse | null>(null);
  const [selectedSkillOptimizationRunId, setSelectedSkillOptimizationRunId] = useState<
    string | null
  >(null);
  const [skillTrainingRunDraft, setSkillTrainingRunDraft] =
    useState<SkillTrainingRunDraft>(EMPTY_TRAINING_RUN_DRAFT);
  const [skillTrainingEpochDraft, setSkillTrainingEpochDraft] = useState<SkillTrainingEpochDraft>(
    EMPTY_TRAINING_EPOCH_DRAFT,
  );
  const [selectedSkillProposalId, setSelectedSkillProposalId] = useState<string | null>(null);
  const [skillActionPending, setSkillActionPending] = useState<string | null>(null);
  const [skillError, setSkillError] = useState<string | null>(null);
  const [skillDraft, setSkillDraft] = useState<SkillInstallDraft>(EMPTY_SKILL_DRAFT);

  const inspectSkill = useCallback(
    async (slug: string, markSelected = true) => {
      if (!config) return;
      if (markSelected) setSelectedSkillSlug(slug);
      setSkillActionPending(`read:${slug}`);
      setSkillError(null);
      try {
        const next = await readSkill(config, slug);
        setSelectedSkillContent(next.content);
        setSkills((rows) =>
          rows.some((row) => row.slug === next.skill.slug)
            ? rows.map((row) => (row.slug === next.skill.slug ? next.skill : row))
            : [...rows, next.skill],
        );
      } catch (err) {
        const message = formatError(err, "Skill read failed");
        setSkillError(message);
        onStatus(message);
      } finally {
        setSkillActionPending(null);
      }
    },
    [config, formatError, onStatus],
  );

  const loadSkills = useCallback(
    async (preferredSlug = selectedSkillSlug) => {
      if (!config) return;
      const loadSeq = ++skillLoadSeqRef.current;
      setSkillError(null);
      try {
        const [next, governance, trainingRuns] = await Promise.all([
          listSkills(config),
          listSkillGovernance(config),
          listSkillOptimizationRuns(config),
        ]);
        if (loadSeq !== skillLoadSeqRef.current) return;
        setSkillDirs(next.skillDirs);
        setSkills(next.skills);
        setSkillGovernance(governance);
        setSkillOptimizationRuns(trainingRuns);
        setSelectedSkillProposalId((current) =>
          current && governance.proposals.some((proposal) => proposal.id === current)
            ? current
            : (governance.proposals[0]?.id ?? null),
        );
        setSelectedSkillOptimizationRunId((current) =>
          current && trainingRuns.some((run) => run.id === current)
            ? current
            : (trainingRuns[0]?.id ?? null),
        );
        const slug =
          preferredSlug && next.skills.some((skill) => skill.slug === preferredSlug)
            ? preferredSlug
            : (next.skills[0]?.slug ?? null);
        setSelectedSkillSlug(slug);
        if (slug) {
          await inspectSkill(slug, false);
        } else {
          setSelectedSkillContent("");
        }
      } catch (err) {
        const message = formatError(err, "Skills failed");
        setSkillError(message);
        onStatus(message);
      }
    },
    [config, formatError, inspectSkill, onStatus, selectedSkillSlug],
  );

  const loadSkillGovernance = useCallback(async () => {
    if (!config) return;
    setSkillError(null);
    try {
      const governance = await listSkillGovernance(config);
      setSkillGovernance(governance);
      setSelectedSkillProposalId((current) =>
        current && governance.proposals.some((proposal) => proposal.id === current)
          ? current
          : (governance.proposals[0]?.id ?? null),
      );
    } catch (err) {
      const message = formatError(err, "Skill proposals failed");
      setSkillError(message);
      onStatus(message);
    }
  }, [config, formatError, onStatus]);

  const loadSkillOptimizationRuns = useCallback(async () => {
    if (!config) return;
    setSkillError(null);
    try {
      const runs = await listSkillOptimizationRuns(config);
      setSkillOptimizationRuns(runs);
      setSelectedSkillOptimizationRunId((current) =>
        current && runs.some((run) => run.id === current) ? current : (runs[0]?.id ?? null),
      );
    } catch (err) {
      const message = formatError(err, "Skill training runs failed");
      setSkillError(message);
      onStatus(message);
    }
  }, [config, formatError, onStatus]);

  const submitSkillInstall = useCallback(async () => {
    if (!config) return;
    const slug = skillDraft.slug.trim();
    const content = skillDraft.content.trim();
    if (!slug || !content) {
      onStatus("Skill needs a slug and body");
      return;
    }
    setSkillActionPending("install");
    setSkillError(null);
    try {
      const installed = await installSkill(config, {
        slug,
        content,
        name: skillDraft.name.trim() || undefined,
        description: skillDraft.description.trim() || undefined,
        trigger: skillDraft.trigger.trim() || undefined,
        enabled: skillDraft.enabled,
        tags: skillDraft.tags
          .split(",")
          .map((tag) => tag.trim())
          .filter(Boolean),
      });
      setSkillDraft({ ...skillDraft, slug: "", content: "" });
      setSkillDraftProvenanceReview(null);
      await loadSkills(installed.skill.slug);
      onStatus(`Skill installed: ${installed.skill.slug}`);
    } catch (err) {
      const message = formatError(err, "Skill install failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, loadSkills, onStatus, skillDraft]);

  const saveSelectedSkill = useCallback(async () => {
    if (!config || !selectedSkillSlug) return;
    const content = selectedSkillContent.trim();
    if (!content) {
      onStatus("Skill body cannot be empty");
      return;
    }
    setSkillActionPending(`save:${selectedSkillSlug}`);
    setSkillError(null);
    try {
      const saved = await saveSkill(config, selectedSkillSlug, {
        content: selectedSkillContent,
      });
      setSelectedSkillContent(saved.content);
      setSkills((rows) => rows.map((row) => (row.slug === saved.skill.slug ? saved.skill : row)));
      onStatus(`Skill saved: ${saved.skill.slug}`);
    } catch (err) {
      const message = formatError(err, "Skill save failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, onStatus, selectedSkillContent, selectedSkillSlug]);

  const toggleSelectedSkill = useCallback(async () => {
    if (!config || !selectedSkillSlug) return;
    const selectedSkill = skills.find((skill) => skill.slug === selectedSkillSlug);
    if (!selectedSkill) return;
    setSkillActionPending(`toggle:${selectedSkillSlug}`);
    setSkillError(null);
    try {
      const saved = await saveSkill(config, selectedSkillSlug, {
        content: selectedSkillContent,
        enabled: !selectedSkill.enabled,
      });
      setSelectedSkillContent(saved.content);
      setSkills((rows) => rows.map((row) => (row.slug === saved.skill.slug ? saved.skill : row)));
      onStatus(
        saved.skill.enabled
          ? `Skill enabled: ${saved.skill.slug}`
          : `Skill disabled: ${saved.skill.slug}`,
      );
    } catch (err) {
      const message = formatError(err, "Skill toggle failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, onStatus, selectedSkillContent, selectedSkillSlug, skills]);

  const deleteSelectedSkill = useCallback(async () => {
    if (!config || !selectedSkillSlug) return;
    const slug = selectedSkillSlug;
    setSkillActionPending(`delete:${slug}`);
    setSkillError(null);
    try {
      await deleteSkill(config, slug);
      await loadSkills(null);
      onStatus(`Skill deleted: ${slug}`);
    } catch (err) {
      const message = formatError(err, "Skill delete failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, loadSkills, onStatus, selectedSkillSlug]);

  const quarantineSelectedSkillProposal = useCallback(async () => {
    if (!config || !selectedSkillProposalId) return;
    const id = selectedSkillProposalId;
    setSkillActionPending(`proposal:quarantine:${id}`);
    setSkillError(null);
    try {
      const result = await quarantineSkillProposal(
        config,
        id,
        "operator quarantined from Settings",
      );
      await loadSkills(result.skill.slug);
      onStatus(`Skill proposal quarantined: ${result.proposal.slug}`);
    } catch (err) {
      const message = formatError(err, "Skill proposal quarantine failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, loadSkills, onStatus, selectedSkillProposalId]);

  const markSelectedSkillProposalBenchmark = useCallback(
    async (passed: boolean) => {
      if (!config || !selectedSkillProposalId) return;
      const proposal = skillGovernance?.proposals.find((row) => row.id === selectedSkillProposalId);
      if (!proposal) return;
      const id = selectedSkillProposalId;
      setSkillActionPending(`proposal:benchmark:${id}`);
      setSkillError(null);
      try {
        await recordSkillProposalBenchmark(config, id, {
          benchmarkId: `settings-${passed ? "pass" : "fail"}`,
          gateMode: "hard",
          scoreContinuous: passed ? 1 : 0,
          passed,
          reason: passed ? "operator recorded fixture pass" : "operator recorded fixture failure",
        });
        await loadSkillGovernance();
        onStatus(
          passed
            ? `Benchmark pass recorded: ${proposal.slug}`
            : `Benchmark failure recorded: ${proposal.slug}`,
        );
      } catch (err) {
        const message = formatError(err, "Skill benchmark update failed");
        setSkillError(message);
        onStatus(message);
      } finally {
        setSkillActionPending(null);
      }
    },
    [config, formatError, loadSkillGovernance, onStatus, selectedSkillProposalId, skillGovernance],
  );

  const reviewSkillDraftProvenance = useCallback(async () => {
    if (!config) return;
    const sourceUrl = skillDraft.sourceUrl.trim();
    const content = skillDraft.content.trim();
    if (!sourceUrl || !content) {
      onStatus("Skill provenance review needs a source URL and body");
      return;
    }
    setSkillActionPending("install:provenance");
    setSkillError(null);
    try {
      const review = await reviewSkillProvenance(config, {
        sourceUrl,
        content,
      });
      setSkillDraftProvenanceReview(review);
      onStatus(
        review.ok
          ? `Skill provenance reviewed: ${review.manifestName || sourceUrl}`
          : `Skill provenance has findings: ${review.manifestName || sourceUrl}`,
      );
    } catch (err) {
      const message = formatError(err, "Skill provenance review failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, onStatus, skillDraft.content, skillDraft.sourceUrl]);

  const transitionSelectedSkill = useCallback(
    async (action: "quarantine" | "activate" | "retire", operatorOverride = false) => {
      if (!config || !selectedSkillSlug) return;
      const slug = selectedSkillSlug;
      setSkillActionPending(`${action}:${slug}`);
      setSkillError(null);
      try {
        const result = await transitionSkillLifecycle(config, slug, action, {
          reason: `operator ${action} from Settings`,
          operatorOverride,
        });
        setSelectedSkillContent(result.content);
        await loadSkills(result.skill.slug);
        onStatus(`Skill ${action}: ${result.skill.slug}`);
      } catch (err) {
        const message = formatError(err, `Skill ${action} failed`);
        setSkillError(message);
        onStatus(message);
      } finally {
        setSkillActionPending(null);
      }
    },
    [config, formatError, loadSkills, onStatus, selectedSkillSlug],
  );

  const executeSelectedSkillOptimizationEpoch = useCallback(async () => {
    if (!config || !selectedSkillOptimizationRunId) return;
    const validationScore = Number.parseFloat(skillTrainingEpochDraft.validationScore);
    const threshold = skillTrainingEpochDraft.threshold.trim()
      ? Number.parseFloat(skillTrainingEpochDraft.threshold)
      : null;
    const proposedBody = skillTrainingEpochDraft.proposedBody.trim();
    if (!proposedBody || !Number.isFinite(validationScore)) {
      onStatus("Skill training needs a candidate body and numeric validation score");
      return;
    }
    if (threshold !== null && !Number.isFinite(threshold)) {
      onStatus("Skill training threshold must be numeric");
      return;
    }
    const runId = selectedSkillOptimizationRunId;
    setSkillActionPending(`training:execute:${runId}`);
    setSkillError(null);
    try {
      const result = await executeSkillOptimizationEpoch(config, runId, {
        proposedBody,
        validationScore,
        operation: skillTrainingEpochDraft.operation.trim() || "replace",
        summary: skillTrainingEpochDraft.summary.trim() || undefined,
        gateMode: skillTrainingEpochDraft.gateMode || undefined,
        threshold: threshold ?? undefined,
      });
      setSkillOptimizationRuns((runs) =>
        runs.some((run) => run.id === result.run.id)
          ? runs.map((run) => (run.id === result.run.id ? result.run : run))
          : [result.run, ...runs],
      );
      setSelectedSkillOptimizationRunId(result.run.id);
      setSkillTrainingEpochDraft((draft) => ({ ...draft, proposedBody: "" }));
      onStatus(
        result.accepted
          ? `Skill training candidate accepted: ${result.candidate.id}`
          : `Skill training candidate rejected: ${result.candidate.id}`,
      );
    } catch (err) {
      const message = formatError(err, "Skill training epoch failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, onStatus, selectedSkillOptimizationRunId, skillTrainingEpochDraft]);

  const proposeCandidateForSelectedSkillOptimizationRun = useCallback(async () => {
    if (!config || !selectedSkillOptimizationRunId) return;
    const runId = selectedSkillOptimizationRunId;
    setSkillActionPending(`training:propose:${runId}`);
    setSkillError(null);
    try {
      const result = await proposeSkillOptimizationCandidate(config, runId, {
        operation: skillTrainingEpochDraft.operation.trim() || undefined,
        summary: skillTrainingEpochDraft.summary.trim() || undefined,
      });
      setSkillOptimizationRuns((runs) =>
        runs.some((run) => run.id === result.run.id)
          ? runs.map((run) => (run.id === result.run.id ? result.run : run))
          : [result.run, ...runs],
      );
      setSelectedSkillOptimizationRunId(result.run.id);
      setSkillTrainingEpochDraft((draft) => ({
        ...draft,
        proposedBody: result.proposal.proposedBody,
        operation: result.proposal.operation,
        summary: result.proposal.summary,
      }));
      onStatus(`Skill training candidate generated: ${result.proposal.id}`);
    } catch (err) {
      const message = formatError(err, "Skill candidate generation failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [
    config,
    formatError,
    onStatus,
    selectedSkillOptimizationRunId,
    skillTrainingEpochDraft.operation,
    skillTrainingEpochDraft.summary,
  ]);

  const createSkillTrainingRun = useCallback(async () => {
    if (!config) return;
    const name = skillTrainingRunDraft.name.trim();
    const skillId = skillTrainingRunDraft.skillId.trim();
    const objective = skillTrainingRunDraft.objective.trim();
    const optimizer = skillTrainingRunDraft.optimizer.trim();
    const targetModel = skillTrainingRunDraft.targetModel.trim();
    const gateName = skillTrainingRunDraft.gateName.trim();
    const gateThreshold = Number.parseFloat(skillTrainingRunDraft.gateThreshold);
    if (!name || !skillId || !objective || !optimizer) {
      onStatus("Skill training run needs name, skill, objective, and optimizer");
      return;
    }
    if (gateName && !Number.isFinite(gateThreshold)) {
      onStatus("Skill training gate threshold must be numeric");
      return;
    }
    setSkillActionPending("training:create");
    setSkillError(null);
    try {
      const run = await createSkillOptimizationRun(config, {
        name,
        target: {
          skillId,
          objective,
          optimizer,
          targetModel: targetModel || undefined,
        },
        gates: gateName
          ? [
              {
                name: gateName,
                kind: skillTrainingRunDraft.gateKind,
                threshold: gateThreshold,
              },
            ]
          : [],
      });
      setSkillOptimizationRuns((runs) => [run, ...runs.filter((row) => row.id !== run.id)]);
      setSelectedSkillOptimizationRunId(run.id);
      setSkillTrainingRunDraft((draft) => ({
        ...EMPTY_TRAINING_RUN_DRAFT,
        skillId: draft.skillId,
        optimizer: draft.optimizer || EMPTY_TRAINING_RUN_DRAFT.optimizer,
      }));
      onStatus(`Skill training run created: ${run.name}`);
    } catch (err) {
      const message = formatError(err, "Skill training run create failed");
      setSkillError(message);
      onStatus(message);
    } finally {
      setSkillActionPending(null);
    }
  }, [config, formatError, onStatus, skillTrainingRunDraft]);

  return {
    skillDirs,
    skills,
    selectedSkillSlug,
    selectedSkillContent,
    setSelectedSkillContent,
    skillGovernance,
    skillOptimizationRuns,
    skillDraftProvenanceReview,
    selectedSkillOptimizationRunId,
    setSelectedSkillOptimizationRunId,
    skillTrainingRunDraft,
    setSkillTrainingRunDraft,
    skillTrainingEpochDraft,
    setSkillTrainingEpochDraft,
    selectedSkillProposalId,
    setSelectedSkillProposalId,
    skillActionPending,
    skillError,
    skillDraft,
    setSkillDraft,
    loadSkills,
    loadSkillGovernance,
    loadSkillOptimizationRuns,
    inspectSkill,
    submitSkillInstall,
    saveSelectedSkill,
    toggleSelectedSkill,
    deleteSelectedSkill,
    quarantineSelectedSkillProposal,
    markSelectedSkillProposalBenchmark,
    reviewSkillDraftProvenance,
    transitionSelectedSkill,
    createSkillTrainingRun,
    proposeCandidateForSelectedSkillOptimizationRun,
    executeSelectedSkillOptimizationEpoch,
  };
}

export type UseSkillsRuntimeReturn = ReturnType<typeof useSkillsRuntime>;
