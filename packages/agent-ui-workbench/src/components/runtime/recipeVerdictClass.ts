import type { RecipeValidationRun } from "@rightkit/agent-ui-core/lib/types";

export function recipeVerdictClass(verdict: RecipeValidationRun["verdict"] | "warn") {
  if (verdict === "pass") return "border-accent-2/40 text-accent-2";
  if (verdict === "block") return "border-danger/40 text-danger";
  return "border-warning/40 text-warning";
}
