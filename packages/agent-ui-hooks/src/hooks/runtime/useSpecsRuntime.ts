import { useCallback, useState } from "react";
import {
  addSpecTasks,
  analyzeSpec,
  checklistSpec,
  clarifySpec,
  createSpec,
  getSessionSpecCompletion,
  getSpec,
  getSpecTrace,
  implementSpec,
  listSpecConstitutionDecisions,
  listSpecs,
  planSpec,
  recordSpecConstitutionDecision,
} from "@rightkit/agent-ui-core/lib/daemon";
import type {
  ConstitutionDecision,
  ConstitutionDecisionVerdict,
  DaemonConfig,
  FeatureSpec,
  SessionSpecCompletion,
  SpecAnalysisRun,
  SpecChecklistResponse,
  SpecClarifyResponse,
  SpecEnvelope,
  SpecImplementResponse,
  SpecPlanResponse,
  SpecTrace,
} from "@rightkit/agent-ui-core/lib/types";
import { parseCommaList } from "@rightkit/agent-ui-core/lib/utils";

const DEFAULT_SPEC_DRAFT = {
  title: "",
  description: "",
};

const DEFAULT_SPEC_TASK_DRAFT = {
  description: "",
  requirementIds: "",
  successCriterionIds: "",
  evidenceIds: "",
  foundationPhase: "",
};

const DEFAULT_SPEC_CONSTITUTION_DRAFT = {
  sourcePath: "",
  sourceSha256: "",
  rule: "",
  verdict: "compliant" as ConstitutionDecisionVerdict,
  rationale: "",
  evidenceIds: "",
};

interface UseSpecsRuntimeParams {
  config: DaemonConfig | null;
  runtimeWorkspace: string;
  activeSessionSpecId: string | null;
  sessionId: string | null;
  onStatus: (status: string) => void;
  formatError: (error: unknown, fallback: string) => string;
}

export function useSpecsRuntime({
  config,
  runtimeWorkspace,
  activeSessionSpecId,
  sessionId,
  onStatus,
  formatError,
}: UseSpecsRuntimeParams) {
  const [specs, setSpecs] = useState<FeatureSpec[]>([]);
  const [selectedSpecId, setSelectedSpecId] = useState<string | null>(null);
  const [specEnvelope, setSpecEnvelope] = useState<SpecEnvelope | null>(null);
  const [specAnalysis, setSpecAnalysis] = useState<SpecAnalysisRun | null>(null);
  const [specTrace, setSpecTrace] = useState<SpecTrace | null>(null);
  const [specConstitutionDecisions, setSpecConstitutionDecisions] = useState<
    ConstitutionDecision[]
  >([]);
  const [sessionSpecCompletion, setSessionSpecCompletion] = useState<SessionSpecCompletion | null>(
    null,
  );
  const [specClarifyResult, setSpecClarifyResult] = useState<SpecClarifyResponse | null>(null);
  const [specPlanResult, setSpecPlanResult] = useState<SpecPlanResponse | null>(null);
  const [specChecklistResult, setSpecChecklistResult] = useState<SpecChecklistResponse | null>(
    null,
  );
  const [specImplementResult, setSpecImplementResult] = useState<SpecImplementResponse | null>(
    null,
  );
  const [specActionPending, setSpecActionPending] = useState<string | null>(null);
  const [specDraft, setSpecDraft] = useState(DEFAULT_SPEC_DRAFT);
  const [specTaskDraft, setSpecTaskDraft] = useState(DEFAULT_SPEC_TASK_DRAFT);
  const [specConstitutionDraft, setSpecConstitutionDraft] = useState(
    DEFAULT_SPEC_CONSTITUTION_DRAFT,
  );

  const loadSpecDetail = useCallback(
    async (id: string) => {
      if (!config) return;
      const [envelope, trace, constitutionDecisions] = await Promise.all([
        getSpec(config, id),
        getSpecTrace(config, id),
        listSpecConstitutionDecisions(config, id),
      ]);
      setSpecEnvelope(envelope);
      setSpecTrace(trace);
      setSpecConstitutionDecisions(constitutionDecisions);
      setSpecAnalysis(trace.latestAnalysis ?? null);
    },
    [config],
  );

  const loadSpecsRuntime = useCallback(async () => {
    if (!config) return;
    const [specRows, specCompletion] = await Promise.all([
      listSpecs(config, runtimeWorkspace.trim() || undefined),
      sessionId ? getSessionSpecCompletion(config, sessionId).catch(() => null) : null,
    ]);
    setSpecs(specRows);
    setSessionSpecCompletion(specCompletion);
    const nextSpecId =
      selectedSpecId && specRows.some((spec) => spec.id === selectedSpecId)
        ? selectedSpecId
        : activeSessionSpecId && specRows.some((spec) => spec.id === activeSessionSpecId)
          ? activeSessionSpecId
          : (specRows[0]?.id ?? null);
    setSelectedSpecId(nextSpecId);
    if (nextSpecId) {
      await loadSpecDetail(nextSpecId);
    } else {
      setSpecEnvelope(null);
      setSpecAnalysis(null);
      setSpecTrace(null);
      setSpecConstitutionDecisions([]);
    }
  }, [activeSessionSpecId, config, loadSpecDetail, runtimeWorkspace, selectedSpecId, sessionId]);

  const selectSpec = useCallback(
    async (id: string) => {
      setSelectedSpecId(id);
      if (!config) return;
      try {
        await loadSpecDetail(id);
      } catch (err) {
        onStatus(formatError(err, "Spec detail failed"));
      }
    },
    [config, formatError, loadSpecDetail, onStatus],
  );

  const patchSpecDraft = useCallback((patch: Partial<typeof DEFAULT_SPEC_DRAFT>) => {
    setSpecDraft((draft) => ({ ...draft, ...patch }));
  }, []);

  const patchSpecTaskDraft = useCallback((patch: Partial<typeof DEFAULT_SPEC_TASK_DRAFT>) => {
    setSpecTaskDraft((draft) => ({ ...draft, ...patch }));
  }, []);

  const patchSpecConstitutionDraft = useCallback(
    (patch: Partial<typeof DEFAULT_SPEC_CONSTITUTION_DRAFT>) => {
      setSpecConstitutionDraft((draft) => ({ ...draft, ...patch }));
    },
    [],
  );

  const createRuntimeSpec = useCallback(async () => {
    if (!config || !runtimeWorkspace.trim() || !specDraft.description.trim()) return;
    setSpecActionPending("create");
    try {
      const created = await createSpec(config, {
        workspaceRoot: runtimeWorkspace.trim(),
        title: specDraft.title.trim() || undefined,
        description: specDraft.description.trim(),
      });
      const next = await listSpecs(config, runtimeWorkspace.trim());
      setSpecs(next);
      setSelectedSpecId(created.id);
      setSpecDraft(DEFAULT_SPEC_DRAFT);
      await loadSpecDetail(created.id);
      onStatus(`Spec created: ${created.title}`);
    } catch (err) {
      onStatus(formatError(err, "Create spec failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [config, formatError, loadSpecDetail, onStatus, runtimeWorkspace, specDraft]);

  const addTaskToSelectedSpec = useCallback(async () => {
    if (!config || !selectedSpecId || !specTaskDraft.description.trim()) return;
    setSpecActionPending(`task:${selectedSpecId}`);
    try {
      await addSpecTasks(config, selectedSpecId, [
        {
          description: specTaskDraft.description.trim(),
          requirementIds: parseCommaList(specTaskDraft.requirementIds),
          successCriterionIds: parseCommaList(specTaskDraft.successCriterionIds),
          evidenceIds: parseCommaList(specTaskDraft.evidenceIds),
          foundationPhase: specTaskDraft.foundationPhase.trim() || undefined,
        },
      ]);
      setSpecTaskDraft(DEFAULT_SPEC_TASK_DRAFT);
      setSpecs(await listSpecs(config, runtimeWorkspace.trim() || undefined));
      await loadSpecDetail(selectedSpecId);
      onStatus("Spec task added");
    } catch (err) {
      onStatus(formatError(err, "Add spec task failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [
    config,
    formatError,
    loadSpecDetail,
    onStatus,
    runtimeWorkspace,
    selectedSpecId,
    specTaskDraft,
  ]);

  const analyzeSelectedSpec = useCallback(async () => {
    if (!config || !selectedSpecId) return;
    setSpecActionPending(`analyze:${selectedSpecId}`);
    try {
      const run = await analyzeSpec(config, selectedSpecId);
      setSpecAnalysis(run);
      await loadSpecDetail(selectedSpecId);
      onStatus(`Spec analysis: ${run.status}`);
    } catch (err) {
      onStatus(formatError(err, "Spec analysis failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [config, formatError, loadSpecDetail, onStatus, selectedSpecId]);

  const clarifySelectedSpec = useCallback(async () => {
    if (!config || !selectedSpecId) return;
    setSpecActionPending(`clarify:${selectedSpecId}`);
    try {
      const result = await clarifySpec(config, selectedSpecId);
      setSpecClarifyResult(result);
      onStatus(
        result.questions.length
          ? "Spec clarification questions ready"
          : "Spec has no clarification blockers",
      );
    } catch (err) {
      onStatus(formatError(err, "Spec clarify failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [config, formatError, onStatus, selectedSpecId]);

  const planSelectedSpec = useCallback(async () => {
    if (!config || !selectedSpecId) return;
    setSpecActionPending(`plan:${selectedSpecId}`);
    try {
      const result = await planSpec(config, selectedSpecId, { includeVerification: true });
      setSpecPlanResult(result);
      setSpecAnalysis(result.analysis);
      setSpecs(await listSpecs(config, runtimeWorkspace.trim() || undefined));
      await loadSpecDetail(selectedSpecId);
      onStatus(`Spec plan created ${result.createdTaskCount} tasks`);
    } catch (err) {
      onStatus(formatError(err, "Spec plan failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [config, formatError, loadSpecDetail, onStatus, runtimeWorkspace, selectedSpecId]);

  const checklistSelectedSpec = useCallback(async () => {
    if (!config || !selectedSpecId) return;
    setSpecActionPending(`checklist:${selectedSpecId}`);
    try {
      const result = await checklistSpec(config, selectedSpecId);
      setSpecChecklistResult(result);
      setSpecAnalysis(result.analysis);
      await loadSpecDetail(selectedSpecId);
      onStatus(
        result.ready
          ? "Spec checklist ready"
          : `Spec checklist has ${result.blockerCount} blockers`,
      );
    } catch (err) {
      onStatus(formatError(err, "Spec checklist failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [config, formatError, loadSpecDetail, onStatus, selectedSpecId]);

  const implementSelectedSpec = useCallback(async () => {
    if (!config || !selectedSpecId) return;
    setSpecActionPending(`implement:${selectedSpecId}`);
    try {
      const result = await implementSpec(config, selectedSpecId);
      setSpecImplementResult(result);
      setSpecAnalysis(result.analysis);
      setSpecs(await listSpecs(config, runtimeWorkspace.trim() || undefined));
      await loadSpecDetail(selectedSpecId);
      onStatus(
        result.sessionId
          ? `Implementation session created: ${result.sessionId}`
          : "Implementation prompt prepared",
      );
    } catch (err) {
      onStatus(formatError(err, "Spec implement failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [config, formatError, loadSpecDetail, onStatus, runtimeWorkspace, selectedSpecId]);

  const recordSelectedSpecConstitutionDecision = useCallback(async () => {
    if (!config || !selectedSpecId || !specConstitutionDraft.rule.trim()) return;
    setSpecActionPending(`constitution:${selectedSpecId}`);
    try {
      const decision = await recordSpecConstitutionDecision(config, selectedSpecId, {
        sourcePath: specConstitutionDraft.sourcePath.trim() || undefined,
        sourceSha256: specConstitutionDraft.sourceSha256.trim() || undefined,
        rule: specConstitutionDraft.rule.trim(),
        verdict: specConstitutionDraft.verdict,
        rationale: specConstitutionDraft.rationale.trim(),
        evidenceIds: parseCommaList(specConstitutionDraft.evidenceIds),
      });
      setSpecConstitutionDecisions(await listSpecConstitutionDecisions(config, selectedSpecId));
      await loadSpecDetail(selectedSpecId);
      setSpecConstitutionDraft((draft) => ({
        ...DEFAULT_SPEC_CONSTITUTION_DRAFT,
        sourcePath: draft.sourcePath,
        sourceSha256: draft.sourceSha256,
      }));
      onStatus(`Constitution decision recorded: ${decision.verdict}`);
    } catch (err) {
      onStatus(formatError(err, "Constitution decision failed"));
    } finally {
      setSpecActionPending(null);
    }
  }, [config, formatError, loadSpecDetail, onStatus, selectedSpecId, specConstitutionDraft]);

  return {
    specs,
    selectedSpecId,
    sessionSpecCompletion,
    specClarifyResult,
    specPlanResult,
    specChecklistResult,
    specImplementResult,
    specEnvelope,
    specAnalysis,
    specTrace,
    specConstitutionDecisions,
    specActionPending,
    specDraft,
    specTaskDraft,
    specConstitutionDraft,
    loadSpecsRuntime,
    selectSpec,
    patchSpecDraft,
    patchSpecTaskDraft,
    patchSpecConstitutionDraft,
    createRuntimeSpec,
    addTaskToSelectedSpec,
    analyzeSelectedSpec,
    clarifySelectedSpec,
    planSelectedSpec,
    checklistSelectedSpec,
    implementSelectedSpec,
    recordSelectedSpecConstitutionDecision,
  };
}
