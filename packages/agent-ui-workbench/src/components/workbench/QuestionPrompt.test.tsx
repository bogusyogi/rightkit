import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import QuestionPrompt from "./QuestionPrompt.js";
import type { UserQuestion } from "@rightkit/agent-ui-core/lib/types";

const questions: UserQuestion[] = [
  {
    header: "Approach",
    question: "Which approach should we take?",
    multiSelect: false,
    options: [
      { label: "Option A", description: "first option" },
      { label: "Option B", description: "second option" },
    ],
  },
  {
    header: "Checks",
    question: "Which checks should run?",
    multiSelect: true,
    options: [
      { label: "Unit", description: "fast checks" },
      { label: "Integration", description: "full checks" },
    ],
  },
];

afterEach(() => cleanup());

describe("QuestionPrompt", () => {
  it("exposes native radio and checkbox semantics and names the alert dialog", () => {
    render(<QuestionPrompt questions={questions} onSubmit={() => {}} />);

    expect(screen.getByRole("alertdialog", { name: "A question for you" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Option A first option/i })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /Unit fast checks/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Answer & continue/i })).toBeDisabled();
  });

  it("preserves the daemon answer map for single and multi-select questions", () => {
    const onSubmit = vi.fn();
    render(<QuestionPrompt questions={questions} onSubmit={onSubmit} />);

    fireEvent.click(screen.getByRole("radio", { name: /Option B/i }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Unit/i }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Integration/i }));
    fireEvent.click(screen.getByRole("button", { name: /Answer & continue/i }));

    expect(onSubmit).toHaveBeenCalledWith({
      Approach: "Option B",
      Checks: ["Unit", "Integration"],
    });
  });

  it("does not submit an empty question payload", () => {
    const onSubmit = vi.fn();
    render(<QuestionPrompt questions={[]} onSubmit={onSubmit} />);

    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("clears choices when the pending question shape changes", () => {
    const onSubmit = vi.fn();
    const { rerender } = render(<QuestionPrompt questions={questions} onSubmit={onSubmit} />);

    fireEvent.click(screen.getByRole("radio", { name: /Option A/i }));
    const nextQuestions = questions.map((question) =>
      question.header === "Approach"
        ? { ...question, question: "Which path should we take?" }
        : question,
    );
    rerender(<QuestionPrompt questions={nextQuestions} onSubmit={onSubmit} />);

    expect(screen.getByRole("radio", { name: /Option A/i })).not.toBeChecked();
    expect(screen.getByRole("button", { name: /Answer & continue/i })).toBeDisabled();
  });
});
