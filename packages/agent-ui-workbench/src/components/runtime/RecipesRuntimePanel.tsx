import { EmptyState } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import RecipeParameterField from "./RecipeParameterField";
import RecipeScheduleDialog from "./RecipeScheduleDialog";
import { recipeVerdictClass } from "./recipeVerdictClass";
import type { RecipesRuntimePanelProps } from "./RecipesRuntimePanelProps";
import {
  formatIsoTime,
  parseRecipeParamsDraft,
  recipeParameterValue,
  recipeParamsDraftWithValue,
} from "./runtimeUtils";

export default function RecipesRuntimePanel({
  recipes,
  selectedRecipeId,
  recipeJsonDraft,
  recipeUrlDraft,
  recipeParamsDraft,
  recipeValidation,
  renderedRecipe,
  recipeRuns,
  pendingId,
  scheduleOpen,
  scheduleDraft,
  onSelectRecipe,
  onRecipeJsonDraft,
  onRecipeUrlDraft,
  onRecipeParamsDraft,
  onValidateDraft,
  onInstallDraft,
  onInstallFile,
  onInstallUrl,
  onOpenRunTarget,
  onRenderSelected,
  onRunSelected,
  onOpenSchedule,
  onScheduleOpen,
  onScheduleDraft,
  onScheduleSelected,
}: RecipesRuntimePanelProps) {
  const selectedRecipe = recipes.find((recipe) => recipe.id === selectedRecipeId) ?? null;
  const parameterValues = parseRecipeParamsDraft(recipeParamsDraft);
  return (
    <div className="relative flex-1 min-h-0 overflow-y-auto p-5 grid grid-cols-1 xl:grid-cols-[360px_minmax(0,1fr)] gap-4">
      <section className="rounded border border-line bg-panel overflow-hidden">
        <div className="px-4 py-3 border-b border-line">
          <h3 className="text-[13px] font-semibold text-text">Installed recipes</h3>
          <p className="text-[12px] text-text-3">{recipes.length} trusted workflow definitions</p>
        </div>
        <div className="divide-y divide-line">
          {recipes.length ? (
            recipes.map((recipe) => (
              <button
                type="button"
                className={`w-full text-left px-4 py-3 cursor-pointer ${
                  recipe.id === selectedRecipeId ? "bg-panel-2" : "bg-transparent hover:bg-panel-2"
                }`}
                key={recipe.id}
                onClick={() => onSelectRecipe(recipe.id)}
              >
                <div className="flex items-center justify-between gap-3">
                  <strong className="text-[13px] text-text truncate">{recipe.title}</strong>
                  <span
                    className={`px-2 py-0.5 rounded-full border text-[11px] ${recipeVerdictClass(
                      recipe.latestValidation?.verdict ?? "warn",
                    )}`}
                  >
                    {recipe.latestValidation?.verdict ?? "unvalidated"}
                  </span>
                </div>
                <p className="mt-1 text-[12px] text-text-3 truncate">{recipe.recipe.description}</p>
                <p className="mt-1 text-[11px] text-text-3 truncate">
                  {recipe.id} · {recipe.version} · {recipe.checksum.slice(0, 18)}
                </p>
              </button>
            ))
          ) : (
            <EmptyState title="No recipes installed">{"Install a recipe document to start building reusable runbooks."}</EmptyState>
          )}
        </div>
      </section>

      <section className="min-w-0 rounded border border-line bg-panel overflow-hidden">
        <div className="grid grid-cols-1 2xl:grid-cols-2 gap-0">
          <div className="border-b 2xl:border-b-0 2xl:border-r border-line">
            <div className="flex items-center justify-between gap-3 px-4 py-3 border-b border-line">
              <div className="min-w-0">
                <h3 className="text-[13px] font-semibold text-text">Recipe document</h3>
                <p className="text-[12px] text-text-3 truncate">
                  Validate and install trusted JSON/YAML.
                </p>
              </div>
              <div className="flex items-center gap-2 flex-wrap justify-end">
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded border border-line text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                  disabled={pendingId !== null}
                  onClick={onInstallFile}
                >
                  <Icon name="folder" size={13} />
                  {pendingId === "install-file" ? "Loading" : "Load file"}
                </button>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded border border-line text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                  disabled={pendingId !== null}
                  onClick={onValidateDraft}
                >
                  <Icon name="check" size={13} />
                  {pendingId === "validate" ? "Validating" : "Validate"}
                </button>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded border border-line text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                  disabled={pendingId !== null}
                  onClick={onInstallDraft}
                >
                  <Icon name="download" size={13} />
                  {pendingId === "install" ? "Installing" : "Install"}
                </button>
              </div>
            </div>
            <div className="flex items-center gap-2 border-b border-line bg-bg/40 px-4 py-3">
              <input
                aria-label="https://github.com/org/repo/blob/main/recipe.yaml"
                className="h-8 min-w-0 flex-1 rounded border border-line bg-bg px-3 text-[12px] text-text outline-none placeholder:text-text-3"
                value={recipeUrlDraft}
                onChange={(event) => onRecipeUrlDraft(event.target.value)}
                placeholder="https://github.com/org/repo/blob/main/recipe.yaml"
              />
              <button
                type="button"
                className="inline-flex items-center gap-2 h-8 px-3 rounded border border-line text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                disabled={pendingId !== null || !recipeUrlDraft.trim()}
                onClick={onInstallUrl}
              >
                <Icon name="download" size={13} />
                {pendingId === "install-url" ? "Installing URL" : "Install URL"}
              </button>
            </div>
            <textarea
              aria-label="Recipe JSON"
              className="h-[360px] w-full resize-y bg-bg p-3 font-mono text-[12px] text-text outline-none"
              value={recipeJsonDraft}
              onChange={(event) => onRecipeJsonDraft(event.target.value)}
              spellCheck={false}
            />
            {recipeValidation ? (
              <div className="border-t border-line px-4 py-3">
                <div className="flex items-center gap-2">
                  <span
                    className={`px-2 py-0.5 rounded-full border text-[11px] ${recipeVerdictClass(
                      recipeValidation.verdict,
                    )}`}
                  >
                    {recipeValidation.verdict}
                  </span>
                  <span className="text-[12px] text-text-3">{recipeValidation.createdAt}</span>
                </div>
                {recipeValidation.findings.length ? (
                  <div className="mt-2 flex flex-col gap-1">
                    {recipeValidation.findings.map((finding) => (
                      <span className="text-[12px] text-text-3" key={finding.id}>
                        {finding.severity}: {finding.message}
                      </span>
                    ))}
                  </div>
                ) : null}
              </div>
            ) : null}
          </div>

          <div className="min-w-0">
            <div className="px-4 py-3 border-b border-line">
              <h3 className="text-[13px] font-semibold text-text">
                {selectedRecipe ? selectedRecipe.title : "No recipe selected"}
              </h3>
              <p className="text-[12px] text-text-3 truncate">
                {selectedRecipe
                  ? `${selectedRecipe.recipe.requiredToolsets.join(", ") || "default toolset"} · ${selectedRecipe.recipe.parameters.length} parameter(s)`
                  : "Select or install a recipe to render and run."}
              </p>
            </div>
            <div className="p-4 flex flex-col gap-3">
              {selectedRecipe ? (
                <div className="grid grid-cols-1 lg:grid-cols-3 gap-2 rounded border border-line bg-bg p-3">
                  <div className="min-w-0">
                    <span className="block text-[11px] uppercase text-text-3">Toolset</span>
                    <code className="block truncate text-[12px] text-accent-2">
                      {selectedRecipe.recipe.settings.toolset ||
                        selectedRecipe.recipe.requiredToolsets[0] ||
                        "default"}
                    </code>
                  </div>
                  <div className="min-w-0">
                    <span className="block text-[11px] uppercase text-text-3">Skills</span>
                    <span className="block truncate text-[12px] text-text-2">
                      {selectedRecipe.recipe.skills.join(", ") || "none"}
                    </span>
                  </div>
                  <div className="min-w-0">
                    <span className="block text-[11px] uppercase text-text-3">Source</span>
                    <span className="block truncate text-[12px] text-text-2">
                      {selectedRecipe.source}
                      {selectedRecipe.trusted ? " · trusted" : ""}
                    </span>
                  </div>
                </div>
              ) : null}
              {selectedRecipe?.recipe.parameters.length ? (
                <div className="rounded border border-line bg-bg">
                  <div className="flex items-center justify-between gap-3 border-b border-line px-3 py-2">
                    <span className="text-[12px] font-semibold text-text-2">Parameters</span>
                    <span className="text-[11px] text-text-3">
                      {selectedRecipe.recipe.parameters.length} field
                      {selectedRecipe.recipe.parameters.length === 1 ? "" : "s"}
                    </span>
                  </div>
                  <div>
                    {selectedRecipe.recipe.parameters.map((parameter) => (
                      <RecipeParameterField
                        key={parameter.key}
                        parameter={parameter}
                        value={recipeParameterValue(parameter, parameterValues)}
                        onChange={(value) =>
                          onRecipeParamsDraft(
                            recipeParamsDraftWithValue(recipeParamsDraft, parameter.key, value),
                          )
                        }
                      />
                    ))}
                  </div>
                </div>
              ) : null}
              <label className="flex flex-col gap-1 text-[12px] text-text-3">
                Parameters JSON
                <textarea
                  className="h-[120px] resize-y rounded border border-line bg-bg p-3 font-mono text-[12px] text-text outline-none"
                  value={recipeParamsDraft}
                  onChange={(event) => onRecipeParamsDraft(event.target.value)}
                  spellCheck={false}
                />
              </label>
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded border border-line text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                  disabled={!selectedRecipeId || pendingId !== null}
                  onClick={onRenderSelected}
                >
                  <Icon name="activity" size={13} />
                  {pendingId?.startsWith("render:") ? "Rendering" : "Render"}
                </button>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded border border-line text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                  disabled={!selectedRecipeId || pendingId !== null}
                  onClick={onRunSelected}
                >
                  <Icon name="play" size={13} />
                  {pendingId?.startsWith("run:") ? "Running" : "Run"}
                </button>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded border border-line text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                  disabled={!selectedRecipeId || pendingId !== null}
                  onClick={onOpenSchedule}
                >
                  <Icon name="flag" size={13} />
                  Schedule
                </button>
              </div>
              {renderedRecipe ? (
                <div className="rounded border border-line bg-bg">
                  <div className="flex items-center justify-between gap-3 px-3 py-2 border-b border-line">
                    <span className="text-[12px] text-text-2 truncate">{renderedRecipe.title}</span>
                    <code className="text-[11px] text-accent-2">
                      {renderedRecipe.renderedPromptHash.slice(0, 18)}
                    </code>
                  </div>
                  <pre className="max-h-[180px] overflow-auto whitespace-pre-wrap p-3 text-[12px] text-text-2">
                    {renderedRecipe.renderedPrompt}
                  </pre>
                </div>
              ) : null}
              <div className="rounded border border-line overflow-hidden">
                <div className="px-3 py-2 border-b border-line text-[12px] text-text-2">
                  Run history
                </div>
                {recipeRuns.length ? (
                  <div className="divide-y divide-line">
                    {recipeRuns.map((run) => (
                      <div
                        className="grid grid-cols-[minmax(0,1fr)_76px_minmax(130px,0.7fr)_minmax(180px,0.8fr)] gap-3 px-3 py-2 text-[12px]"
                        key={run.id}
                      >
                        <div className="min-w-0">
                          <span className="block text-text truncate">{run.id}</span>
                          <code className="block truncate text-[11px] text-accent-2">
                            {run.renderedPromptHash}
                          </code>
                        </div>
                        <span className="text-text-3">{run.status}</span>
                        <div className="min-w-0">
                          <code className="block truncate text-[11px] text-accent-2">
                            {run.resultArtifactId || "no artifact"}
                          </code>
                          <span className="block truncate text-[11px] text-text-3">
                            {run.sessionId}
                          </span>
                        </div>
                        <div className="flex min-w-0 flex-col items-end gap-1">
                          <span className="text-text-3 text-right">
                            {formatIsoTime(run.createdAt)}
                          </span>
                          <div className="flex items-center gap-1">
                            <button
                              type="button"
                              className="h-6 rounded border border-line px-2 text-[11px] text-text-3 hover:text-text disabled:opacity-40"
                              onClick={() => onOpenRunTarget(run, "session")}
                            >
                              Session
                            </button>
                            <button
                              type="button"
                              className="h-6 rounded border border-line px-2 text-[11px] text-text-3 hover:text-text disabled:opacity-40"
                              disabled={!run.resultArtifactId}
                              onClick={() => onOpenRunTarget(run, "artifact")}
                            >
                              Artifact
                            </button>
                            <button
                              type="button"
                              className="h-6 rounded border border-line px-2 text-[11px] text-text-3 hover:text-text"
                              onClick={() => onOpenRunTarget(run, "evidence")}
                            >
                              Evidence
                            </button>
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                ) : (
                  <EmptyState title="No runs recorded">{"Run history for this recipe appears here after Render or Run executes it."}</EmptyState>
                )}
              </div>
            </div>
          </div>
        </div>
      </section>
      {scheduleOpen ? (
        <RecipeScheduleDialog
          draft={scheduleDraft}
          recipe={selectedRecipe}
          pending={pendingId?.startsWith("schedule:") ?? false}
          onClose={() => onScheduleOpen(false)}
          onDraft={onScheduleDraft}
          onSubmit={onScheduleSelected}
        />
      ) : null}
    </div>
  );
}
