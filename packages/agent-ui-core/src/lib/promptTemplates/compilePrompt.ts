import { mergeRoles, type RoleDefinition } from "./roles.js";
import { mergeStyles, type StyleDefinition, type SelectedStyle } from "./styles.js";

// ── v2 interfaces ──

export interface ComposerFields {
  roleIds: string[];
  task: string;
  scope: string;
  constraints: string;
  styleSelections: SelectedStyle[];
  style: string; // free-text override (Additional Style Notes)
}

export const EMPTY_FIELDS: ComposerFields = {
  roleIds: [],
  task: "",
  scope: "",
  constraints: "",
  styleSelections: [],
  style: "",
};

// ── Compiler v2 ──

export function compilePrompt(
  fields: ComposerFields,
  allRoles: RoleDefinition[],
  allStyles: StyleDefinition[] = [],
): string {
  const sections: string[] = [];
  const roleText = mergeRoles(fields.roleIds, allRoles);
  if (roleText.trim()) sections.push(`**Role:** ${roleText.trim()}`);
  if (fields.task.trim()) sections.push(`**Task:** ${fields.task.trim()}`);
  if (fields.scope.trim()) sections.push(`**Scope:** ${fields.scope.trim()}`);
  if (fields.constraints.trim()) sections.push(`**Constraints:** ${fields.constraints.trim()}`);

  // Style: merge selections + free-text
  const selectionsText = mergeStyles(fields.styleSelections, allStyles);
  const freeText = fields.style.trim();
  const combined = [selectionsText, freeText].filter(Boolean).join(" ");
  if (combined) sections.push(`**Style:** ${combined}`);

  return sections.join("\n\n");
}

// ── Re-exports for convenience ──

export { BUILT_IN_TEMPLATES, TEMPLATE_CATEGORIES } from "./templates.js";
export type { PromptTemplate, TemplateCategory } from "./templates.js";
export { BUILT_IN_ROLES, mergeRoles, validateCustomRole } from "./roles.js";
export type { RoleDefinition } from "./roles.js";
export { BUILT_IN_STYLES, mergeStyles, validateCustomStyle } from "./styles.js";
export type { StyleDefinition, SelectedStyle } from "./styles.js";
