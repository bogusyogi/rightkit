export type ThemeChoice = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export const THEME_CHOICES: readonly ThemeChoice[] = ["system", "light", "dark"];

export interface ThemeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface ThemeStoreOptions {
  /** Per-app storage key, e.g. `myapp.theme`. */
  storageKey: string;
  /** Defaults to `window.localStorage` when available. */
  storage?: ThemeStorage | null;
  /** Element receiving `data-theme`; defaults to `document.documentElement`. */
  root?: { setAttribute(n: string, v: string): void; removeAttribute(n: string): void } | null;
  /** Browser-QA override: `?<qaParam>=light|dark` beats storage. */
  qaOverride?: ThemeChoice;
}

function defaultStorage(): ThemeStorage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

/** Resolve a choice against the OS preference. */
export function resolveTheme(choice: ThemeChoice, systemPrefersDark: boolean): ResolvedTheme {
  if (choice === "system") return systemPrefersDark ? "dark" : "light";
  return choice;
}

export function systemPrefersDark(): boolean {
  try {
    return typeof window !== "undefined" && window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch {
    return false;
  }
}

/** Persisted light/dark/system choice applied as `data-theme` on the root element. */
export function createThemeStore(options: ThemeStoreOptions) {
  const storage = () => (options.storage !== undefined ? options.storage : defaultStorage());
  const root = () =>
    options.root !== undefined ? options.root : typeof document === "undefined" ? null : document.documentElement;
  const listeners = new Set<(choice: ThemeChoice) => void>();
  let current: ThemeChoice | null = null;

  function apply(choice: ThemeChoice): void {
    const el = root();
    if (!el) return;
    if (choice === "system") el.removeAttribute("data-theme");
    else el.setAttribute("data-theme", choice);
  }

  function readStored(): ThemeChoice {
    if (options.qaOverride === "light" || options.qaOverride === "dark") return options.qaOverride;
    try {
      const value = storage()?.getItem(options.storageKey);
      if (value === "light" || value === "dark") return value;
    } catch {
      /* storage unavailable */
    }
    return "system";
  }

  function read(): ThemeChoice {
    if (current === null) current = readStored();
    return current;
  }

  function save(choice: ThemeChoice): void {
    try {
      if (choice === "system") storage()?.removeItem(options.storageKey);
      else storage()?.setItem(options.storageKey, choice);
    } catch {
      /* storage unavailable */
    }
    current = choice;
    apply(choice);
    for (const listener of listeners) listener(choice);
  }

  function subscribe(listener: (choice: ThemeChoice) => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  }

  return { read, apply, save, subscribe };
}

export type ThemeStore = ReturnType<typeof createThemeStore>;
