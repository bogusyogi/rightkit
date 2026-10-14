export type ThemeChoice = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export const THEME_CHOICES: readonly ThemeChoice[] = ["system", "light", "dark"];

export interface ThemeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** The slice of `MediaQueryList` the store listens to; injectable for tests and non-DOM hosts. */
export interface SystemThemeMedia {
  readonly matches: boolean;
  addEventListener(type: "change", listener: () => void): void;
  removeEventListener(type: "change", listener: () => void): void;
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
  /**
   * Choice used when nothing is stored (and for the QA override's fallback). Default `"system"`.
   * An app that wants a fixed first-run look sets `"light"` or `"dark"`.
   */
  defaultChoice?: ThemeChoice;
  /**
   * `true` (default): `system` resolves to light or dark and `data-theme` is always set to that value,
   * so CSS keyed on `data-theme` never has to re-implement the media query. `false` restores the
   * earlier behaviour: `system` removes `data-theme` and the OS decides in CSS alone.
   */
  alwaysPaintDataTheme?: boolean;
  /**
   * OS colour-scheme query. Default: `window.matchMedia("(prefers-color-scheme: dark)")` when present;
   * `null` disables the listener. While the choice is `system` the store re-applies on every OS change,
   * without React, and notifies subscribers.
   */
  systemMedia?: SystemThemeMedia | null;
}

function defaultStorage(): ThemeStorage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

function defaultSystemMedia(): SystemThemeMedia | null {
  try {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return null;
    return window.matchMedia("(prefers-color-scheme: dark)");
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

/**
 * Persisted light/dark/system choice applied as `data-theme` on the root element. The store owns
 * the OS listener, so `data-theme` follows the system without a mounted React tree.
 */
export function createThemeStore(options: ThemeStoreOptions) {
  const storage = () => (options.storage !== undefined ? options.storage : defaultStorage());
  const root = () =>
    options.root !== undefined ? options.root : typeof document === "undefined" ? null : document.documentElement;
  const media = options.systemMedia !== undefined ? options.systemMedia : defaultSystemMedia();
  const fallback: ThemeChoice = options.defaultChoice ?? "system";
  const paint = options.alwaysPaintDataTheme ?? true;
  const listeners = new Set<(choice: ThemeChoice) => void>();
  let current: ThemeChoice | null = null;

  function resolved(choice: ThemeChoice = read()): ResolvedTheme {
    return resolveTheme(choice, media?.matches ?? false);
  }

  function apply(choice: ThemeChoice): void {
    const el = root();
    if (!el) return;
    if (choice === "system" && !paint) el.removeAttribute("data-theme");
    else el.setAttribute("data-theme", choice === "system" ? resolved(choice) : choice);
  }

  function readStored(): ThemeChoice {
    if (options.qaOverride === "light" || options.qaOverride === "dark") return options.qaOverride;
    try {
      const value = storage()?.getItem(options.storageKey);
      if (value === "light" || value === "dark" || value === "system") return value;
    } catch {
      /* storage unavailable */
    }
    return fallback;
  }

  function read(): ThemeChoice {
    if (current === null) current = readStored();
    return current;
  }

  function save(choice: ThemeChoice): void {
    try {
      // Storing the fallback removes the key, so the default stays the app's own default.
      if (choice === fallback) storage()?.removeItem(options.storageKey);
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

  // Store-level OS listener: independent of React, re-applies only while the choice follows the system.
  const onSystemChange = () => {
    if (read() !== "system") return;
    apply("system");
    for (const listener of listeners) listener("system");
  };
  media?.addEventListener("change", onSystemChange);

  /** Detach the OS listener (hosts that create and discard stores, tests). */
  function dispose(): void {
    media?.removeEventListener("change", onSystemChange);
    listeners.clear();
  }

  return { read, resolved, apply, save, subscribe, dispose };
}

export type ThemeStore = ReturnType<typeof createThemeStore>;
