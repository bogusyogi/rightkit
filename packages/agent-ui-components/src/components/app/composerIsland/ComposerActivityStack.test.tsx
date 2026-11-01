// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import { ComposerActivityStack } from "./ComposerActivityStack.js";

const agents: SubAgentState[] = [
  {
    subSessionId: "agent-1",
    role: "reviewer",
    taskTitle: "Review composer UX",
    status: "running",
    startedAt: 1,
    tokensIn: 1200,
    tokensOut: 300,
    toolUses: 4,
  },
];

const baseProps = {
  agents,
  goal: {
    goalId: "goal-1",
    objective: "Ship the corrected composer",
    status: "active",
    startedAt: new Date(0).toISOString(),
    closedAt: null,
    dismissedAt: null,
    decompositionState: "usable",
    planRevision: 2,
    completionPercent: 50,
    now: "Implement controller",
    next: "Verify UI",
    steps: [
      {
        id: "step-1",
        label: "Implement",
        weight: 2,
        state: "completed",
        verificationState: "verified",
      },
    ],
    verificationState: "pending",
    budgetTokens: 1000,
    tokensUsed: 200,
    continuationCount: 0,
    noProgressCount: 0,
  },
  onPauseGoal: vi.fn(),
  onResumeGoal: vi.fn(),
  onEditGoal: vi.fn(),
  onClearGoal: vi.fn(),
};

afterEach(cleanup);

describe("ComposerActivityStack", () => {
  it("shows compact real-time summaries and expands agents inline", () => {
    const onOpenAgentsPanel = vi.fn();
    render(<ComposerActivityStack {...baseProps} onOpenAgentsPanel={onOpenAgentsPanel} />);

    expect(screen.getByText("1 active · 1 total")).toBeTruthy();
    expect(screen.getByText("1.5k tok")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Expand agent summary" }));
    expect(screen.getByText("Review composer UX")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open background tasks panel" }));
    expect(onOpenAgentsPanel).toHaveBeenCalledTimes(1);
  });

  it("expands, edits, pauses, and clears the active goal", () => {
    const onEditGoal = vi.fn();
    const onPauseGoal = vi.fn();
    const onClearGoal = vi.fn();
    render(
      <ComposerActivityStack
        {...baseProps}
        onEditGoal={onEditGoal}
        onPauseGoal={onPauseGoal}
        onClearGoal={onClearGoal}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Expand goal summary" }));
    expect(screen.getByText("Objective: Ship the corrected composer")).toBeTruthy();
    expect(screen.getByText(/Verification: pending/).getAttribute("aria-live")).toBe("polite");
    expect(screen.getByText(/Verification: pending/).getAttribute("aria-atomic")).toBe("true");
    expect(screen.getByText("Implement · completed · 2 · verified")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Pause goal" }));
    expect(onPauseGoal).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Edit goal" }));
    fireEvent.change(screen.getByLabelText("Edit active goal"), {
      target: { value: "Ship and verify the corrected composer" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(onEditGoal).toHaveBeenCalledWith("Ship and verify the corrected composer");
    fireEvent.click(screen.getByRole("button", { name: "Clear goal" }));
    expect(onClearGoal).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["paused", "Goal paused"],
    ["blocked", "Goal blocked"],
    ["achieved", "Goal achieved"],
  ] as const)("renders %s goal state truthfully", (status, label) => {
    render(<ComposerActivityStack {...baseProps} goal={{ ...baseProps.goal, status }} />);

    expect(screen.getByText(label)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Pause goal" })).toBeNull();
  });

  it("renders unknown completion as a plan-pending progress bar at zero", () => {
    render(<ComposerActivityStack {...baseProps} goal={{ ...baseProps.goal, completionPercent: null }} />);

    // RightKit's ProgressBar is determinate only; an unknown plan reads as zero with a "plan pending" name.
    const progress = screen.getByRole("progressbar", { name: "Goal completion: plan pending" });
    expect(progress.getAttribute("aria-valuenow")).toBe("0");
  });
});
