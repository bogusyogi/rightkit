import type { SkillOptimizationRun } from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "../shared.js";
import { skillTrainingStatusClass } from "./SkillShared.js";

export default function SkillTrainingRunList({
  runs,
  selectedRun,
  onSelectRun,
}: {
  runs: SkillOptimizationRun[];
  selectedRun: SkillOptimizationRun | null;
  onSelectRun: (id: string) => void;
}) {
  return (
    <div className="flex min-h-0 flex-col overflow-hidden rounded-xl border border-line">
      <div className="grid grid-cols-3 gap-2 border-b border-line bg-panel p-3">
        <SkillTrainingStat label="Runs" value={runs.length} />
        <SkillTrainingStat
          label="Accepted"
          value={runs.reduce(
            (total, run) => total + run.candidates.filter((c) => c.accepted).length,
            0,
          )}
        />
        <SkillTrainingStat
          label="Rejected"
          value={runs.reduce(
            (total, run) => total + run.candidates.filter((c) => !c.accepted).length,
            0,
          )}
        />
      </div>
      <div className="flex min-h-0 flex-col divide-y divide-line overflow-auto">
        {runs.length ? (
          runs.map((run) => (
            <button
              type="button"
              key={run.id}
              className={`grid grid-cols-[minmax(0,1fr)_auto] gap-3 border-0 px-4 py-3 text-left cursor-pointer ${
                selectedRun?.id === run.id
                  ? "bg-panel-2 text-text"
                  : "bg-transparent text-text-2 hover:bg-panel hover:text-text"
              }`}
              onClick={() => onSelectRun(run.id)}
            >
              <span className="min-w-0">
                <strong className="block truncate text-[13px] font-semibold">{run.name}</strong>
                <span className="block truncate text-[12px] text-text-3">
                  {run.target.skillId} · {run.target.objective}
                </span>
                <span className="block truncate text-[11px] font-mono text-text-3">
                  {run.bestVersionHash ?? "no best hash"}
                </span>
              </span>
              <span className="flex flex-col items-end gap-1">
                <code className={`text-[10px] ${skillTrainingStatusClass(run.status)}`}>
                  {run.status}
                </code>
                <code className="text-[10px] text-text-3">
                  epoch {run.currentEpoch} · {run.candidates.length} edits
                </code>
              </span>
            </button>
          ))
        ) : (
          <OfflinePanel
            title="No training runs."
            action="Create one below to start validation-gated skill optimization."
          />
        )}
      </div>
    </div>
  );
}

function SkillTrainingStat({ label, value }: { label: string; value: number }) {
  return (
    <div className="rounded-lg border border-line bg-bg p-2">
      <span className="block text-[10px] uppercase text-text-3">{label}</span>
      <strong className="mt-1 block text-[16px] text-text">{value}</strong>
    </div>
  );
}
