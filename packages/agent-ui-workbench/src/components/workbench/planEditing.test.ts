import { describe, expect, it } from "vitest";

import { deletePlanStep, movePlanStep, parseEditablePlan, updatePlanStep } from "./planEditing.js";

describe("planEditing", () => {
  it("parses checklist-style plan steps without losing surrounding text", () => {
    const draft = parseEditablePlan(
      "Plan:\n1. Inspect state\n- Add tests\n- [ ] Wire UI\nNotes stay visible",
    );

    expect(draft.intro).toBe("Plan:");
    expect(draft.steps.map((step) => step.text)).toEqual(["Inspect state", "Add tests", "Wire UI"]);
    expect(draft.outro).toBe("Notes stay visible");
  });

  it("edits, deletes, and reorders steps while keeping stable ids", () => {
    const draft = parseEditablePlan("- First\n- Second\n- Third");
    const updated = updatePlanStep(draft, draft.steps[1].id, "Middle");
    const deleted = deletePlanStep(updated, draft.steps[0].id);
    const moved = movePlanStep(deleted, draft.steps[2].id, -1);

    expect(moved.steps.map((step) => step.text)).toEqual(["Third", "Middle"]);
    expect(moved.steps.map((step) => step.id)).toEqual([draft.steps[2].id, draft.steps[1].id]);
  });
});
