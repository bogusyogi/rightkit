import { Select } from "@rightkit/react-shell/react";
import { useState } from "react";
import type {
  BenchmarkReport,
  BenchmarkRunRequest,
  TerminalBenchRunResult,
  TerminalBenchTask,
} from "@rightkit/agent-ui-core/lib/types";
import { RuntimeEmpty, ScoreBar, formatPanelTime, panelWrap, scoreColor } from "./shared";
import { hostAppName } from "../../../appName";

export function BenchmarksPanel({
  rows,
  selectedSessionId,
  pending,
  terminalBenchTasks,
  terminalBenchPending,
  latestTerminalBenchResult,
  onRunBenchmark,
  onRunTerminalBench,
}: {
  rows: BenchmarkReport[];
  selectedSessionId?: string | null;
  pending: boolean;
  terminalBenchTasks: TerminalBenchTask[];
  terminalBenchPending: boolean;
  latestTerminalBenchResult: TerminalBenchRunResult | null;
  onRunBenchmark: (body: BenchmarkRunRequest) => void;
  onRunTerminalBench: (taskId: string, agent: string) => void;
}) {
  const [role, setRole] = useState("coder");
  const [model, setModel] = useState("");
  const [goal, setGoal] = useState("");
  const [repeats, setRepeats] = useState(3);
  const [terminalTaskId, setTerminalTaskId] = useState("");
  const [terminalAgent, setTerminalAgent] = useState("coder");
  const effectiveTaskId = terminalTaskId || terminalBenchTasks[0]?.id || "";
  const selectedTerminalTask = terminalBenchTasks.find((task) => task.id === effectiveTaskId);
  const canRun =
    Boolean(selectedSessionId) &&
    role.trim().length > 0 &&
    goal.trim().length > 0 &&
    repeats > 0 &&
    !pending;
  const canRunTerminal =
    Boolean(selectedSessionId) &&
    Boolean(effectiveTaskId) &&
    terminalAgent.trim().length > 0 &&
    !terminalBenchPending;

  return (
    <div className={panelWrap}>
      <div className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-3">
        <div className="grid grid-cols-[minmax(120px,0.7fr)_minmax(140px,1fr)_80px_auto] gap-2 items-center">
          <input
            aria-label="role"
            className="h-8 min-w-0 rounded-lg border border-line bg-panel-2 px-3 text-[12px] text-text outline-none focus:border-accent"
            value={role}
            onChange={(e) => setRole(e.target.value)}
            placeholder="role"
          />
          <input
            aria-label="model override"
            className="h-8 min-w-0 rounded-lg border border-line bg-panel-2 px-3 text-[12px] text-text outline-none focus:border-accent"
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder="model override"
          />
          <input
            aria-label="Benchmark repeat count"
            className="h-8 min-w-0 rounded-lg border border-line bg-panel-2 px-2 text-[12px] text-text outline-none focus:border-accent"
            type="number"
            min={1}
            max={20}
            value={repeats}
            onChange={(e) => setRepeats(Number(e.target.value))}
            title="Repeats"
          />
          <button
            type="button"
            className="inline-flex items-center justify-center h-8 px-3 rounded-lg border border-accent/40 bg-accent/10 text-accent text-[12px] font-semibold cursor-pointer hover:bg-accent/15 disabled:opacity-40 disabled:cursor-not-allowed"
            disabled={!canRun}
            onClick={() =>
              onRunBenchmark({
                role: role.trim(),
                model: model.trim() || undefined,
                goal: goal.trim(),
                repeats,
              })
            }
          >
            {pending ? "Running" : "Run"}
          </button>
        </div>
        <textarea
          aria-label="Benchmark goal"
          className="min-h-[72px] rounded-lg border border-line bg-panel-2 px-3 py-2 text-[12px] text-text outline-none resize-y focus:border-accent"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
          placeholder="benchmark goal"
        />
        {!selectedSessionId ? (
          <span className="text-[11px] text-text-3">
            Select a session before running a benchmark.
          </span>
        ) : null}
      </div>
      <div className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-3">
        <div className="grid grid-cols-[minmax(180px,1fr)_minmax(120px,0.6fr)_auto] gap-2 items-center">
          <Select
            label="Terminal benchmark task"
            className="h-8 min-w-0 rounded-lg border border-line bg-panel-2 px-3 text-[12px] text-text outline-none focus:border-accent"
            value={effectiveTaskId}
            onChange={(value) => setTerminalTaskId(value)}
            disabled={!terminalBenchTasks.length}
            options={
              terminalBenchTasks.length
                ? terminalBenchTasks.map((task) => ({
                    value: task.id,
                    label: `${task.id} - ${task.category} - ${task.difficulty}`,
                  }))
                : [{ value: "", label: "No tasks loaded" }]
            }
          />
          <input
            aria-label="Terminal agent"
            className="h-8 min-w-0 rounded-lg border border-line bg-panel-2 px-3 text-[12px] text-text outline-none focus:border-accent"
            value={terminalAgent}
            onChange={(e) => setTerminalAgent(e.target.value)}
            placeholder="agent"
          />
          <button
            type="button"
            className="inline-flex items-center justify-center h-8 px-3 rounded-lg border border-accent-2/40 bg-accent-2/10 text-accent-2 text-[12px] font-semibold cursor-pointer hover:bg-accent-2/15 disabled:opacity-40 disabled:cursor-not-allowed"
            disabled={!canRunTerminal}
            onClick={() => onRunTerminalBench(effectiveTaskId, terminalAgent.trim())}
          >
            {terminalBenchPending ? "Running" : "Run task"}
          </button>
        </div>
        {selectedTerminalTask ? (
          <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,0.7fr)] gap-3 text-[11px]">
            <p className="text-text-2 leading-relaxed line-clamp-3">
              {selectedTerminalTask.prompt}
            </p>
            <div className="min-w-0 text-right">
              <code className="block font-mono text-accent-2 truncate">
                {selectedTerminalTask.verifyCommand}
              </code>
              <span className="block text-text-3 truncate">
                expected {(selectedTerminalTask.expectedPassRate * 100).toFixed(0)}%
              </span>
            </div>
          </div>
        ) : null}
        {latestTerminalBenchResult ? (
          <div className="rounded-lg border border-line bg-panel-2 p-3 grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-3">
            <div className="min-w-0">
              <div className="flex items-center gap-2 mb-2">
                <span
                  className={`w-2 h-2 rounded-full ${
                    latestTerminalBenchResult.passed ? "bg-accent-2" : "bg-danger"
                  }`}
                />
                <strong className="text-[12px] font-semibold text-text truncate">
                  {latestTerminalBenchResult.taskId}
                </strong>
                <span className="text-[11px] text-text-3">
                  {latestTerminalBenchResult.passed ? "passed" : "failed"}
                </span>
              </div>
              <pre className="max-h-28 overflow-auto text-[11px] text-text-2 whitespace-pre-wrap break-words font-mono">
                {latestTerminalBenchResult.verifyOutput ||
                  latestTerminalBenchResult.verifyError ||
                  "(no verifier output)"}
              </pre>
            </div>
            <div className="min-w-0">
              <span className="block text-[10px] uppercase tracking-wider text-text-3 mb-2">
                Agent output
              </span>
              <pre className="max-h-28 overflow-auto text-[11px] text-text-2 whitespace-pre-wrap break-words font-mono">
                {latestTerminalBenchResult.agentOutput || "(empty)"}
              </pre>
            </div>
          </div>
        ) : null}
      </div>
      {!rows.length ? (
        <RuntimeEmpty
          title="No benchmark evidence yet"
          body={`Role and ${hostAppName()} internal task-pack runs appear here after \`/benchmarks\`, the legacy \`/terminal-bench run\` command, or the benchmark API records validation evidence.`}
        />
      ) : null}
      {rows.map((r) => {
        const rate = Number.isFinite(r.successRate) ? r.successRate : 0;
        const modelSlug = r.model.includes("/") ? r.model.split("/").slice(-1)[0] : r.model;
        return (
          <div
            key={r.id}
            className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-3"
          >
            <div className="flex items-start gap-3">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 min-w-0">
                  <span className="text-[10px] uppercase tracking-wider text-text-2 border border-line bg-panel-2 px-2 py-0.5 rounded-md font-semibold">
                    {r.role}
                  </span>
                  <strong className="text-[13px] font-semibold text-text truncate">
                    {r.suite || "benchmark"}
                  </strong>
                </div>
                <code className="block mt-1 text-[11px] font-mono text-text-3 truncate">
                  {modelSlug || r.model}
                </code>
              </div>
              <span
                className="text-[14px] font-bold font-mono tabular-nums"
                style={{ color: scoreColor(rate) }}
              >
                {(rate * 100).toFixed(0)}%
              </span>
            </div>
            <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 items-center">
              <ScoreBar value={rate} />
              <span className="text-[11px] text-text-3 tabular-nums">
                {r.successes}/{r.repeats} runs
              </span>
            </div>
            <div className="flex items-center gap-2 text-[11px] text-text-3">
              <code className="font-mono text-accent-2 truncate">{r.id}</code>
              <span className="flex-1" />
              <span className="truncate">{formatPanelTime(r.createdAt)}</span>
            </div>
            {r.taskId ? (
              <div className="border-t border-line/70 pt-3 grid gap-3">
                <div className="flex flex-wrap items-center gap-2 text-[11px]">
                  <span className="text-text-3">{hostAppName()} task pack</span>
                  <code className="font-mono text-accent-2">{r.taskId}</code>
                  <span
                    className={`rounded-md border px-2 py-0.5 font-semibold ${
                      r.passed
                        ? "border-accent-2/40 bg-accent-2/10 text-accent-2"
                        : "border-danger/40 bg-danger/10 text-danger"
                    }`}
                  >
                    {r.passed ? "passed" : "failed"}
                  </span>
                </div>
                <div className="grid gap-3 md:grid-cols-2">
                  <div className="min-w-0">
                    <span className="block text-[10px] uppercase tracking-wider text-text-3 mb-2">
                      Verifier
                    </span>
                    <pre className="max-h-28 overflow-auto text-[11px] text-text-2 whitespace-pre-wrap break-words font-mono">
                      {r.verifyOutput || r.verifyError || "(no verifier output)"}
                    </pre>
                  </div>
                  <div className="min-w-0">
                    <span className="block text-[10px] uppercase tracking-wider text-text-3 mb-2">
                      Agent output
                    </span>
                    <pre className="max-h-28 overflow-auto text-[11px] text-text-2 whitespace-pre-wrap break-words font-mono">
                      {r.agentOutput || "(empty)"}
                    </pre>
                  </div>
                </div>
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

// ── model rollouts ─────────────────────────────────────────────────────────
