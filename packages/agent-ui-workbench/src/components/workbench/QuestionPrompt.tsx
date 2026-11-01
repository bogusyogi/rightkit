import { useEffect, useMemo, useState, type FormEvent } from "react";
import type { UserQuestion } from "@rightkit/agent-ui-core/lib/types";

const QUESTION_TITLE_ID = "question-prompt-title";

export default function QuestionPrompt({
  questions,
  onSubmit,
}: {
  questions: UserQuestion[];
  onSubmit: (answers: Record<string, string | string[]>) => void;
}) {
  const [selections, setSelections] = useState<Record<string, string[]>>({});
  // A new question event can reuse the same component instance. Do not carry
  // answers from the previous event into the next daemon question.
  const questionSignature = useMemo(
    () =>
      questions
        .map((question, index) =>
          [
            index,
            question.header,
            question.question,
            question.multiSelect,
            question.options.map((option) => `${option.label}\u0000${option.description}`).join("\u0001"),
          ].join("\u0002"),
        )
        .join("\u0003"),
    [questions],
  );

  // biome-ignore lint/correctness/useExhaustiveDependencies: protocol signature resets answers when question payload changes.
  useEffect(() => {
    setSelections({});
  }, [questionSignature]);

  // Selection state is indexed so duplicate display headers cannot couple two
  // questions. The answer key remains the existing daemon-facing header/index
  // contract below.
  function selectionKey(i: number): string {
    return String(i);
  }

  function answerKey(q: UserQuestion, i: number): string {
    return q.header || String(i);
  }

  function toggle(key: string, label: string, multi: boolean) {
    setSelections((prev) => {
      const cur = prev[key] ?? [];
      if (multi) {
        return cur.includes(label)
          ? { ...prev, [key]: cur.filter((l) => l !== label) }
          : { ...prev, [key]: [...cur, label] };
      }
      return { ...prev, [key]: [label] };
    });
  }

  const allAnswered = questions.every((_, i) => (selections[selectionKey(i)] ?? []).length > 0);

  function submit(event?: FormEvent<HTMLFormElement>) {
    event?.preventDefault();
    if (!allAnswered) return;
    const answers: Record<string, string | string[]> = {};
    questions.forEach((q, i) => {
      const selection = selections[selectionKey(i)] ?? [];
      const key = answerKey(q, i);
      answers[key] = q.multiSelect ? selection : (selection[0] ?? "");
    });
    onSubmit(answers);
  }

  // An empty question list is not an answer. It is malformed/terminal input,
  // so avoid presenting a misleading enabled submit action.
  if (questions.length === 0) return null;

  return (
    <article
      // The agent is waiting on this answer; announce it on arrival like the
      // permission and plan gates, or a screen-reader user never learns the run
      // has stopped for them.
      role="alertdialog"
      aria-live="assertive"
      aria-labelledby={QUESTION_TITLE_ID}
      className="cr-msg-in self-start flex flex-col gap-4 max-w-[80%] p-4 rounded-xl border border-line bg-panel"
    >
      <header className="flex items-center gap-2.5">
        <span
          aria-hidden="true"
          className="grid h-6 w-6 shrink-0 place-items-center rounded-full border border-line bg-panel-2 text-[13px] font-semibold text-text-2"
        >
          ?
        </span>
        <h3 id={QUESTION_TITLE_ID} className="text-[13px] font-semibold text-text">
          A question for you
        </h3>
      </header>
      <form className="flex flex-col gap-4" onSubmit={submit}>
        {questions.map((q, i) => {
          const key = selectionKey(i);
          const questionId = `question-prompt-question-${i}`;
          const instructionId = `question-prompt-instruction-${i}`;
          const picked = selections[key] ?? [];
          return (
            <fieldset
              className="flex min-w-0 flex-col gap-2"
              // biome-ignore lint/suspicious/noArrayIndexKey: protocol questions expose no stable identity.
              key={`${key}-${i}`}
              aria-describedby={q.multiSelect ? `${questionId} ${instructionId}` : questionId}
            >
              <legend className="text-[11px] uppercase tracking-widest text-text-3 font-semibold">
                {q.header || `Question ${i + 1}`}
              </legend>
              <p id={questionId} className="text-[13px] text-text leading-relaxed">
                {q.question}
              </p>
              <div className="flex flex-col gap-1.5">
                {q.options.map((opt) => {
                  const active = picked.includes(opt.label);
                  return (
                    <label
                      className={`flex min-h-9 cursor-pointer items-start gap-2 rounded-lg border px-3 py-2 text-left transition-colors ${
                        active
                          ? "border-accent bg-[color-mix(in_srgb,var(--color-accent)_12%,transparent)] text-text"
                          : "border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
                      }`}
                      key={opt.label}
                    >
                      <input
                        type={q.multiSelect ? "checkbox" : "radio"}
                        name={`question-prompt-${i}`}
                        value={opt.label}
                        checked={active}
                        onChange={() => toggle(key, opt.label, q.multiSelect)}
                        className="mt-0.5 h-6 w-6 shrink-0 accent-accent"
                      />
                      <span className="flex min-w-0 flex-col gap-0.5">
                        <span className="text-[13px] font-semibold">{opt.label}</span>
                        {opt.description ? (
                          <span className="text-[11px] text-text-3 leading-snug">
                            {opt.description}
                          </span>
                        ) : null}
                      </span>
                    </label>
                  );
                })}
              </div>
              {q.multiSelect ? (
                <span id={instructionId} className="text-[10px] text-text-3">
                  Select all that apply
                </span>
              ) : null}
            </fieldset>
          );
        })}
        <button
          type="submit"
          disabled={!allAnswered}
          className="self-start inline-flex items-center h-8 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:cursor-not-allowed disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3 disabled:opacity-100"
        >
          Answer &amp; continue
        </button>
      </form>
    </article>
  );
}
