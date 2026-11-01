import { create } from "zustand";

// Keep this shell namespace separate until the legacy ComposerIsland migration
// can safely unify composer state with the shared composerStore.
export const COMPOSER_SCOPE_STORAGE_KEY = "cr.shell.composer-scope.v1";
export const COMPOSER_SCOPES = ["session", "workspace"] as const;
export type ComposerScope = (typeof COMPOSER_SCOPES)[number];

type ComposerScopeState = {
  scope: ComposerScope;
  setScope: (scope: ComposerScope) => void;
};

function isComposerScope(value: unknown): value is ComposerScope {
  return typeof value === "string" && COMPOSER_SCOPES.includes(value as ComposerScope);
}

function readScope(): ComposerScope {
  if (typeof localStorage === "undefined") return "session";
  try {
    const stored = localStorage.getItem(COMPOSER_SCOPE_STORAGE_KEY);
    return isComposerScope(stored) ? stored : "session";
  } catch {
    return "session";
  }
}

function persistScope(scope: ComposerScope) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(COMPOSER_SCOPE_STORAGE_KEY, scope);
  } catch {
    // Storage is best-effort in private or capacity-constrained webviews.
  }
}

export const useComposerScopeStore = create<ComposerScopeState>((set, get) => ({
  scope: readScope(),
  setScope(scope) {
    if (!isComposerScope(scope) || get().scope === scope) return;
    persistScope(scope);
    set({ scope });
  },
}));
