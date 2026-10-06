import type { ReactNode } from "react";

export type ThemeChoice = "system" | "light" | "dark";

export const THEME_CHOICES: readonly ThemeChoice[] = ["system", "light", "dark"];

export interface NavItem {
  id: string;
  label: string;
  icon?: ReactNode;
  /** Right-aligned tag, count, or status dot. */
  badge?: ReactNode;
  /** Extra Jump-to search terms. */
  keywords?: readonly string[];
}

export interface NavGroup {
  title: string;
  items: readonly NavItem[];
}

/** Jump-to filter: case-insensitive match on label or keywords; empty groups drop out. */
export function filterNavGroups(groups: readonly NavGroup[], query: string): NavGroup[] {
  const q = query.trim().toLowerCase();
  if (!q) return groups.map((group) => ({ ...group }));
  return groups
    .map((group) => ({
      ...group,
      items: group.items.filter(
        (item) =>
          item.label.toLowerCase().includes(q) ||
          (item.keywords ?? []).some((keyword) => keyword.toLowerCase().includes(q)),
      ),
    }))
    .filter((group) => group.items.length > 0);
}

export function firstNavItem(groups: readonly NavGroup[]): NavItem | undefined {
  return groups.find((group) => group.items.length > 0)?.items[0];
}

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

/** Persisted light/dark/system choice applied as `data-theme` on the root element. */
export function createThemeStore(options: ThemeStoreOptions) {
  const storage = () => (options.storage !== undefined ? options.storage : defaultStorage());
  const root = () =>
    options.root !== undefined
      ? options.root
      : typeof document === "undefined"
        ? null
        : document.documentElement;

  function apply(choice: ThemeChoice): void {
    const el = root();
    if (!el) return;
    if (choice === "system") el.removeAttribute("data-theme");
    else el.setAttribute("data-theme", choice);
  }

  function read(): ThemeChoice {
    if (options.qaOverride === "light" || options.qaOverride === "dark") return options.qaOverride;
    try {
      const value = storage()?.getItem(options.storageKey);
      if (value === "light" || value === "dark") return value;
    } catch {
      /* storage unavailable */
    }
    return "system";
  }

  function save(choice: ThemeChoice): void {
    try {
      if (choice === "system") storage()?.removeItem(options.storageKey);
      else storage()?.setItem(options.storageKey, choice);
    } catch {
      /* storage unavailable */
    }
    apply(choice);
  }

  return { read, apply, save };
}

export type ThemeStore = ReturnType<typeof createThemeStore>;
