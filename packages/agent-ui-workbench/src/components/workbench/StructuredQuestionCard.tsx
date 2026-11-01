import { getPlatform } from "@rightkit/platform-ui";
import { Button } from "@rightkit/react-shell/react";
import { useEffect, useMemo, useRef, useState } from "react";
import { registerShortcut } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";
import type { UserQuestion, UserQuestionOption } from "@rightkit/agent-ui-core/lib/types";

const OTHER_LABEL = "Other";

/** the host app question option with the optional Hermes preview seam retained. */
export type StructuredQuestionOption = UserQuestionOption & { preview?: string };
export type StructuredQuestion = Omit<UserQuestion, "options"> & {
  options: StructuredQuestionOption[];
};

export interface StructuredQuestionAnswerState {
  selected: string[];
  otherText: string;
}

export interface StructuredQuestionCardProps {
  /** Structured questions emitted by the daemon's `ask_user` tool. */
  questions: StructuredQuestion[];
  /** The existing the host app answer connector, keyed by header or question index. */
  onSubmit: (answers: Record<string, string | string[]>) => void;
  /** Resolves the question as cancelled/declined through the owning connector. */
  onCancel: () => void;
  /** Durable question id used by the hub for tracing and stale-response guards. */
  questionId?: string;
  /** Compatibility alias for callers that use the donor's tracing name. */
  dialogId?: string;
}

function questionKey(question: UserQuestion, index: number): string {
  return question.header || String(index);
}

function actionModifierLabel(): string {
  const platform = getPlatform();
  return platform === "macos" || platform === "ios" ? "⌘" : "Ctrl+";
}

/**
 * Projects the card's per-question state onto the daemon answer contract.
 * Single-select values are strings; multi-select values remain arrays. The
 * freeform `Other` value replaces the sentinel when text is present.
 */
export function buildStructuredQuestionAnswers(
  questions: StructuredQuestion[],
  state: StructuredQuestionAnswerState[],
): Record<string, string | string[]> {
  const answers: Record<string, string | string[]> = {};
  questions.forEach((question, index) => {
    const current = state[index] ?? { selected: [], otherText: "" };
    const custom = current.otherText.trim();
    const values = current.selected.map((value) =>
      value === OTHER_LABEL && custom ? custom : value,
    );
    answers[questionKey(question, index)] = question.multiSelect
      ? values
      : (values[0] ?? "");
  });
  return answers;
}

export function StructuredQuestionCard({
  questions,
  onSubmit,
  onCancel,
  questionId,
  dialogId,
}: StructuredQuestionCardProps) {
  const [state, setState] = useState<StructuredQuestionAnswerState[]>(() =>
    questions.map(() => ({ selected: [], otherText: "" })),
  );
  const onCancelRef = useRef(onCancel);
  onCancelRef.current = onCancel;
  const submitActionRef = useRef<() => void>(() => {});

  // An empty question set is not a usable card. Match the donor's safe
  // behavior by returning control to the owning connector rather than leaving
  // an empty blocking surface mounted.
  useEffect(() => {
    if (questions.length === 0) onCancelRef.current();
  }, [questions.length]);

  // Esc cancels; Cmd/Ctrl+Enter submits. Naked Enter remains available to the
  // freeform textarea, matching the donor's keyboard contract.
  useEffect(() => {
    const unregister = [
      registerShortcut({
        id: "question.cancel",
        chord: "esc",
        label: "Cancel question",
        hidden: true,
        ignoreExtraModifiers: true,
        allowDefaultPrevented: true,
        preventDefault: false,
        run: () => {
          onCancelRef.current();
          return true;
        },
      }),
      registerShortcut({
        id: "question.submit",
        chord: "mod+enter",
        label: "Submit answers",
        hidden: true,
        allowDefaultPrevented: true,
        run: () => {
          submitActionRef.current();
          return true;
        },
      }),
    ];
    return () => {
      for (const remove of unregister) remove();
    };
  }, []);

  const focusedOptions = useMemo(
    () =>
      questions.map((question, index) => {
        const selected = state[index]?.selected[0];
        return selected
          ? question.options.find((option) => option.label === selected) ?? null
          : null;
      }),
    [questions, state],
  );

  const submitDisabled = useMemo(
    () =>
      questions.some((_, index) => {
        const current = state[index];
        if (!current || current.selected.length === 0) return true;
        return current.selected.includes(OTHER_LABEL) && current.otherText.trim() === "";
      }),
    [questions, state],
  );

  if (questions.length === 0) return null;

  function handleSelect(questionIndex: number, label: string, multi: boolean) {
    setState((previous) => {
      const next = [...previous];
      const current = next[questionIndex] ?? { selected: [], otherText: "" };
      next[questionIndex] = multi
        ? {
            ...current,
            selected: current.selected.includes(label)
              ? current.selected.filter((value) => value !== label)
              : [...current.selected, label],
          }
        : { ...current, selected: [label] };
      return next;
    });
  }

  function handleOtherText(questionIndex: number, value: string) {
    setState((previous) => {
      const next = [...previous];
      next[questionIndex] = {
        ...(next[questionIndex] ?? { selected: [], otherText: "" }),
        otherText: value,
      };
      return next;
    });
  }

  function handleSubmit() {
    if (submitDisabled) return;
    onSubmit(buildStructuredQuestionAnswers(questions, state));
  }

  // Keep the stable global listener pointed at the current state closure.
  submitActionRef.current = handleSubmit;
  const traceId = dialogId ?? questionId;

  return (
    <section
      className="cr-msg-in self-start flex w-full max-w-[680px] flex-col rounded-xl border border-line bg-panel"
      data-dialog-id={traceId}
      data-question-id={questionId}
      role="alertdialog"
      aria-live="assertive"
      aria-label="Question from the agent"
    >
      <div className="h-0.5 shrink-0 rounded-t-xl bg-accent" aria-hidden="true" />
      <div className="flex flex-col gap-4 p-4">
        <header className="text-[11px] font-semibold uppercase tracking-widest text-accent">
          The agent is waiting for your answer
        </header>
        {questions.map((question, questionIndex) => {
          const current = state[questionIndex] ?? { selected: [], otherText: "" };
          const hasPreview = question.options.some((option) => Boolean(option.preview));
          return (
            <fieldset
              // biome-ignore lint/suspicious/noArrayIndexKey: protocol questions can repeat without identifiers.
              key={`${questionKey(question, questionIndex)}-${questionIndex}`}
              className="m-0 border-0 p-0"
            >
              <legend className="mb-1 text-[11px] font-semibold uppercase tracking-widest text-text-3">
                {question.header || `Q${questionIndex + 1}`}
              </legend>
              <p className="mb-2.5 font-mono text-[13px] italic leading-relaxed text-text">
                {question.question}
              </p>
              <div className="grid gap-4 md:grid-cols-2">
                <div className="flex flex-col gap-1.5">
                  {question.options.map((option) => (
                    <OptionRow
                      key={option.label}
                      questionIndex={questionIndex}
                      option={option}
                      multi={question.multiSelect}
                      selected={current.selected.includes(option.label)}
                      onSelect={() => handleSelect(questionIndex, option.label, question.multiSelect)}
                    />
                  ))}
                  <OtherRow
                    questionIndex={questionIndex}
                    multi={question.multiSelect}
                    selected={current.selected.includes(OTHER_LABEL)}
                    text={current.otherText}
                    onSelect={() => handleSelect(questionIndex, OTHER_LABEL, question.multiSelect)}
                    onTextChange={(value) => handleOtherText(questionIndex, value)}
                  />
                </div>
                {hasPreview ? <PreviewPane option={focusedOptions[questionIndex]} /> : null}
              </div>
            </fieldset>
          );
        })}
        <div className="flex items-center justify-end gap-3 border-t border-line pt-3">
          <Button variant="secondary" onClick={() => onCancel()}>
            <span aria-hidden="true" className="mr-1.5 rounded border border-line px-1.5 py-0.5 text-[10px] text-text-3">
              Esc
            </span>
            cancel
          </Button>
          <Button
            variant="primary"
            onClick={handleSubmit}
            disabled={submitDisabled}
            title={`Submit answers (${actionModifierLabel()}Enter)`}
          >
            <span aria-hidden="true" className="mr-1.5 text-[10px] opacity-85">
              {actionModifierLabel()}⏎
            </span>
            send
          </Button>
        </div>
      </div>
    </section>
  );
}

function OptionRow({
  questionIndex,
  option,
  multi,
  selected,
  onSelect,
}: {
  questionIndex: number;
  option: StructuredQuestionOption;
  multi: boolean;
  selected: boolean;
  onSelect: () => void;
}) {
  const id = `structured-question-${questionIndex}-${option.label}`;
  return (
    <label className="flex cursor-pointer items-start gap-2.5 rounded-md px-1 py-1.5" data-selected={selected}>
      <input
        id={id}
        type={multi ? "checkbox" : "radio"}
        name={`structured-question-${questionIndex}`}
        checked={selected}
        onChange={onSelect}
        className="sr-only"
      />
      <span aria-hidden="true" className="mt-0.5 w-4 shrink-0 font-mono text-[14px] leading-none text-text-3">
        {selected ? (multi ? "☑" : "◉") : multi ? "☐" : "○"}
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="font-mono text-[12px] text-text-2">{option.label}</span>
        {option.description ? <span className="font-mono text-[11px] leading-snug text-text-3">{option.description}</span> : null}
      </span>
    </label>
  );
}

function OtherRow({
  questionIndex,
  multi,
  selected,
  text,
  onSelect,
  onTextChange,
}: {
  questionIndex: number;
  multi: boolean;
  selected: boolean;
  text: string;
  onSelect: () => void;
  onTextChange: (value: string) => void;
}) {
  return (
    <label className="flex cursor-pointer items-start gap-2.5 rounded-md px-1 py-1.5" data-selected={selected}>
      <input
        type={multi ? "checkbox" : "radio"}
        name={`structured-question-${questionIndex}`}
        checked={selected}
        onChange={onSelect}
        className="sr-only"
      />
      <span aria-hidden="true" className="mt-0.5 w-4 shrink-0 font-mono text-[14px] leading-none text-text-3">
        {selected ? (multi ? "☑" : "◉") : multi ? "☐" : "○"}
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="font-mono text-[12px] text-text-2">{OTHER_LABEL}</span>
        {selected ? (
          <textarea
            className="mt-1 w-full resize-y rounded-md border border-line bg-bg px-2 py-1.5 font-mono text-[12px] text-text outline-none focus:border-accent"
            aria-label="Custom answer"
            value={text}
            onChange={(event) => onTextChange(event.target.value)}
            placeholder="type a custom answer…"
            rows={2}
            // biome-ignore lint/a11y/noAutofocus: selecting Other must move keyboard focus into its new field.
            autoFocus
          />
        ) : null}
      </span>
    </label>
  );
}

function PreviewPane({ option }: { option: StructuredQuestionOption | null }) {
  return (
    <div className="min-h-16 border border-line bg-bg p-2.5 font-mono text-[11px] text-text-2" data-testid="structured-question-preview">
      {option?.preview ? <pre className="m-0 whitespace-pre font-mono text-[11px]">{option.preview}</pre> : <span className="italic text-text-3">focus an option to preview</span>}
    </div>
  );
}

export default StructuredQuestionCard;
