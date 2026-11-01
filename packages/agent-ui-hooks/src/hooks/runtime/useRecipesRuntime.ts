import { useCallback, useState } from "react";
import {
  installRecipe,
  installRecipeFile,
  installRecipeUrl,
  listRecipes,
  listRecipeRuns,
  parseRecipeDeeplink,
  renderRecipe,
  runRecipe,
  scheduleRecipe,
  validateRecipe,
} from "@rightkit/agent-ui-core/lib/daemon";
import type {
  DaemonConfig,
  InstalledRecipe,
  Recipe,
  RecipeRun,
  RecipeScheduleDraft,
  RecipeValidationRun,
  RenderedRecipe,
} from "@rightkit/agent-ui-core/lib/types";
import { parseCommaList } from "@rightkit/agent-ui-core/lib/utils";

const DEFAULT_RECIPE_DRAFT = JSON.stringify(
  {
    id: "recipe-code-review",
    version: "1.0.0",
    title: "Code review runbook",
    description: "Review a focused change and produce actionable findings.",
    instructions: "Follow the repository instructions and prioritize correctness risks.",
    prompt: "Review {{scope}} and report findings with file references.",
    parameters: [
      {
        key: "scope",
        inputType: "string",
        requirement: "required",
        description: "Change, branch, file, or feature scope to review",
        default: "the current worktree",
      },
    ],
    requiredToolsets: ["coding-local"],
    requiredMcpServers: [],
    requiredPlugins: [],
    skills: [],
    settings: { toolset: "coding-local" },
    subRecipes: [],
    activities: [],
  },
  null,
  2,
);

function recipeDefaultParams(recipe: Recipe): Record<string, string> {
  const params: Record<string, string> = {};
  for (const param of recipe.parameters ?? []) {
    params[param.key] = param.value ?? param.default ?? "";
  }
  return params;
}

function defaultRecipeScheduleDraft(
  installed: InstalledRecipe | null,
  workspaceRoot: string,
): RecipeScheduleDraft {
  const recipe = installed?.recipe;
  const toolset =
    recipe?.settings?.toolset || recipe?.requiredToolsets?.find((value) => value.trim()) || "";
  return {
    id: "",
    cronExpr: "0 9 * * 1-5",
    timeZone: "UTC",
    workspaceRoot: recipe?.settings?.workspace || workspaceRoot,
    workdir: "",
    toolset,
    executionBackend: recipe?.settings?.executionBackend || "",
    skills: recipe?.skills?.join(", ") || "",
    deliveryKind: "desktop_notification",
    deliveryTarget: "",
    paused: false,
  };
}

interface UseRecipesRuntimeParams {
  config: DaemonConfig | null;
  runtimeWorkspace: string;
  onStatus: (status: string) => void;
  formatError: (error: unknown, fallback: string) => string;
  /** Cross-domain: refresh schedules after scheduling a recipe. */
  refreshSchedules: (workspaceOverride?: string) => Promise<unknown>;
}

export function useRecipesRuntime({
  config,
  runtimeWorkspace,
  onStatus,
  formatError,
  refreshSchedules,
}: UseRecipesRuntimeParams) {
  const [recipes, setRecipes] = useState<InstalledRecipe[]>([]);
  const [selectedRecipeId, setSelectedRecipeId] = useState<string | null>(null);
  const [recipeJsonDraft, setRecipeJsonDraft] = useState(DEFAULT_RECIPE_DRAFT);
  const [recipeUrlDraft, setRecipeUrlDraft] = useState("");
  const [recipeParamsDraft, setRecipeParamsDraft] = useState("{}");
  const [recipeValidation, setRecipeValidation] = useState<RecipeValidationRun | null>(null);
  const [renderedRecipe, setRenderedRecipe] = useState<RenderedRecipe | null>(null);
  const [recipeRuns, setRecipeRuns] = useState<RecipeRun[]>([]);
  const [recipeActionPending, setRecipeActionPending] = useState<string | null>(null);
  const [recipeScheduleOpen, setRecipeScheduleOpen] = useState(false);
  const [recipeScheduleDraft, setRecipeScheduleDraft] = useState<RecipeScheduleDraft>(() =>
    defaultRecipeScheduleDraft(null, runtimeWorkspace),
  );

  const loadRecipes = useCallback(async () => {
    if (!config) return;
    const recipeRows = await listRecipes(config);
    setRecipes(recipeRows);
    if (!selectedRecipeId && recipeRows.length > 0) {
      setSelectedRecipeId(recipeRows[0].id);
      setRecipeParamsDraft(JSON.stringify(recipeDefaultParams(recipeRows[0].recipe), null, 2));
      setRecipeScheduleDraft(defaultRecipeScheduleDraft(recipeRows[0], runtimeWorkspace));
    }
  }, [config, runtimeWorkspace, selectedRecipeId]);

  const selectRecipe = useCallback(
    async (id: string) => {
      setSelectedRecipeId(id);
      const recipe = recipes.find((row) => row.id === id);
      if (recipe) {
        setRecipeParamsDraft(JSON.stringify(recipeDefaultParams(recipe.recipe), null, 2));
        setRecipeScheduleDraft(defaultRecipeScheduleDraft(recipe, runtimeWorkspace));
        setRenderedRecipe(null);
        setRecipeValidation(recipe.latestValidation ?? null);
      }
      if (!config) return;
      try {
        setRecipeRuns(await listRecipeRuns(config, id));
      } catch (err) {
        onStatus(formatError(err, "Recipe run history failed"));
      }
    },
    [config, formatError, onStatus, recipes, runtimeWorkspace],
  );

  const patchRecipeScheduleDraft = useCallback((patch: Partial<RecipeScheduleDraft>) => {
    setRecipeScheduleDraft((draft) => ({ ...draft, ...patch }));
  }, []);

  const validateRecipeDraft = useCallback(async () => {
    if (!config) return;
    setRecipeActionPending("validate");
    try {
      const recipe = JSON.parse(recipeJsonDraft) as Recipe;
      const validation = await validateRecipe(config, recipe);
      setRecipeValidation(validation);
      onStatus(`Recipe validation: ${validation.verdict}`);
    } catch (err) {
      onStatus(formatError(err, "Recipe validation failed"));
    } finally {
      setRecipeActionPending(null);
    }
  }, [config, formatError, onStatus, recipeJsonDraft]);

  const installRecipeDraft = useCallback(async () => {
    if (!config) return;
    setRecipeActionPending("install");
    try {
      const recipe = JSON.parse(recipeJsonDraft) as Recipe;
      const installed = await installRecipe(config, { recipe, trusted: true, source: "inline" });
      const next = await listRecipes(config);
      setRecipes(next);
      setSelectedRecipeId(installed.id);
      setRecipeParamsDraft(JSON.stringify(recipeDefaultParams(installed.recipe), null, 2));
      setRecipeScheduleDraft(defaultRecipeScheduleDraft(installed, runtimeWorkspace));
      setRecipeValidation(installed.latestValidation ?? null);
      setRenderedRecipe(null);
      setRecipeRuns(await listRecipeRuns(config, installed.id));
      onStatus(`Recipe installed: ${installed.title}`);
    } catch (err) {
      onStatus(formatError(err, "Recipe install failed"));
    } finally {
      setRecipeActionPending(null);
    }
  }, [config, formatError, onStatus, recipeJsonDraft, runtimeWorkspace]);

  const installRecipeFromFile = useCallback(
    async (path: string) => {
      if (!config) return;
      setRecipeActionPending("install-file");
      try {
        const installed = await installRecipeFile(config, { path, trusted: true });
        const next = await listRecipes(config);
        setRecipes(next);
        setSelectedRecipeId(installed.id);
        setRecipeJsonDraft(JSON.stringify(installed.recipe, null, 2));
        setRecipeParamsDraft(JSON.stringify(recipeDefaultParams(installed.recipe), null, 2));
        setRecipeScheduleDraft(defaultRecipeScheduleDraft(installed, runtimeWorkspace));
        setRecipeValidation(installed.latestValidation ?? null);
        setRenderedRecipe(null);
        setRecipeRuns(await listRecipeRuns(config, installed.id));
        onStatus(`Recipe installed from file: ${installed.title}`);
      } catch (err) {
        onStatus(formatError(err, "Recipe file install failed"));
      } finally {
        setRecipeActionPending(null);
      }
    },
    [config, formatError, onStatus, runtimeWorkspace],
  );

  const installRecipeFromUrl = useCallback(async () => {
    if (!config) return;
    const url = recipeUrlDraft.trim();
    if (!url) return;
    setRecipeActionPending("install-url");
    try {
      const installed = await installRecipeUrl(config, { url, trusted: true });
      const next = await listRecipes(config);
      setRecipes(next);
      setSelectedRecipeId(installed.id);
      setRecipeJsonDraft(JSON.stringify(installed.recipe, null, 2));
      setRecipeParamsDraft(JSON.stringify(recipeDefaultParams(installed.recipe), null, 2));
      setRecipeScheduleDraft(defaultRecipeScheduleDraft(installed, runtimeWorkspace));
      setRecipeValidation(installed.latestValidation ?? null);
      setRenderedRecipe(null);
      setRecipeRuns(await listRecipeRuns(config, installed.id));
      setRecipeUrlDraft("");
      onStatus(`Recipe installed from GitHub: ${installed.title}`);
    } catch (err) {
      onStatus(formatError(err, "Recipe URL install failed"));
    } finally {
      setRecipeActionPending(null);
    }
  }, [config, formatError, onStatus, recipeUrlDraft, runtimeWorkspace]);

  const renderSelectedRecipe = useCallback(async () => {
    if (!config || !selectedRecipeId) return;
    setRecipeActionPending(`render:${selectedRecipeId}`);
    try {
      const params = JSON.parse(recipeParamsDraft || "{}") as Record<string, string>;
      const rendered = await renderRecipe(config, { recipeId: selectedRecipeId, params });
      setRenderedRecipe(rendered);
      onStatus(`Recipe rendered: ${rendered.title}`);
    } catch (err) {
      onStatus(formatError(err, "Recipe render failed"));
    } finally {
      setRecipeActionPending(null);
    }
  }, [config, formatError, onStatus, recipeParamsDraft, selectedRecipeId]);

  const runSelectedRecipe = useCallback(async () => {
    if (!config || !selectedRecipeId) return;
    setRecipeActionPending(`run:${selectedRecipeId}`);
    try {
      const params = JSON.parse(recipeParamsDraft || "{}") as Record<string, string>;
      const run = await runRecipe(config, selectedRecipeId, {
        params,
        workspaceRoot: runtimeWorkspace.trim() || undefined,
      });
      setRecipeRuns(await listRecipeRuns(config, selectedRecipeId));
      onStatus(`Recipe run ${run.status}: ${run.id}`);
    } catch (err) {
      onStatus(formatError(err, "Recipe run failed"));
    } finally {
      setRecipeActionPending(null);
    }
  }, [config, formatError, onStatus, recipeParamsDraft, runtimeWorkspace, selectedRecipeId]);

  const openRecipeDeeplink = useCallback(
    async (url: string) => {
      if (!config) return;
      setRecipeActionPending("deeplink");
      try {
        const parsed = await parseRecipeDeeplink(config, url);
        const next = await listRecipes(config);
        const installed = next.find((recipe) => recipe.id === parsed.recipeId);
        setRecipes(next);
        setSelectedRecipeId(parsed.recipeId);
        if (installed) {
          setRecipeJsonDraft(JSON.stringify(installed.recipe, null, 2));
          setRecipeValidation(installed.latestValidation ?? null);
          setRecipeScheduleDraft(defaultRecipeScheduleDraft(installed, runtimeWorkspace));
        }
        setRecipeParamsDraft(JSON.stringify(parsed.params, null, 2));
        setRenderedRecipe(null);

        if (parsed.action === "run") {
          const run = await runRecipe(config, parsed.recipeId, {
            params: parsed.params,
            workspaceRoot: runtimeWorkspace.trim() || undefined,
          });
          setRecipeRuns(await listRecipeRuns(config, parsed.recipeId));
          onStatus(`Recipe deeplink run ${run.status}: ${run.id}`);
          return;
        }

        if (parsed.action === "render") {
          const rendered = await renderRecipe(config, {
            recipeId: parsed.recipeId,
            params: parsed.params,
          });
          setRenderedRecipe(rendered);
          setRecipeRuns(await listRecipeRuns(config, parsed.recipeId));
          onStatus(`Recipe deeplink rendered: ${rendered.title}`);
          return;
        }

        setRecipeRuns(await listRecipeRuns(config, parsed.recipeId));
        if (parsed.action === "schedule") {
          setRecipeScheduleOpen(true);
        }
        onStatus(
          parsed.action === "schedule"
            ? "Recipe deeplink opened for scheduling."
            : `Recipe deeplink opened: ${parsed.action}`,
        );
      } catch (err) {
        onStatus(formatError(err, "Recipe deeplink failed"));
      } finally {
        setRecipeActionPending(null);
      }
    },
    [config, formatError, onStatus, runtimeWorkspace],
  );

  const openRecipeSchedule = useCallback(() => {
    const recipe = recipes.find((row) => row.id === selectedRecipeId) ?? null;
    setRecipeScheduleDraft(defaultRecipeScheduleDraft(recipe, runtimeWorkspace));
    setRecipeScheduleOpen(true);
  }, [recipes, runtimeWorkspace, selectedRecipeId]);

  const refreshRuns = useCallback(
    async (id: string) => {
      if (!config) return;
      setRecipeRuns(await listRecipeRuns(config, id));
    },
    [config],
  );

  const applyRenderedRecipe = useCallback((rendered: RenderedRecipe) => {
    setRenderedRecipe(rendered);
  }, []);

  const applyRecipeDeeplinkState = useCallback(
    async (parsed: { recipeId: string; params: Record<string, string> }) => {
      if (!config) return;
      const next = await listRecipes(config);
      const installed = next.find((recipe) => recipe.id === parsed.recipeId);
      setRecipes(next);
      setSelectedRecipeId(parsed.recipeId);
      if (installed) {
        setRecipeJsonDraft(JSON.stringify(installed.recipe, null, 2));
        setRecipeValidation(installed.latestValidation ?? null);
        setRecipeScheduleDraft(defaultRecipeScheduleDraft(installed, runtimeWorkspace));
      }
      setRecipeParamsDraft(JSON.stringify(parsed.params, null, 2));
      setRenderedRecipe(null);
    },
    [config, runtimeWorkspace],
  );

  const scheduleSelectedRecipe = useCallback(async () => {
    if (!config || !selectedRecipeId) return;
    setRecipeActionPending(`schedule:${selectedRecipeId}`);
    try {
      const params = JSON.parse(recipeParamsDraft || "{}") as Record<string, string>;
      const delivery =
        recipeScheduleDraft.deliveryKind === "none"
          ? []
          : [
              {
                kind: recipeScheduleDraft.deliveryKind,
                target: recipeScheduleDraft.deliveryTarget.trim() || undefined,
              },
            ];
      const scheduled = await scheduleRecipe(config, selectedRecipeId, {
        id: recipeScheduleDraft.id.trim() || undefined,
        cronExpr: recipeScheduleDraft.cronExpr.trim(),
        params,
        workspaceRoot:
          recipeScheduleDraft.workspaceRoot.trim() || runtimeWorkspace.trim() || undefined,
        timeZone: recipeScheduleDraft.timeZone.trim() || "UTC",
        skills: parseCommaList(recipeScheduleDraft.skills),
        workdir: recipeScheduleDraft.workdir.trim() || undefined,
        toolset: recipeScheduleDraft.toolset.trim() || undefined,
        executionBackend: recipeScheduleDraft.executionBackend.trim() || undefined,
        delivery,
        paused: recipeScheduleDraft.paused,
      });
      await refreshSchedules(
        scheduled.workspaceRoot || recipeScheduleDraft.workspaceRoot.trim() || runtimeWorkspace,
      );
      setRecipeScheduleOpen(false);
      onStatus(`Recipe scheduled: ${scheduled.id}`);
    } catch (err) {
      onStatus(formatError(err, "Recipe schedule failed"));
    } finally {
      setRecipeActionPending(null);
    }
  }, [
    config,
    formatError,
    onStatus,
    recipeParamsDraft,
    recipeScheduleDraft,
    refreshSchedules,
    runtimeWorkspace,
    selectedRecipeId,
  ]);

  return {
    recipes,
    selectedRecipeId,
    recipeJsonDraft,
    recipeUrlDraft,
    recipeParamsDraft,
    recipeValidation,
    renderedRecipe,
    recipeRuns,
    recipeActionPending,
    recipeScheduleOpen,
    recipeScheduleDraft,
    loadRecipes,
    selectRecipe,
    setRecipeJsonDraft,
    setRecipeUrlDraft,
    setRecipeParamsDraft,
    validateRecipeDraft,
    installRecipeDraft,
    installRecipeFromFile,
    installRecipeFromUrl,
    renderSelectedRecipe,
    runSelectedRecipe,
    openRecipeSchedule,
    setRecipeScheduleOpen,
    patchRecipeScheduleDraft,
    scheduleSelectedRecipe,
    applyRecipeDeeplinkState,
    refreshRuns,
    applyRenderedRecipe,
    openRecipeDeeplink,
  };
}

export { recipeDefaultParams, defaultRecipeScheduleDraft };
