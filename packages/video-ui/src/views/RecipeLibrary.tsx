// Channel recipe — the app-wide recipe library applied to one project (recipes.* engine methods).
import { Button, Field, Input, useToast } from "@rightkit/react-shell/react";
import { Icon } from "../ui";
import { useCallback, useEffect, useState } from "react";
import { errorMessage } from "../lib/engine";
import {
  RECIPE_ID_RULE,
  type RecipeStatusResult,
  type RecipeSummary,
  recipesApply,
  recipesDelete,
  recipesList,
  recipesSaveFromProject,
  recipesStatus,
} from "../lib/media";


const ID_RULE_TEXT = "Use 1–64 lowercase letters, digits or hyphens.";

/** Panel on the Edit recipe screen: apply a saved recipe here, or save this project's recipe to the library. */
export function RecipeLibrary({
  dir,
  onApplied,
  onBeforeSave,
}: {
  dir: string;
  onApplied?: () => void;
  /** Writes what the Edit recipe screen shows into the project, so that is what gets saved. */
  onBeforeSave?: () => Promise<void>;
}) {
  const toast = useToast();
  const [library, setLibrary] = useState<RecipeSummary[]>([]);
  const [status, setStatus] = useState<RecipeStatusResult | null>(null);
  const [selected, setSelected] = useState("");
  const [saveId, setSaveId] = useState("");
  const [saveName, setSaveName] = useState("");
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const [list, st] = await Promise.all([recipesList(), recipesStatus(dir)]);
    setLibrary(list.recipes);
    setStatus(st);
    setSelected((cur) => (list.recipes.some((r) => r.id === cur) ? cur : ""));
  }, [dir]);

  useEffect(() => {
    refresh().catch((e) => setError(errorMessage(e)));
  }, [refresh]);

  // A pending delete never survives a change of selection.
  useEffect(() => {
    setArmed(false);
  }, [selected]);

  /** Runs one write, then the caller's reload; failures stay visible inline. */
  const act = async (tag: string, fn: () => Promise<void>) => {
    setBusy(tag);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const applied = status?.ref ?? null;
  const appliedName = applied ? (library.find((r) => r.id === applied.id)?.name ?? applied.id) : "";
  const selectedRow = library.find((r) => r.id === selected);
  const idError = saveId !== "" && !RECIPE_ID_RULE.test(saveId) ? ID_RULE_TEXT : null;
  const existing = idError === null ? library.find((r) => r.id === saveId) : undefined;
  const saveLabel = existing ? `Update ${existing.id} to v${existing.version + 1}` : "Save recipe";
  const canSave = busy === null && saveId !== "" && idError === null && saveName.trim() !== "";

  const statusText = !status
    ? "Checking the applied recipe…"
    : !applied
      ? "No saved recipe applied"
      : [
          `Using ${appliedName} v${applied.version}`,
          status.overridden ? "edited in this project" : null,
          status.update_available
            ? status.library_version != null
              ? `update available (v${status.library_version})`
              : "update available"
            : null,
        ]
          .filter(Boolean)
          .join(" · ");

  const apply = () =>
    void act("apply", async () => {
      await recipesApply(dir, selected);
      toast(`Applied ${selectedRow?.name ?? selected} to this project`, { kind: "success" });
      await refresh();
      onApplied?.();
    });

  const save = () =>
    void act("save", async () => {
      const name = saveName.trim();
      await onBeforeSave?.();
      await recipesSaveFromProject({ dir, id: saveId, name });
      toast(`Saved ${name} as ${saveId}`, { kind: "success" });
      setSelected(saveId);
      setSaveId("");
      setSaveName("");
      await refresh();
    });

  const remove = () => {
    if (!selectedRow) return;
    if (!armed) {
      setArmed(true);
      return;
    }
    setArmed(false);
    void act("delete", async () => {
      await recipesDelete(selectedRow.id);
      toast(`Deleted ${selectedRow.name}`, { kind: "success" });
      setSelected("");
      await refresh();
    });
  };

  return (
    <section className="recipe-library" data-testid="recipe-library" aria-label="Channel recipe">
      <h2 className="panel-title">Channel recipe</h2>
      {/* Always rendered with three lines reserved (recipe-status), so the select and buttons below never move. */}
      <p className="panel-note recipe-status" data-testid="recipe-status">
        {statusText}
        {status?.overridden && (
          <>
            <br />
            Applying replaces this project's recipe settings.
          </>
        )}
      </p>

      <div className="panel-stack">
        <Field label="Library recipe">{(ids) => (<select {...ids}
            className="rk-input cr-select"
            data-testid="recipe-select"
            value={selected}
            disabled={library.length === 0}
            onChange={(e) => setSelected(e.target.value)}
          >
            <option value="">{library.length === 0 ? "No saved recipes yet" : "Choose a recipe…"}</option>
            {library.map((r) => (
              <option key={r.id} value={r.id}>
                {`${r.name} (v${r.version})`}
              </option>
            ))}
          </select>)}</Field>
        <div className="panel-row">
          <Button
            variant="primary"
            size="sm"
            data-testid="recipe-apply"
            onClick={apply}
            disabled={(!selectedRow || busy !== null) || (busy === "apply")}
            aria-busy={(busy === "apply") || undefined}
          >{!(busy === "apply") && <Icon name="check" />}
            {busy === "apply" ? "Applying…" : "Apply to this project"}
          </Button>
          <Button
            variant={armed ? "danger" : "ghost"}
            size="sm"
            data-testid="recipe-delete"
            onClick={remove}
            onKeyDown={(e) => {
              if (e.key === "Escape") setArmed(false);
            }}
            onBlur={() => setArmed(false)}
            disabled={(!selectedRow || (busy !== null && busy !== "delete")) || (busy === "delete")}
            aria-busy={(busy === "delete") || undefined}
          >{!(busy === "delete") && <Icon name="trash-2" />}
            {busy === "delete" ? "Deleting…" : armed ? "Confirm delete" : "Delete"}
          </Button>
        </div>
      </div>

      <div className="panel-stack">
        <span className="cr-overline">Save this project's recipe as…</span>
        <Field label="Recipe id">{(ids) => (<Input {...ids}
            data-testid="recipe-save-id"
            value={saveId}
            placeholder="talking-head"
            aria-invalid={idError ? true : undefined}
            onChange={(e) => setSaveId(e.target.value)}
          />)}</Field>
        {idError && (
          <p role="alert" className="panel-error" data-testid="recipe-save-id-error">
            {idError}
          </p>
        )}
        <Field label="Name">{(ids) => (<Input {...ids}
            data-testid="recipe-save-name"
            value={saveName}
            placeholder="Talking head, 16:9 and 9:16"
            onChange={(e) => setSaveName(e.target.value)}
          />)}</Field>
        <div className="panel-row">
          <Button
            variant="secondary"
            size="sm"
            data-testid="recipe-save"
            onClick={save}
            disabled={(!canSave) || (busy === "save")}
            aria-busy={(busy === "save") || undefined}
          >
            {busy === "save" ? "Saving…" : saveLabel}
          </Button>
        </div>
      </div>

      {error && (
        <p role="alert" className="panel-error" data-testid="recipe-error">
          {error}
        </p>
      )}
    </section>
  );
}
