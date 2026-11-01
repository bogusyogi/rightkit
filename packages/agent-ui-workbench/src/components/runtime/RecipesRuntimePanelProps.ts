import type {
  InstalledRecipe,
  RecipeRun,
  RecipeScheduleDraft,
  RecipeValidationRun,
  RenderedRecipe,
} from "@rightkit/agent-ui-core/lib/types";

export interface RecipesRuntimePanelProps {
  recipes: InstalledRecipe[];
  selectedRecipeId: string | null;
  recipeJsonDraft: string;
  recipeUrlDraft: string;
  recipeParamsDraft: string;
  recipeValidation: RecipeValidationRun | null;
  renderedRecipe: RenderedRecipe | null;
  recipeRuns: RecipeRun[];
  pendingId: string | null;
  scheduleOpen: boolean;
  scheduleDraft: RecipeScheduleDraft;
  onSelectRecipe: (id: string) => void;
  onRecipeJsonDraft: (draft: string) => void;
  onRecipeUrlDraft: (draft: string) => void;
  onRecipeParamsDraft: (draft: string) => void;
  onValidateDraft: () => void;
  onInstallDraft: () => void;
  onInstallFile: () => void;
  onInstallUrl: () => void;
  onOpenRunTarget: (run: RecipeRun, target: "session" | "artifact" | "evidence") => void;
  onRenderSelected: () => void;
  onRunSelected: () => void;
  onOpenSchedule: () => void;
  onScheduleOpen: (open: boolean) => void;
  onScheduleDraft: (patch: Partial<RecipeScheduleDraft>) => void;
  onScheduleSelected: () => void;
}
