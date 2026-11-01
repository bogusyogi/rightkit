import type {
  ArtifactRow,
  AuditLogView,
  BenchmarkGate,
  BenchmarkReport,
  BenchmarkRunRequest,
  BenchmarkRunResult,
  DaemonConfig,
  FinetuningRow,
  HarnessRadarSnapshot,
  LearningProposal,
  MiddlewareRegistrySnapshot,
  RejectedEdit,
  RoleMetric,
  RolloutView,
  RuntimeObservation,
  RuntimeSpan,
  ScheduleRow,
  SkillOptimizationEpochInput,
  SkillOptimizationEpochResult,
  SkillOptimizationProposalInput,
  SkillOptimizationProposalResult,
  SkillOptimizationRun,
  SkillOptimizationTarget,
  TaskNode,
  TaskRow,
  TasteProposal,
  TasteScore,
  TerminalBenchRunResult,
  TerminalBenchTask,
} from "../types";
import { requestDaemonApi } from "../generated/daemon-openapi";

export async function getTasks(
  config: DaemonConfig,
  sessionId?: string,
  workspaceRoot?: string,
): Promise<TaskRow[]> {
  return requestDaemonApi<TaskRow[]>(config, "getTasks", {
    query: { sessionId, workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function getArtifacts(
  config: DaemonConfig,
  sessionId: string,
): Promise<ArtifactRow[]> {
  return requestDaemonApi<ArtifactRow[]>(config, "getSessionsByIdRuntimeArtifacts", {
    pathParams: { id: sessionId },
  });
}

export async function getTaskGraph(config: DaemonConfig, sessionId: string): Promise<TaskNode[]> {
  return requestDaemonApi<TaskNode[]>(config, "getSessionsByIdRuntimeTaskgraph", {
    pathParams: { id: sessionId },
  });
}

export async function getLearning(config: DaemonConfig): Promise<LearningProposal[]> {
  const rows = await requestDaemonApi<unknown[]>(config, "getRuntimeLearning");
  return rows.map(normalizeLearningProposal);
}

function normalizeLearningProposal(row: unknown): LearningProposal {
  const value = (row && typeof row === "object" ? row : {}) as Record<string, unknown>;
  return {
    id: String(value.id ?? ""),
    kind: String(value.kind ?? ""),
    title: String(value.title ?? value.summary ?? value.id ?? "Learning proposal"),
    status: String(value.status ?? "Candidate"),
    confidence: Number(value.confidence ?? value.score ?? 0),
    body: String(value.body ?? value.evidence ?? ""),
    path: typeof value.path === "string" ? value.path : undefined,
  };
}

export async function decideLearningProposal(
  config: DaemonConfig,
  proposalId: string,
  decision: "approve" | "reject",
): Promise<void> {
  await requestDaemonApi(
    config,
    decision === "approve" ? "postRuntimeLearningByIdApprove" : "postRuntimeLearningByIdReject",
    { pathParams: { id: proposalId } },
  );
}

export async function listSkillOptimizationRuns(
  config: DaemonConfig,
): Promise<SkillOptimizationRun[]> {
  const body = await requestDaemonApi<{ runs?: SkillOptimizationRun[] }>(
    config,
    "getRuntimeSkilloptRuns",
  );
  return body.runs ?? [];
}

export async function getHarnessRadar(config: DaemonConfig): Promise<HarnessRadarSnapshot> {
  return requestDaemonApi<HarnessRadarSnapshot>(config, "getRuntimeHarnessRadar");
}

export async function promoteHarnessRadarProposal(
  config: DaemonConfig,
  proposalId: string,
): Promise<LearningProposal> {
  return normalizeLearningProposal(
    await requestDaemonApi(config, "postRuntimeHarnessRadarProposalsByIdPromote", {
      pathParams: { id: proposalId },
    }),
  );
}

export async function scheduleHarnessRadarRefresh(
  config: DaemonConfig,
  body: {
    workspaceRoot: string;
    cronExpr?: string;
    timeZone?: string;
    paused?: boolean;
  },
): Promise<{ scheduled: boolean; id: string; schedule: ScheduleRow }> {
  return requestDaemonApi<{ scheduled: boolean; id: string; schedule: ScheduleRow }, typeof body>(
    config,
    "postRuntimeHarnessRadarSchedule",
    { body },
  );
}

export async function createSkillOptimizationRun(
  config: DaemonConfig,
  body: {
    name: string;
    target: SkillOptimizationTarget;
    rejectedEdits?: RejectedEdit[];
    gates?: BenchmarkGate[];
  },
): Promise<SkillOptimizationRun> {
  return requestDaemonApi<SkillOptimizationRun, typeof body>(config, "postRuntimeSkilloptRuns", {
    body,
  });
}

export async function executeSkillOptimizationEpoch(
  config: DaemonConfig,
  runId: string,
  body: SkillOptimizationEpochInput,
): Promise<SkillOptimizationEpochResult> {
  return requestDaemonApi<SkillOptimizationEpochResult, SkillOptimizationEpochInput>(
    config,
    "postRuntimeSkilloptRunsByIdExecute",
    { pathParams: { id: runId }, body },
  );
}

export async function proposeSkillOptimizationCandidate(
  config: DaemonConfig,
  runId: string,
  body: SkillOptimizationProposalInput = {},
): Promise<SkillOptimizationProposalResult> {
  return requestDaemonApi<SkillOptimizationProposalResult, SkillOptimizationProposalInput>(
    config,
    "postRuntimeSkilloptRunsByIdProposeCandidate",
    {
      pathParams: { id: runId },
      body,
    },
  );
}

export async function getBenchmarks(config: DaemonConfig): Promise<BenchmarkReport[]> {
  return requestDaemonApi<BenchmarkReport[]>(config, "getRuntimeBenchmarks");
}

export async function getRuntimeMiddleware(
  config: DaemonConfig,
): Promise<MiddlewareRegistrySnapshot> {
  return requestDaemonApi<MiddlewareRegistrySnapshot>(config, "getRuntimeMiddleware");
}

export async function setRuntimeMiddlewareEnabled(
  config: DaemonConfig,
  id: string,
  enabled: boolean,
): Promise<void> {
  await requestDaemonApi(config, "putRuntimeMiddlewareByIdEnabled", {
    pathParams: { id },
    body: { enabled },
  });
}

export async function getRuntimeSpans(
  config: DaemonConfig,
  sessionId?: string,
): Promise<RuntimeSpan[]> {
  const body = await requestDaemonApi<{ spans: RuntimeSpan[] }>(config, "getRuntimeSpans", {
    query: { sessionId },
  });
  return body.spans;
}

export async function getRuntimeObservations(
  config: DaemonConfig,
  sessionId?: string,
): Promise<RuntimeObservation[]> {
  const body = await requestDaemonApi<{ observations: RuntimeObservation[] }>(
    config,
    "getRuntimeObservations",
    { query: { sessionId } },
  );
  return body.observations;
}

export async function runBenchmark(
  config: DaemonConfig,
  sessionId: string,
  body: BenchmarkRunRequest,
): Promise<BenchmarkRunResult> {
  return requestDaemonApi<BenchmarkRunResult, BenchmarkRunRequest & { sessionId: string }>(
    config,
    "postRuntimeBenchmark",
    {
      body: {
        sessionId,
        role: body.role,
        model: body.model,
        goal: body.goal,
        repeats: body.repeats,
      },
    },
  );
}

export async function listTerminalBenchTasks(config: DaemonConfig): Promise<TerminalBenchTask[]> {
  return requestDaemonApi<TerminalBenchTask[]>(config, "getRuntimeTerminalBenchTasks");
}

export async function runTerminalBenchTask(
  config: DaemonConfig,
  sessionId: string,
  taskId: string,
  agent: string,
): Promise<TerminalBenchRunResult> {
  return requestDaemonApi<
    TerminalBenchRunResult,
    { sessionId: string; taskId: string; agent: string }
  >(config, "postRuntimeTerminalBenchRun", {
    body: {
      sessionId,
      taskId,
      agent,
    },
  });
}

export async function getMetrics(config: DaemonConfig): Promise<RoleMetric[]> {
  return requestDaemonApi<RoleMetric[]>(config, "getRuntimeMetrics");
}

export async function getAudit(config: DaemonConfig): Promise<AuditLogView> {
  return requestDaemonApi<AuditLogView>(config, "getRuntimeAudit");
}

export interface BlindEntry {
  label: string;
  output: string;
}

export interface CompareResult {
  id: string;
  prompt: string;
  entries: BlindEntry[];
}

export async function runCompare(
  config: DaemonConfig,
  prompt: string,
  models: string[],
): Promise<CompareResult> {
  return requestDaemonApi<CompareResult, { prompt: string; models: string[] }>(
    config,
    "postRuntimeCompare",
    { body: { prompt, models } },
  );
}

export interface JudgeResult {
  winnerModel: string;
  winnerLabel: string;
  field: string[];
  mapping: { label: string; model: string }[];
}

export async function judgeCompare(
  config: DaemonConfig,
  id: string,
  winner: string,
): Promise<JudgeResult> {
  return requestDaemonApi<JudgeResult, { id: string; winner: string }>(
    config,
    "postRuntimeCompareJudge",
    { body: { id, winner } },
  );
}

export async function getTaste(config: DaemonConfig): Promise<TasteScore[]> {
  return requestDaemonApi<TasteScore[]>(config, "getRuntimeTaste");
}

export async function getProposals(config: DaemonConfig): Promise<TasteProposal[]> {
  return requestDaemonApi<TasteProposal[]>(config, "getRuntimeProposals");
}

export async function getRollouts(config: DaemonConfig): Promise<RolloutView[]> {
  return requestDaemonApi<RolloutView[]>(config, "getRuntimeRollouts");
}

export async function getFinetuning(config: DaemonConfig): Promise<FinetuningRow[]> {
  return requestDaemonApi<FinetuningRow[]>(config, "getRuntimeFinetuning");
}

/**
 * Hand a goal to the Conductor for a full multi-agent run. Returns immediately;
 * progress streams over the session's event channel. `dryRun` previews the plan
 * (who would do what) without spawning agents.
 */
