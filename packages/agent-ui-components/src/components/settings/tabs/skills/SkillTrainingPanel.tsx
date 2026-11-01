import SkillTrainingRunDetail from "./SkillTrainingRunDetail";
import SkillTrainingRunForm from "./SkillTrainingRunForm";
import SkillTrainingRunList from "@rightkit/agent-ui-settings/components/settings/tabs/skills/SkillTrainingRunList";
import type { SkillTrainingPanelProps } from "./types";

export function SkillTrainingPanel({
  runs,
  selectedRunId,
  runDraft,
  epochDraft,
  pendingId,
  skills,
  onSelectRun,
  onRunDraft,
  onEpochDraft,
  onCreateRun,
  onProposeCandidate,
  onExecuteEpoch,
}: SkillTrainingPanelProps) {
  const selectedRun = runs.find((r) => r.id === selectedRunId) ?? runs[0] ?? null;
  const executePending = selectedRun ? pendingId === `training:execute:${selectedRun.id}` : false;
  const proposePending = selectedRun ? pendingId === `training:propose:${selectedRun.id}` : false;
  const createPending = pendingId === "training:create";
  return (
    <div className="grid min-h-[620px] grid-cols-[minmax(280px,0.8fr)_minmax(0,1.2fr)] gap-4">
      <SkillTrainingRunList runs={runs} selectedRun={selectedRun} onSelectRun={onSelectRun} />
      <div className="flex min-w-0 flex-col gap-4 overflow-hidden">
        <SkillTrainingRunForm
          runDraft={runDraft}
          skills={skills}
          createPending={createPending}
          onRunDraft={onRunDraft}
          onCreateRun={onCreateRun}
        />
        <SkillTrainingRunDetail
          selectedRun={selectedRun}
          epochDraft={epochDraft}
          executePending={executePending}
          proposePending={proposePending}
          onEpochDraft={onEpochDraft}
          onProposeCandidate={onProposeCandidate}
          onExecuteEpoch={onExecuteEpoch}
        />
      </div>
    </div>
  );
}
