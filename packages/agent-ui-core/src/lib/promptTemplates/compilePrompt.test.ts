import { describe, expect, it } from "vitest";
import { BUILT_IN_ROLES } from "./roles.js";
import { BUILT_IN_STYLES } from "./styles.js";
import { BUILT_IN_TEMPLATES } from "./templates.js";
import { compilePrompt, EMPTY_FIELDS, type ComposerFields } from "./compilePrompt.js";

describe("compilePrompt", () => {
  it("compiles v2 sections in deterministic order and omits blank fields", () => {
    const fields: ComposerFields = { ...EMPTY_FIELDS, roleIds: ["debugger"], task: " T ", scope: "S", constraints: "C", styleSelections: [{ id: "concise", level: 3 }], style: "St" };
    expect(compilePrompt(fields, BUILT_IN_ROLES, BUILT_IN_STYLES).split("\n\n").map((line) => line.split(":")[0])).toEqual(["**Role", "**Task", "**Scope", "**Constraints", "**Style"]);
    expect(compilePrompt({ ...EMPTY_FIELDS, task: "  " }, BUILT_IN_ROLES, BUILT_IN_STYLES)).toBe("");
    expect(compilePrompt({ ...EMPTY_FIELDS, task: "Do it", style: "Be concise" }, BUILT_IN_ROLES, BUILT_IN_STYLES)).toBe("**Task:** Do it\n\n**Style:** Be concise");
  });

  it("exports a complete template catalog", () => {
    expect(BUILT_IN_TEMPLATES).toHaveLength(108);
    for (const template of BUILT_IN_TEMPLATES) expect(compilePrompt({ ...EMPTY_FIELDS, ...template.fields, roleIds: template.recommendedRoles, styleSelections: template.recommendedStyles }, BUILT_IN_ROLES, BUILT_IN_STYLES)).toBeTruthy();
  });
});
