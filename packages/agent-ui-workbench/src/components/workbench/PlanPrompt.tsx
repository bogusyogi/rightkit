import { useEffect, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  deletePlanStep,
  movePlanStep,
  parseEditablePlan,
  updatePlanStep,
  type EditablePlanDraft,
} from "./planEditing";

export default function PlanPrompt({
  plan,
  onDecision,
}: {
  plan: string;
  onDecision: (approved: boolean) => void;
}) {
  const [draft, setDraft] = useState<EditablePlanDraft>(() => parseEditablePlan(plan));

  useEffect(() => {
    setDraft(parseEditablePlan(plan));
  }, [plan]);

  return (
    <article
      // Like the permission card, a plan gate blocks the run until answered,
      // so its arrival has to be announced rather than silently rendered.
      role="alertdialog"
      aria-live="assertive"
      aria-label="Plan approval"
      className="cr-msg-in self-start flex flex-col gap-4 max-w-[80%] p-4 rounded-xl border border-line bg-panel"
    >
      <header className="flex items-center gap-2.5">
        <span
          aria-hidden="true"
          className="grid h-6 w-6 shrink-0 place-items-center rounded-full border border-accent-2/40 bg-accent-2/10 text-accent-2"
        >
          <Icon name="tasks" size={13} />
        </span>
        <h3 className="text-[13px] font-semibold text-text">Plan ready for review</h3>
      </header>
      {draft.intro ? (
        <p className="text-[13px] text-text-2 leading-relaxed whitespace-pre-wrap break-words">
          {draft.intro}
        </p>
      ) : null}
      <ol className="flex flex-col gap-2">
        {draft.steps.map((step, index) => (
          <li key={step.id} className="grid grid-cols-[20px_minmax(0,1fr)_auto] items-start gap-2">
            <span className="mt-2 h-4 w-4 rounded border border-line-2 bg-panel-2" />
            <textarea
              value={step.text}
              onChange={(event) =>
                setDraft((current) => updatePlanStep(current, step.id, event.target.value))
              }
              rows={Math.max(1, Math.min(4, Math.ceil(step.text.length / 64)))}
              aria-label={`Plan step ${index + 1}`}
              className="min-h-9 resize-y rounded-lg border border-line bg-panel px-3 py-2 text-[13px] leading-relaxed text-text outline-none focus:border-accent"
            />
            <div className="flex items-center gap-1">
              <button
                type="button"
                disabled={index === 0}
                onClick={() => setDraft((current) => movePlanStep(current, step.id, -1))}
                className="grid h-8 w-8 place-items-center rounded-md border border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text disabled:cursor-not-allowed disabled:opacity-40"
                aria-label={`Move step ${index + 1} up`}
                title="Move up"
              >
                <span className="rotate-180">
                  <Icon name="chevron" size={13} />
                </span>
              </button>
              <button
                type="button"
                disabled={index === draft.steps.length - 1}
                onClick={() => setDraft((current) => movePlanStep(current, step.id, 1))}
                className="grid h-8 w-8 place-items-center rounded-md border border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text disabled:cursor-not-allowed disabled:opacity-40"
                aria-label={`Move step ${index + 1} down`}
                title="Move down"
              >
                <Icon name="chevron" size={13} />
              </button>
              <button
                type="button"
                onClick={() => setDraft((current) => deletePlanStep(current, step.id))}
                className="grid h-8 w-8 place-items-center rounded-md border border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
                aria-label={`Delete step ${index + 1}`}
                title="Delete"
              >
                <Icon name="trash" size={13} />
              </button>
            </div>
          </li>
        ))}
      </ol>
      {draft.outro ? (
        <p className="text-[12px] text-text-3 leading-relaxed whitespace-pre-wrap break-words">
          {draft.outro}
        </p>
      ) : null}
      <div className="flex items-center gap-2">
        <button
          type="button"
          className="inline-flex items-center h-8 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90"
          onClick={() => onDecision(true)}
        >
          Approve &amp; proceed
        </button>
        <button
          type="button"
          className="inline-flex items-center h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text"
          onClick={() => onDecision(false)}
        >
          Keep planning
        </button>
      </div>
    </article>
  );
}
