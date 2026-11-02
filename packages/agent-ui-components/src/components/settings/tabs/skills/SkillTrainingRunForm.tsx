import { Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { BenchmarkGateKind, SkillItem } from "@rightkit/agent-ui-core/lib/types";
import type { SkillTrainingPanelProps } from "./types.js";

export default function SkillTrainingRunForm({
  runDraft,
  skills,
  createPending,
  onRunDraft,
  onCreateRun,
}: {
  runDraft: SkillTrainingPanelProps["runDraft"];
  skills: SkillItem[];
  createPending: boolean;
  onRunDraft: SkillTrainingPanelProps["onRunDraft"];
  onCreateRun: SkillTrainingPanelProps["onCreateRun"];
}) {
  return (
    <div className="rounded-xl border border-line p-4">
      <div className="mb-3 flex items-center justify-between gap-3">
        <div className="min-w-0">
          <h4 className="text-[13px] font-semibold text-text">New training run</h4>
          <p className="truncate text-[12px] text-text-3">
            SkillOpt target, optimizer split, and first benchmark gate.
          </p>
        </div>
        <button
          type="button"
          className="inline-flex h-9 items-center gap-2 rounded-lg border-0 bg-accent px-4 text-[13px] font-semibold text-on-accent cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
          disabled={createPending}
          onClick={onCreateRun}
        >
          <Icon name="plus" /> {createPending ? "Creating" : "Create"}
        </button>
      </div>
      <div className="grid grid-cols-2 gap-3">
        <label className="flex flex-col gap-1 text-[12px] text-text-2">
          Name
          <input
            className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
            value={runDraft.name}
            onChange={(event) => onRunDraft({ ...runDraft, name: event.target.value })}
            placeholder="Review Tightening"
          />
        </label>
        <label className="flex flex-col gap-1 text-[12px] text-text-2">
          Skill
          <input
            className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
            value={runDraft.skillId}
            onChange={(event) => onRunDraft({ ...runDraft, skillId: event.target.value })}
            list="skill-training-skill-ids"
            placeholder="review"
          />
          <datalist id="skill-training-skill-ids">
            {skills.map((skill) => (
              <option key={skill.slug} value={skill.slug} />
            ))}
          </datalist>
        </label>
        <label className="col-span-2 flex flex-col gap-1 text-[12px] text-text-2">
          Objective
          <input
            className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
            value={runDraft.objective}
            onChange={(event) => onRunDraft({ ...runDraft, objective: event.target.value })}
            placeholder="Reduce missed regression comments"
          />
        </label>
        <label className="flex flex-col gap-1 text-[12px] text-text-2">
          Optimizer
          <input
            className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
            value={runDraft.optimizer}
            onChange={(event) => onRunDraft({ ...runDraft, optimizer: event.target.value })}
            placeholder="gpt-5"
          />
        </label>
        <label className="flex flex-col gap-1 text-[12px] text-text-2">
          Target model
          <input
            className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
            value={runDraft.targetModel}
            onChange={(event) => onRunDraft({ ...runDraft, targetModel: event.target.value })}
            placeholder="session/default"
          />
        </label>
        <label className="flex flex-col gap-1 text-[12px] text-text-2">
          Gate
          <input
            className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
            value={runDraft.gateName}
            onChange={(event) => onRunDraft({ ...runDraft, gateName: event.target.value })}
            placeholder="held-out validation"
          />
        </label>
        <div className="grid grid-cols-[minmax(0,1fr)_110px] gap-2">
          <label className="flex flex-col gap-1 text-[12px] text-text-2">
            Kind
            <Select value={runDraft.gateKind} options={[{ value: "hard", label: "hard" }, { value: "soft", label: "soft" }, { value: "mixed", label: "mixed" }]} onChange={(value) => onRunDraft({
                  ...runDraft,
                  gateKind: value as BenchmarkGateKind,
                })} label="Kind" className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none" />
          </label>
          <label className="flex flex-col gap-1 text-[12px] text-text-2">
            Threshold
            <input
              className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
              value={runDraft.gateThreshold}
              onChange={(event) => onRunDraft({ ...runDraft, gateThreshold: event.target.value })}
              placeholder="0.8"
            />
          </label>
        </div>
      </div>
    </div>
  );
}
