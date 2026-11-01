// UI/shell store actions: the undo toast, the generic busy-wrapped runner, theme
// + font pickers, and the hidden-labels toggle.
import { setThemeChoice, applyFont, saveFontChoice } from "../../lib/theme";
import { setPref } from "../../lib/preferences";
import type { AppState } from "../types";
import { UNDO_TOAST_MS } from "../types";
import type { StoreSet, StoreGet } from "../mk";

type UiActions = Pick<
  AppState,
  "showUndo" | "clearUndo" | "run" | "pickTheme" | "pickFont" | "toggleLabelHidden"
>;

// Module-level timer for the undo toast (lifted out of App so any view can
// trigger the toast without prop-drilling).
let undoTimerId: ReturnType<typeof setTimeout> | undefined;

export function uiActions(set: StoreSet, get: StoreGet): UiActions {
  return {
    showUndo: (msg, refs, action) => {
      if (undoTimerId) clearTimeout(undoTimerId);
      set({ undo: { msg, refs, action, at: Date.now() } });
      undoTimerId = setTimeout(() => set({ undo: null }), UNDO_TOAST_MS);
    },
    clearUndo: () => {
      if (undoTimerId) clearTimeout(undoTimerId);
      set({ undo: null });
    },
    run: async (label, fn) => {
      const s = get();
      s.setErr(null);
      s.setBusy(label);
      try {
        await fn();
        await s.refresh();
        s.setReloadKey((k) => k + 1);
      } catch (e) {
        s.setErr(String(e));
      } finally {
        s.setBusy(null);
      }
    },
    pickTheme: (id) => {
      setThemeChoice(id);
      get().setThemeChoice(id);
    },
    pickFont: (id) => {
      applyFont(id);
      saveFontChoice(id);
      get().setFontChoice(id);
    },
    toggleLabelHidden: (id) => {
      get().setHiddenLabels((prev) => {
        const n = new Set(prev);
        if (n.has(id)) n.delete(id);
        else n.add(id);
        setPref("hiddenLabels", [...n]);
        return n;
      });
    },
  };
}
