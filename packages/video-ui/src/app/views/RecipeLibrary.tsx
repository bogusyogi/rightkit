import { Button, Field, Input, useToast } from "@rightkit/react-shell/react";
import { useVideoRecipeLibrary, type VideoRecipeLibraryOptions } from "../hooks/useVideoRecipeLibrary.js";
import { Icon } from "../ui/index.js";

const ID_RULE_TEXT = "Use 1–64 lowercase letters, digits or hyphens.";

/** Apply a saved recipe or save this project's recipe to the library. */
export function RecipeLibrary(props: Omit<VideoRecipeLibraryOptions, "onComplete">) {
  const toast = useToast();
  const {
    library, status, applied, appliedName, selected, selectedRow, saveId, saveName, busy, error,
    armed, invalidId, existing, canSave, select, setSaveId, setSaveName, disarm, disarmOnEscape, apply, save, remove,
  } = useVideoRecipeLibrary({
    ...props,
    onComplete: (result) => {
      switch (result.kind) {
        case "applied":
          toast(`Applied ${result.name} to this project`, { kind: "success" });
          break;
        case "saved":
          toast(`Saved ${result.name} as ${result.id}`, { kind: "success" });
          break;
        case "deleted":
          toast(`Deleted ${result.name}`, { kind: "success" });
          break;
      }
    },
  });
  const idError = invalidId ? ID_RULE_TEXT : null;
  const saveLabel = existing ? `Update ${existing.id} to v${existing.version + 1}` : "Save recipe";

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
            onChange={(e) => select(e.target.value)}
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
            onKeyDown={disarmOnEscape}
            onBlur={disarm}
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
