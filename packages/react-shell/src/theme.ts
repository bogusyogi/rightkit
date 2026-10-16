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
  root?: {
    setAttribute(n: string, v: string): void;
    removeAttribute(n: string): void;
    /** Optional; when present, applying a `data-theme` the element already has is a no-op. */
    getAttribute?(n: string): string | null;
  } | null;
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
    if (choice === "system" && !paint) {
      if (el.getAttribute && el.getAttribute("data-theme") === null) return;
      el.removeAttribute("data-theme");
      return;
    }
    const value = choice === "system" ? resolved(choice) : choice;
    // Already applied (for example by `applyStoredThemeSync` before first render): no second write.
    if (el.getAttribute && el.getAttribute("data-theme") === value) return;
    el.setAttribute("data-theme", value);
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

/**
 * Synchronous boot step: apply the stored theme to the root element before the first render, so a
 * saved dark theme never paints light first. Call it in the entry module before `createRoot(...).render`.
 * Same storage key, choices, system resolution and `data-theme` semantics as `createThemeStore`
 * (it uses one internally and disposes it, leaving no listener). The store created later by the
 * provider finds `data-theme` already correct and does not write it again. Never throws; with
 * unavailable or throwing storage it applies the default choice. Returns the applied theme, or
 * `null` when nothing was set (no root, or `system` with `alwaysPaintDataTheme: false`).
 */
export function applyStoredThemeSync(options: ThemeStoreOptions): ResolvedTheme | null {
  try {
    const store = createThemeStore(options);
    try {
      const choice = store.read();
      store.apply(choice);
      if (choice === "system" && options.alwaysPaintDataTheme === false) return null;
      return store.resolved(choice);
    } finally {
      store.dispose();
    }
  } catch {
    return null;
  }
}

/**
 * A tiny self-contained inline script for `index.html` (`<script>${themeBootScript({...})}</script>`
 * in `<head>`) that applies the stored theme before any bundle loads. It mirrors `applyStoredThemeSync`
 * (storage key, light/dark/system, `qaOverride`, `defaultChoice`, `alwaysPaintDataTheme`) and never throws.
 * A strict CSP must allow it with a `'sha256-...'` hash of the exact script text or a nonce on the tag.
 */
export function themeBootScript(options: Pick<ThemeStoreOptions, "storageKey" | "qaOverride" | "defaultChoice" | "alwaysPaintDataTheme">): string {
  const json = (v: unknown) => JSON.stringify(v).replace(/</g, "\\u003c").replace(/\u2028/g, "\\u2028").replace(/\u2029/g, "\\u2029");
  const qa = options.qaOverride === "light" || options.qaOverride === "dark" ? options.qaOverride : null;
  return (
    `(function(){try{var k=${json(options.storageKey)},c=${json(options.defaultChoice ?? "system")},p=${options.alwaysPaintDataTheme === false ? "false" : "true"},q=${json(qa)};` +
    `if(q){c=q}else{try{var v=localStorage.getItem(k);if(v==="light"||v==="dark"||v==="system")c=v}catch(_){}}` +
    `var r=document.documentElement;` +
    `if(c==="system"){if(!p){r.removeAttribute("data-theme");return}` +
    `c=matchMedia("(prefers-color-scheme: dark)").matches?"dark":"light"}` +
    `r.setAttribute("data-theme",c)}catch(_){}})();`
  );
}
