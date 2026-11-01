import { describe, expect, it } from "vitest";
import { BUILT_IN_ROLES, mergeRoles, validateCustomRole, type RoleDefinition } from "./roles";

describe("prompt template roles", () => {
  it("merges one role verbatim and multiple roles with an Oxford-comma opening", () => {
    expect(mergeRoles([], BUILT_IN_ROLES)).toBe("");
    expect(mergeRoles(["debugger"], BUILT_IN_ROLES)).toBe(BUILT_IN_ROLES.find((r) => r.id === "debugger")!.systemInstruction);
    expect(mergeRoles(["debugger", "backend-eng", "architect"], BUILT_IN_ROLES)).toContain("You are an Expert Debugger, Senior Backend Engineer, and Software Architect.");
  });

  it("skips unknown roles, deduplicates capabilities, and validates custom roles", () => {
    expect(mergeRoles(["debugger", "missing"], BUILT_IN_ROLES)).toBe(BUILT_IN_ROLES.find((r) => r.id === "debugger")!.systemInstruction);
    const roles: RoleDefinition[] = [
      { id: "a", label: "Role A", systemInstruction: "You are a Role A with expertise in testing.", builtIn: false },
      { id: "b", label: "Role B", systemInstruction: "You are a Role B with expertise in testing.", builtIn: false },
    ];
    expect((mergeRoles(["a", "b"], roles).match(/expertise in testing/gi) ?? []).length).toBe(1);
    expect(validateCustomRole({ label: "", systemInstruction: "x" }, BUILT_IN_ROLES).valid).toBe(false);
    expect(validateCustomRole({ label: "expert debugger", systemInstruction: "x" }, BUILT_IN_ROLES).valid).toBe(false);
    expect(validateCustomRole({ label: "Unique", systemInstruction: "You are unique." }, BUILT_IN_ROLES).valid).toBe(true);
  });
});
