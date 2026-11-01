import { describe, expect, it } from "vitest";
import { BUILT_IN_ROLES } from "./roles.js";
import { BUILT_IN_STYLES } from "./styles.js";
import { BUILT_IN_TEMPLATES, TEMPLATE_CATEGORIES, type TemplateCategory } from "./templates.js";
import { compilePrompt, EMPTY_FIELDS } from "./compilePrompt.js";

describe("prompt template registry", () => {
  it("ports 108 built-in templates with valid categories and unique IDs", () => {
    expect(BUILT_IN_TEMPLATES).toHaveLength(108);
    expect(new Set(BUILT_IN_TEMPLATES.map((template) => template.id)).size).toBe(108);
    const categories = Object.keys(TEMPLATE_CATEGORIES) as TemplateCategory[];
    for (const template of BUILT_IN_TEMPLATES) {
      expect(template.builtIn).toBe(true);
      expect(categories).toContain(template.category);
      expect(template.recommendedRoles.every((id) => BUILT_IN_ROLES.some((role) => role.id === id))).toBe(true);
      expect(template.recommendedStyles.every((style) => BUILT_IN_STYLES.some((candidate) => candidate.id === style.id))).toBe(true);
    }
  });

  it("applies recommendations and compiles every built-in template", () => {
    const template = BUILT_IN_TEMPLATES.find((candidate) => candidate.id === "debug-root-cause")!;
    expect(template.recommendedRoles).toEqual(["debugger", "backend-eng"]);
    for (const candidate of BUILT_IN_TEMPLATES) {
      expect(compilePrompt({ ...EMPTY_FIELDS, ...candidate.fields, roleIds: candidate.recommendedRoles, styleSelections: candidate.recommendedStyles }, BUILT_IN_ROLES, BUILT_IN_STYLES)).toBeTruthy();
    }
  });
});
