import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { getPlatform } from "@rightkit/platform-ui";
import {
  buildStructuredQuestionAnswers,
  type StructuredQuestion,
} from "./StructuredQuestionCard";
import StructuredQuestionCard from "./StructuredQuestionCard";
import { rightKitShortcuts } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";

// The card's keys are registry bindings; AppBody attaches the registry to the window in the app.
let detachShortcuts: () => void = () => {};
beforeEach(() => {
  detachShortcuts = rightKitShortcuts.attach();
});
afterEach(() => {
  detachShortcuts();
  cleanup();
});

const SAMPLE_QUESTIONS: StructuredQuestion[] = [
  {
    question: "Which approach should we take?",
    header: "Approach",
    multiSelect: false,
    options: [
      { label: "Option A", description: "first option" },
      { label: "Option B", description: "second option" },
    ],
  },
];

function renderCard(
  questions: StructuredQuestion[] = SAMPLE_QUESTIONS,
  onSubmit = vi.fn(),
  onCancel = vi.fn(),
) {
  return render(
    <StructuredQuestionCard
      questions={questions}
      onSubmit={onSubmit}
      onCancel={onCancel}
      questionId="q-42"
    />,
  );
}

function fireActionEnter() {
  const platform = getPlatform();
  const mac = platform === "macos" || platform === "ios";
  fireEvent.keyDown(window, { key: "Enter", metaKey: mac, ctrlKey: !mac });
}

describe("buildStructuredQuestionAnswers", () => {
  it("keys single-select answers by question header", () => {
    expect(
      buildStructuredQuestionAnswers(SAMPLE_QUESTIONS, [
        { selected: ["Option A"], otherText: "" },
      ]),
    ).toEqual({ Approach: "Option A" });
  });

  it("keeps multi-select answers as arrays", () => {
    const questions = [{ ...SAMPLE_QUESTIONS[0], multiSelect: true }];
    expect(
      buildStructuredQuestionAnswers(questions, [
        { selected: ["Option A", "Option B"], otherText: "" },
      ]),
    ).toEqual({ Approach: ["Option A", "Option B"] });
  });

  it("uses typed freeform text instead of the Other sentinel", () => {
    expect(
      buildStructuredQuestionAnswers(SAMPLE_QUESTIONS, [
        { selected: ["Other"], otherText: "a different framework" },
      ]),
    ).toEqual({ Approach: "a different framework" });
  });
});

describe("StructuredQuestionCard — render and selection", () => {
  it("renders single-select radios and the automatic Other option", () => {
    renderCard();
    expect(screen.getByRole("radio", { name: /Option A/i })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Option B/i })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Other/i })).toBeInTheDocument();
  });

  it("renders multi-select checkboxes", () => {
    renderCard([{ ...SAMPLE_QUESTIONS[0], multiSelect: true }]);
    expect(screen.getByRole("checkbox", { name: /Option A/i })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /Option B/i })).toBeInTheDocument();
  });

  it("reveals Other's textarea and blocks submit until it has text", () => {
    renderCard();
    fireEvent.click(screen.getByRole("radio", { name: /Other/i }));
    expect(screen.getByRole("textbox")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /send/i })).toBeDisabled();
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "custom answer" } });
    expect(screen.getByRole("button", { name: /send/i })).not.toBeDisabled();
  });

  it("renders an option preview when that option is selected", () => {
    renderCard([
      {
        ...SAMPLE_QUESTIONS[0],
        question: "Pick a layout",
        header: "Layout",
        options: [
          { label: "L1", description: "side-by-side", preview: "│ A │ B │" },
          { label: "L2", description: "stacked", preview: "── A ──\n── B ──" },
        ],
      },
    ]);
    fireEvent.click(screen.getByRole("radio", { name: /L1/i }));
    expect(screen.getByTestId("structured-question-preview")).toHaveTextContent("A");
  });

  it("exposes the question id for tracing", () => {
    renderCard();
    expect(document.querySelector('[data-dialog-id="q-42"]')).not.toBeNull();
    expect(document.querySelector('[data-question-id="q-42"]')).not.toBeNull();
  });
});

describe("StructuredQuestionCard — submit and cancel", () => {
  it("submits a single selection through the App answer connector", () => {
    const onSubmit = vi.fn();
    renderCard(SAMPLE_QUESTIONS, onSubmit);
    fireEvent.click(screen.getByRole("radio", { name: /Option A/i }));
    fireEvent.click(screen.getByRole("button", { name: /send/i }));
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit).toHaveBeenCalledWith({ Approach: "Option A" });
  });

  it("submits multi-select labels and typed Other text", () => {
    const onSubmit = vi.fn();
    renderCard([{ ...SAMPLE_QUESTIONS[0], multiSelect: true }], onSubmit);
    fireEvent.click(screen.getByRole("checkbox", { name: /Option A/i }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Other/i }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "custom" } });
    fireEvent.click(screen.getByRole("button", { name: /send/i }));
    expect(onSubmit).toHaveBeenCalledWith({ Approach: ["Option A", "custom"] });
  });

  it("Esc and the cancel button invoke onCancel", () => {
    const onCancel = vi.fn();
    renderCard(SAMPLE_QUESTIONS, vi.fn(), onCancel);
    fireEvent.keyDown(window, { key: "Escape" });
    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(onCancel).toHaveBeenCalledTimes(2);
  });

  it("Cmd/Ctrl+Enter submits only after the form is valid", () => {
    const onSubmit = vi.fn();
    renderCard(SAMPLE_QUESTIONS, onSubmit);
    fireActionEnter();
    expect(onSubmit).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("radio", { name: /Option A/i }));
    fireActionEnter();
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });

  it("naked Enter does not submit", () => {
    const onSubmit = vi.fn();
    renderCard(SAMPLE_QUESTIONS, onSubmit);
    fireEvent.click(screen.getByRole("radio", { name: /Option A/i }));
    fireEvent.keyDown(window, { key: "Enter" });
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("auto-cancels a degenerate empty question set", () => {
    const onCancel = vi.fn();
    renderCard([], vi.fn(), onCancel);
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("renders all questions in a multi-question prompt", () => {
    renderCard([
      { ...SAMPLE_QUESTIONS[0], question: "first?", header: "Q1" },
      { ...SAMPLE_QUESTIONS[0], question: "second?", header: "Q2" },
      { ...SAMPLE_QUESTIONS[0], question: "third?", header: "Q3" },
    ]);
    expect(screen.getByText("first?")).toBeInTheDocument();
    expect(screen.getByText("second?")).toBeInTheDocument();
    expect(screen.getByText("third?")).toBeInTheDocument();
  });
});
