import SpecArtifactSummary from "./specs/SpecArtifactSummary.js";
import SpecConstitutionPanel from "./specs/SpecConstitutionPanel.js";
import SpecDetailPane from "./specs/SpecDetailPane.js";
import SpecListPane from "./specs/SpecListPane.js";
import SpecTaskEditor from "./specs/SpecTaskEditor.js";
import type { SpecConstitutionDraft, SpecDraft, SpecTaskDraft } from "./specs/types.js";
import type {
  ConstitutionDecision,
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

export default function SpecsRuntimePanel({
  specs,
  selectedSpecId,
  activeSessionSpecId,
  sessionSpecCompletion,
  clarifyResult,
  planResult,
  checklistResult,
  implementResult,
  envelope,
  analysis,
  trace,
  constitutionDecisions,
  pending,
  draft,
  taskDraft,
  constitutionDraft,
  onSelect,
  onDraft,
  onTaskDraft,
  onConstitutionDraft,
  onCreate,
  onAddTask,
  onAnalyze,
  onClarify,
  onPlan,
  onChecklist,
  onImplement,
  onRecordConstitutionDecision,
  onBindSessionSpec,
  onClearSessionSpec,
}: {
  specs: FeatureSpec[];
  selectedSpecId: string | null;
  activeSessionSpecId: string | null;
  sessionSpecCompletion: SessionSpecCompletion | null;
  clarifyResult: SpecClarifyResponse | null;
  planResult: SpecPlanResponse | null;
  checklistResult: SpecChecklistResponse | null;
  implementResult: SpecImplementResponse | null;
  envelope: SpecEnvelope | null;
  analysis: SpecAnalysisRun | null;
  trace: SpecTrace | null;
  constitutionDecisions: ConstitutionDecision[];
  pending: string | null;
  draft: SpecDraft;
  taskDraft: SpecTaskDraft;
  constitutionDraft: SpecConstitutionDraft;
  onSelect: (id: string) => void;
  onDraft: (patch: Partial<SpecDraft>) => void;
  onTaskDraft: (patch: Partial<SpecTaskDraft>) => void;
  onConstitutionDraft: (patch: Partial<SpecConstitutionDraft>) => void;
  onCreate: () => void;
  onAddTask: () => void;
  onAnalyze: () => void;
  onClarify: () => void;
  onPlan: () => void;
  onChecklist: () => void;
  onImplement: () => void;
  onRecordConstitutionDecision: () => void;
  onBindSessionSpec: () => void;
  onClearSessionSpec: () => void;
}) {
  const selected = envelope?.spec ?? specs.find((spec) => spec.id === selectedSpecId) ?? null;
  const selectedIsBound = Boolean(selected && activeSessionSpecId === selected.id);
  const hasSessionBinding = Boolean(activeSessionSpecId);
  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <SpecArtifactSummary specs={specs} envelope={envelope} analysis={analysis} trace={trace} />
      <div className="grid grid-cols-[minmax(260px,0.34fr)_minmax(0,1fr)] divide-x divide-line">
        <SpecListPane
          specs={specs}
          selectedSpecId={selectedSpecId}
          draft={draft}
          pending={pending}
          onSelect={onSelect}
          onDraft={onDraft}
          onCreate={onCreate}
        />
        <div className="min-w-0">
          {selected ? (
            <div className="flex flex-col divide-y divide-line">
              <SpecDetailPane
                spec={selected}
                selectedIsBound={selectedIsBound}
                hasSessionBinding={hasSessionBinding}
                pending={pending}
                clarifyResult={clarifyResult}
                planResult={planResult}
                checklistResult={checklistResult}
                implementResult={implementResult}
                sessionSpecCompletion={sessionSpecCompletion}
                envelope={envelope}
                trace={trace}
                onClarify={onClarify}
                onPlan={onPlan}
                onChecklist={onChecklist}
                onAnalyze={onAnalyze}
                onImplement={onImplement}
                onBindSessionSpec={onBindSessionSpec}
                onClearSessionSpec={onClearSessionSpec}
              />
              <div className="grid grid-cols-[minmax(0,1fr)_minmax(260px,0.45fr)] divide-x divide-line">
                <SpecTaskEditor
                  specId={selected.id}
                  taskDraft={taskDraft}
                  pending={pending}
                  tasks={envelope?.tasks ?? []}
                  onTaskDraft={onTaskDraft}
                  onAddTask={onAddTask}
                />
                <div className="min-w-0">
                  <SpecConstitutionPanel
                    spec={selected}
                    decisions={constitutionDecisions}
                    draft={constitutionDraft}
                    pending={pending === `constitution:${selected.id}`}
                    onDraft={onConstitutionDraft}
                    onRecord={onRecordConstitutionDecision}
                  />
                </div>
              </div>
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}
