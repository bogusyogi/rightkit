import { useCallback, useState } from "react";
import {
  decideLearningProposal,
  getAudit,
  getBenchmarks,
  getFinetuning,
  getLearning,
  getMetrics,
  getProposals,
  getRollouts,
  getTaste,
  listTerminalBenchTasks,
  runBenchmark,
  runTerminalBenchTask,
} from "@rightkit/agent-ui-core/lib/daemon";
import type {
  AuditLogView,
  BenchmarkReport,
  BenchmarkRunRequest,
  DaemonConfig,
  FinetuningRow,
  LearningProposal,
  RoleMetric,
  RolloutView,
  TasteProposal,
  TasteScore,
  TerminalBenchRunResult,
  TerminalBenchTask,
} from "@rightkit/agent-ui-core/lib/types";
import { hostAppName } from "../../appName.js";

const EMPTY_AUDIT: AuditLogView = {
  verified: true,
  entries: [],
};

interface UseLearningRuntimeParams {
  config: DaemonConfig | null;
  sessionId: string | null;
  onStatus: (status: string) => void;
  formatError: (error: unknown, fallback: string) => string;
}

export function useLearningRuntime({
  config,
  sessionId,
  onStatus,
  formatError,
}: UseLearningRuntimeParams) {
  const [learning, setLearning] = useState<LearningProposal[]>([]);
  const [learningActionPending, setLearningActionPending] = useState<string | null>(null);
  const [benchmarks, setBenchmarks] = useState<BenchmarkReport[]>([]);
  const [benchmarkActionPending, setBenchmarkActionPending] = useState(false);
  const [terminalBenchTasks, setTerminalBenchTasks] = useState<TerminalBenchTask[]>([]);
  const [terminalBenchActionPending, setTerminalBenchActionPending] = useState(false);
  const [latestTerminalBenchResult, setLatestTerminalBenchResult] =
    useState<TerminalBenchRunResult | null>(null);
  const [taste, setTaste] = useState<TasteScore[]>([]);
  const [roleMetrics, setRoleMetrics] = useState<RoleMetric[]>([]);
  const [rollouts, setRollouts] = useState<RolloutView[]>([]);
  const [finetuning, setFinetuning] = useState<FinetuningRow[]>([]);
  const [proposals, setProposals] = useState<TasteProposal[]>([]);
  const [audit, setAudit] = useState<AuditLogView>(EMPTY_AUDIT);

  const loadLearningRuntime = useCallback(async () => {
    if (!config) return;
    const [l, b, tb, ts, mx, ro, ft, pr, au] = await Promise.all([
      getLearning(config),
      getBenchmarks(config),
      listTerminalBenchTasks(config),
      getTaste(config),
      getMetrics(config),
      getRollouts(config),
      getFinetuning(config),
      getProposals(config),
      getAudit(config),
    ]);
    setLearning(l);
    setBenchmarks(b);
    setTerminalBenchTasks(tb);
    setTaste(ts);
    setRoleMetrics(mx);
    setRollouts(ro);
    setFinetuning(ft);
    setProposals(pr);
    setAudit(au);
  }, [config]);

  const startBenchmarkRun = useCallback(
    async (body: BenchmarkRunRequest) => {
      if (!config || !sessionId) return;
      setBenchmarkActionPending(true);
      try {
        const result = await runBenchmark(config, sessionId, body);
        onStatus(`Benchmark ${result.role}: ${result.successes}/${result.repeats} passed`);
        setBenchmarks(await getBenchmarks(config));
      } catch (err) {
        onStatus(formatError(err, "Benchmark failed"));
      } finally {
        setBenchmarkActionPending(false);
      }
    },
    [config, formatError, onStatus, sessionId],
  );

  const startTerminalBenchRun = useCallback(
    async (taskId: string, agent: string) => {
      if (!config || !sessionId) return;
      setTerminalBenchActionPending(true);
      try {
        const result = await runTerminalBenchTask(config, sessionId, taskId, agent);
        onStatus(`${hostAppName()} task pack ${result.taskId}: ${result.passed ? "passed" : "failed"}`);
        setLatestTerminalBenchResult(result);
        setBenchmarks(await getBenchmarks(config));
      } catch (err) {
        onStatus(formatError(err, `${hostAppName()} task-pack run failed`));
      } finally {
        setTerminalBenchActionPending(false);
      }
    },
    [config, formatError, onStatus, sessionId],
  );

  const submitLearningDecision = useCallback(
    async (
      proposalId: string,
      decision: "approve" | "reject",
      onAfterDecision?: () => Promise<void> | void,
    ) => {
      if (!config) return;
      setLearningActionPending(proposalId);
      try {
        await decideLearningProposal(config, proposalId, decision);
        const nextStatus = decision === "approve" ? "Promoted" : "Rejected";
        setLearning((rows) =>
          rows.map((row) => (row.id === proposalId ? { ...row, status: nextStatus } : row)),
        );
        onStatus(`Learning proposal ${decision === "approve" ? "approved" : "rejected"}`);
        await onAfterDecision?.();
      } catch (err) {
        onStatus(formatError(err, "Learning proposal update failed"));
      } finally {
        setLearningActionPending(null);
      }
    },
    [config, formatError, onStatus],
  );

  return {
    learning,
    learningActionPending,
    benchmarks,
    benchmarkActionPending,
    terminalBenchTasks,
    terminalBenchActionPending,
    latestTerminalBenchResult,
    taste,
    roleMetrics,
    rollouts,
    finetuning,
    proposals,
    audit,
    loadLearningRuntime,
    startBenchmarkRun,
    startTerminalBenchRun,
    submitLearningDecision,
  };
}
