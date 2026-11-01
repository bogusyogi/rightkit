import { create } from "zustand";

// Keep this shell namespace separate until the legacy ComposerIsland migration
// can safely unify composer state with the shared composerStore.
export const COMPOSER_MODE_STORAGE_KEY = "cr.shell.composer-mode.v1";
export const COMPOSER_MODES = ["read", "edit", "plan"] as const;
export type ComposerMode = (typeof COMPOSER_MODES)[number];

type ComposerModeState = {
  mode: ComposerMode;
  setMode: (mode: ComposerMode) => void;
};

function isComposerMode(value: unknown): value is ComposerMode {
  return typeof value === "string" && COMPOSER_MODES.includes(value as ComposerMode);
}

function readMode(): ComposerMode {
  if (typeof localStorage === "undefined") return "edit";
  try {
    const stored = localStorage.getItem(COMPOSER_MODE_STORAGE_KEY);
    return isComposerMode(stored) ? stored : "edit";
  } catch {
    return "edit";
  }
}

function persistMode(mode: ComposerMode) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(COMPOSER_MODE_STORAGE_KEY, mode);
  } catch {
    // Storage is best-effort in private or capacity-constrained webviews.
  }
}

export const useComposerModeStore = create<ComposerModeState>((set, get) => ({
  mode: readMode(),
  setMode(mode) {
    if (!isComposerMode(mode) || get().mode === mode) return;
    persistMode(mode);
    set({ mode });
  },
}));
