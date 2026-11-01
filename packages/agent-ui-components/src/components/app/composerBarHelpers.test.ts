import { describe, expect, it } from "vitest";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import {
  goalStatusLabel,
  runLabel,
  skillScopeLabel,
  skillStatusLabel,
  skillToolLabel,
  skillTriggerLabel,
} from "./composerBarHelpers.js";

function skill(overrides: Partial<SkillItem> = {}): SkillItem {
  return {
    slug: "audit",
    name: "Audit",
    description: "Audit the workspace",
    trigger: "",
    enabled: true,
    category: "tool",
    priority: 0,
    tags: [],
    pathAffinity: [],
    path: "D:/Claude/tools/skills/audit/SKILL.md",
    directory: "D:/Claude/tools/skills/audit",
    kind: "local",
    valid: true,
    ...overrides,
  };
}

describe("composer bar helpers", () => {
  it("labels run targets and goal states without exposing internal values", () => {
    expect(runLabel("worktree")).toBe("Worktree");
    expect(runLabel("local")).toBe("Local");
    expect(goalStatusLabel("blocked")).toBe("Goal blocked");
    expect(goalStatusLabel(null)).toBe("Goal active");
  });

  it("prioritizes invalid, quarantined, retired, and disabled skill states", () => {
    expect(skillStatusLabel(skill({ valid: false }))).toBe("Invalid");
    expect(skillStatusLabel(skill({ lifecycle: "quarantined" }))).toBe("Quarantined");
    expect(skillStatusLabel(skill({ lifecycle: "retired" }))).toBe("Retired");
    expect(skillStatusLabel(skill({ active: false }))).toBe("Disabled");
    expect(skillStatusLabel(skill({ enabled: false }))).toBe("Disabled");
    expect(skillStatusLabel(skill())).toBe("Active");
  });

  it("summarizes triggers, scope, and tool constraints for the skills picker", () => {
    expect(skillTriggerLabel(skill({ disableModelInvocation: true }))).toBe("User-only");
    expect(skillTriggerLabel(skill({ trigger: "run a repo audit" }))).toBe("run a repo audit");
    expect(skillTriggerLabel(skill({ trigger: "  " }))).toBe("Model-invoked");

    expect(
      skillScopeLabel(
        skill({
          category: "tool",
          kind: "builtin",
          effects: { executionMode: "read-only" },
        }),
      ),
    ).toBe("tool · builtin · mode read-only");

    expect(
      skillToolLabel(
        skill({ effects: { allowedTools: ["read_file", "search", "git_diff", "shell"] } }),
      ),
    ).toBe("Allows read_file, search, git_diff");
    expect(skillToolLabel(skill({ effects: { disallowedTools: ["shell", "apply_patch"] } }))).toBe(
      "Blocks shell, apply_patch",
    );
    expect(
      skillToolLabel(
        skill({
          metadata: { coderight: { requiresToolsets: ["browser", "vision", "shell"] } },
        }),
      ),
    ).toBe("Needs browser, vision, shell");
    expect(skillToolLabel(skill())).toBe("No tool constraints");
  });
});
