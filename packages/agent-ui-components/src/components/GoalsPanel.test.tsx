// @vitest-environment happy-dom
import { renderToStaticMarkup } from "react-dom/server";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import GoalsPanel, { type GoalsPanelGoal } from "./GoalsPanel";

function goal(overrides: Partial<GoalsPanelGoal> = {}): GoalsPanelGoal {
  return {
    id: "G1",
    title: "Ship the launch",
    status: "in_progress",
    lastUpdateTurn: 0,
    note: undefined,
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
});

describe("GoalsPanel — row rendering with status", () => {
  it("renders one list item per goal with the status label", () => {
    const goals: GoalsPanelGoal[] = [
      goal({ id: "G1", title: "Ship the launch", status: "in_progress" }),
      goal({ id: "G2", title: "Wait for design", status: "blocked" }),
      goal({ id: "G3", title: "Cut the release", status: "done" }),
    ];
    const html = renderToStaticMarkup(<GoalsPanel goals={goals} />);
    expect(html).toContain('data-goal-id="G1"');
    expect(html).toContain('data-goal-id="G2"');
    expect(html).toContain('data-goal-id="G3"');
    expect(html).toContain('data-status="in_progress"');
    expect(html).toContain('data-status="blocked"');
    expect(html).toContain('data-status="done"');
    expect(html).toContain("Active");
    expect(html).toContain("Blocked");
    expect(html).toContain("Done");
  });

  it("renders different status dot classes per status", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel
        goals={[
          goal({ id: "G1", status: "in_progress" }),
          goal({ id: "G2", status: "blocked" }),
          goal({ id: "G3", status: "done" }),
        ]}
      />,
    );
    expect(html).toContain("var(--goal-status-active");
    expect(html).toContain("var(--goal-status-blocked");
    expect(html).toContain("var(--goal-status-done");
  });

  it("shows the note text when expanded and hides it when collapsed", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel goals={[goal({ note: "drafting the first 3 sections" })]} expandedId="G1" />,
    );
    expect(html).toContain("drafting the first 3 sections");
  });

  it("hides the note when no row is expanded", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel goals={[goal({ note: "should not render" })]} expandedId={null} />,
    );
    expect(html).not.toContain("should not render");
  });
});

describe("GoalsPanel — stale banner after 5 turns", () => {
  it("does NOT show the stale banner when currentTurn - lastUpdateTurn <= 5", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel goals={[goal({ lastUpdateTurn: 0 })]} turn={5} />,
    );
    expect(html).not.toContain("goal-stale-banner");
  });

  it("shows the stale banner when currentTurn - lastUpdateTurn > 5", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel goals={[goal({ lastUpdateTurn: 0 })]} turn={6} />,
    );
    expect(html).toContain('data-testid="goal-stale-banner"');
    expect(html).toContain("Mark done");
    expect(html).toContain("Still going");
    expect(html).toContain("Blocked");
  });

  it("only flags stale for in_progress goals, not done/blocked", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel
        goals={[
          goal({ id: "G1", status: "done", lastUpdateTurn: 0 }),
          goal({ id: "G2", status: "blocked", lastUpdateTurn: 0 }),
        ]}
        turn={20}
      />,
    );
    expect(html).not.toContain("goal-stale-banner");
  });

  it("invokes onStaleAction with the right action when a stale button is clicked", () => {
    const onStaleAction = vi.fn();
    render(
      <GoalsPanel
        goals={[goal({ id: "G1", lastUpdateTurn: 0, status: "in_progress" })]}
        turn={10}
        onStaleAction={onStaleAction}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Mark done" }));
    expect(onStaleAction).toHaveBeenCalledWith("G1", "mark_done");
  });
});

describe("GoalsPanel — blocked popover vs modal", () => {
  it("uses role=dialog with aria-label='Goal blocked' (NOT aria-modal)", () => {
    const onStaleAction = vi.fn(() => {});
    render(
      <GoalsPanel
        goals={[goal({ id: "G1", lastUpdateTurn: 0, status: "in_progress" })]}
        turn={10}
        onStaleAction={onStaleAction}
      />,
    );
    const blockButtons = screen.getAllByRole("button", { name: "Blocked" });
    // The first matching button is the "Blocked" status label for any goal
    // with status=blocked; in this single-goal test we expect exactly one.
    expect(blockButtons.length).toBe(1);
    fireEvent.click(blockButtons[0]);
    const dialog = screen.getByRole("dialog", { name: "Goal blocked" });
    expect(dialog).toBeTruthy();
    expect(dialog.getAttribute("aria-modal")).toBeNull();
  });

  it("does NOT introduce any element with aria-modal=true", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel goals={[goal({ lastUpdateTurn: 0 })]} turn={10} />,
    );
    expect(html).not.toContain('aria-modal="true"');
    expect(html).not.toContain("aria-modal='true'");
  });

  it("the Blocked popover is inline within the row's list item", () => {
    const { container } = render(
      <GoalsPanel
        goals={[goal({ id: "G1", lastUpdateTurn: 0, status: "in_progress" })]}
        turn={10}
        onStaleAction={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Blocked" }));
    const dialog = screen.getByRole("dialog", { name: "Goal blocked" });
    expect(container.querySelector('[role="dialog"]')).toBeTruthy();
    expect(dialog.parentElement?.tagName).toBe("LI");
  });
});

describe("GoalsPanel — inline expansion (not modal)", () => {
  it("expanded rows do NOT render role=dialog", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel goals={[goal({ note: "expanded note" })]} expandedId="G1" />,
    );
    expect(html).not.toContain('role="dialog"');
  });

  it("expanded note renders inline as a <p>, not a dialog", () => {
    const html = renderToStaticMarkup(
      <GoalsPanel goals={[goal({ note: "inline note text" })]} expandedId="G1" />,
    );
    expect(html).toContain("inline note text");
    expect(html).not.toContain('role="dialog"');
  });

  it("clicking the title toggles expandedId via onExpandedIdChange", () => {
    const onExpandedIdChange = vi.fn();
    render(
      <GoalsPanel
        goals={[goal({ title: "Toggle me" })]}
        expandedId={null}
        onExpandedIdChange={onExpandedIdChange}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Toggle me" }));
    expect(onExpandedIdChange).toHaveBeenCalledWith("G1");
  });
});
