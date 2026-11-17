import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { errorMessage } from "../../lib/engine.js";
import {
  RECIPE_ID_RULE,
  recipesApply,
  recipesDelete,
  recipesList,
  recipesSaveFromProject,
  recipesStatus,
  type RecipeStatusResult,
  type RecipeSummary,
} from "../../lib/media.js";

export type RecipeLibraryResult =
  | { kind: "applied"; name: string }
  | { kind: "saved"; id: string; name: string }
  | { kind: "deleted"; name: string };

export interface VideoRecipeLibraryOptions {
  dir: string;
  onApplied?: () => void;
  onBeforeSave?: () => Promise<void>;
  applyRecipe?: (id: string) => Promise<void>;
  saveRecipe?: (id: string, name: string) => Promise<void>;
  onComplete?: (result: RecipeLibraryResult) => void;
}

type Action = "apply" | "save" | "delete";

export function useVideoRecipeLibrary({
  dir, onApplied, onBeforeSave, applyRecipe, saveRecipe, onComplete,
}: VideoRecipeLibraryOptions) {
  const [library, setLibrary] = useState<RecipeSummary[]>([]);
  const [status, setStatus] = useState<RecipeStatusResult | null>(null);
  const [selected, setSelected] = useState("");
  const [saveId, setSaveId] = useState("");
  const [saveName, setSaveName] = useState("");
  const [armedId, setArmedId] = useState("");
  const [busy, setBusy] = useState<Action | null>(null);
  const [error, setError] = useState<string | null>(null);
  const scope = useRef<{ action: Action | null; read: number } | null>(null);

  useLayoutEffect(() => {
    const current = { action: null, read: 0 };
    scope.current = current;
    setStatus(null);
    setArmedId("");
    setBusy(null);
    setError(null);
    return () => { if (scope.current === current) scope.current = null; };
  }, [dir]);

  const refresh = useCallback(async () => {
    const current = scope.current;
    if (!current) return false;
    const read = ++current.read;
    try {
      const [list, nextStatus] = await Promise.all([recipesList(), recipesStatus(dir)]);
      if (scope.current !== current || current.read !== read) return false;
      setLibrary(list.recipes);
      setStatus(nextStatus);
      setSelected((previous) => list.recipes.some((row) => row.id === previous) ? previous : "");
      return true;
    } catch (e) {
      if (scope.current === current && current.read === read) setError(errorMessage(e));
      return false;
    }
  }, [dir]);

  useEffect(() => { void refresh(); }, [refresh]);

  const selectedRow = library.find((row) => row.id === selected);
  const invalidId = saveId !== "" && !RECIPE_ID_RULE.test(saveId);
  const existing = invalidId ? undefined : library.find((row) => row.id === saveId);
  const canSave = busy === null && saveId !== "" && !invalidId && saveName.trim() !== "";
  const applied = status?.ref ?? null;
  const appliedName = applied ? library.find((row) => row.id === applied.id)?.name ?? applied.id : "";

  const act = async (action: Action, operation: (isCurrent: () => boolean) => Promise<RecipeLibraryResult | undefined>) => {
    const current = scope.current;
    if (!current || current.action) return;
    current.action = action;
    setArmedId("");
    setBusy(action);
    setError(null);
    try {
      const isCurrent = () => scope.current === current;
      const result = await operation(isCurrent);
      if (!result || !isCurrent()) return;
      if (result.kind === "saved") {
        setSelected(result.id);
        setSaveId((previous) => previous === saveId ? "" : previous);
        setSaveName((previous) => previous === saveName ? "" : previous);
      } else if (result.kind === "deleted") {
        setSelected((previous) => previous === selected ? "" : previous);
      }
      onComplete?.(result);
      const refreshed = await refresh();
      if (refreshed && result.kind === "applied" && isCurrent()) onApplied?.();
    } catch (e) {
      if (scope.current === current) setError(errorMessage(e));
    } finally {
      current.action = null;
      if (scope.current === current) setBusy(null);
    }
  };

  const apply = () => {
    if (!selectedRow) return;
    return act("apply", async () => {
      if (applyRecipe) await applyRecipe(selectedRow.id);
      else await recipesApply(dir, selectedRow.id);
      return { kind: "applied", name: selectedRow.name };
    });
  };

  const save = () => {
    if (!canSave) return;
    const name = saveName.trim();
    return act("save", async (isCurrent) => {
      if (saveRecipe) await saveRecipe(saveId, name);
      else {
        await onBeforeSave?.();
        if (!isCurrent()) return;
        await recipesSaveFromProject({ dir, id: saveId, name });
      }
      return { kind: "saved", id: saveId, name };
    });
  };

  const remove = () => {
    if (!selectedRow || !scope.current || scope.current.action) return;
    if (armedId !== selectedRow.id) {
      setArmedId(selectedRow.id);
      return;
    }
    return act("delete", async () => {
      await recipesDelete(selectedRow.id);
      return { kind: "deleted", name: selectedRow.name };
    });
  };

  const disarm = () => setArmedId("");
  const disarmOnEscape = (event: KeyboardEvent) => { if (event.key === "Escape") disarm(); };

  return {
    library, status, applied, appliedName, selected, selectedRow, saveId, saveName, busy, error,
    armed: Boolean(selectedRow && armedId === selectedRow.id),
    invalidId, existing, canSave,
    select: (id: string) => { setArmedId(""); setSelected(id); },
    setSaveId, setSaveName, disarm, disarmOnEscape,
    apply, save, remove,
  };
}
