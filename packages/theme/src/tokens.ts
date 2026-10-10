/** Minimal element surface so this stays testable without a DOM. */
export interface StyleTarget {
  style: {
    setProperty(name: string, value: string): void;
    removeProperty(name: string): string;
    /** Optional: lets `applyTokens` restore a previous inline value on undo. */
    getPropertyValue?(name: string): string;
  };
}

/** Unprefixed alias name -> the suite token it resolves to (`{ "--ink": "--rk-ink" }`). */
export type AliasMap = Readonly<Record<string, string>>;

/** A CSS custom property name, e.g. `--ink` or `--rk-ink`. */
const CUSTOM_PROPERTY = /^--[A-Za-z0-9_-]+$/;

export function assertCustomProperty(name: string): void {
  if (!CUSTOM_PROPERTY.test(name)) throw new Error(`Invalid CSS custom property name: ${JSON.stringify(name)}`);
}

/**
 * Set arbitrary CSS custom properties on `root`'s inline style and touch nothing else. Use it for
 * app palettes (theme preset x accent), font stacks, or any token an app owns. `undefined` values are
 * skipped. Returns an undo function that restores the previous inline value of each name it set.
 */
export function applyTokens(record: Readonly<Record<string, string | undefined>>, root: StyleTarget | null = typeof document === "undefined" ? null : document.documentElement): () => void {
  if (!root) return () => {};
  const previous = new Map<string, string>();
  for (const [name, value] of Object.entries(record)) {
    if (value === undefined) continue;
    assertCustomProperty(name);
    const before = root.style.getPropertyValue?.(name) ?? "";
    if (!previous.has(name)) previous.set(name, before);
    root.style.setProperty(name, value);
  }
  return () => {
    for (const [name, before] of previous) {
      if (before) root.style.setProperty(name, before);
      else root.style.removeProperty(name);
    }
  };
}

/**
 * Build a token record that makes unprefixed names resolve to suite tokens:
 * `aliasTokens({ "--ink": "--rk-ink" })` -> `{ "--ink": "var(--rk-ink)" }`. Pass the result to
 * `applyTokens` so an app keeps its own flat names without a stylesheet that could override them.
 */
export function aliasTokens(map: AliasMap): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [alias, target] of Object.entries(map)) {
    assertCustomProperty(alias);
    assertCustomProperty(target);
    out[alias] = `var(${target})`;
  }
  return out;
}

/**
 * The flat names `@rightkit/app-shell/suite-aliases.css` defines, as data. Kept identical to that file
 * (a test in this package compares them). Apply with `applyBrand(..., { applySuiteAliases: true })`
 * or `applyTokens(aliasTokens(SUITE_ALIASES))`.
 */
export const SUITE_ALIASES: AliasMap = {
  "--chrome": "--rk-chrome", "--plane": "--rk-plane", "--elevated": "--rk-elevated",
  "--win": "--rk-win", "--side": "--rk-side", "--bg": "--rk-bg", "--panel": "--rk-panel", "--sunk": "--rk-sunk",
  "--fill": "--rk-fill", "--fill-2": "--rk-fill-2", "--fill-3": "--rk-fill-3",
  "--line": "--rk-line", "--line-strong": "--rk-line-strong", "--hairline": "--rk-hairline",
  "--ink": "--rk-ink", "--ink-2": "--rk-ink-2", "--ink-3": "--rk-ink-3",
  "--accent": "--rk-accent", "--accent-soft": "--rk-accent-soft", "--accent-ink": "--rk-accent-ink", "--focus": "--rk-focus",
  "--ok": "--rk-ok", "--ok-dot": "--rk-ok-dot", "--ok-soft": "--rk-ok-soft",
  "--warn": "--rk-warn", "--warn-dot": "--rk-warn-dot", "--warn-soft": "--rk-warn-soft",
  "--bad": "--rk-bad", "--bad-soft": "--rk-bad-soft", "--danger": "--rk-danger",
  "--radius": "--rk-radius", "--card": "--rk-card", "--knob": "--rk-knob", "--sel": "--rk-sel",
  "--seg-on": "--rk-seg-on", "--ghost": "--rk-ghost", "--pop": "--rk-pop", "--scrim": "--rk-scrim",
  "--font-mono": "--rk-font-mono",
  // The CSS file falls back to `inherit` when the wordmark font is unset; an alias map cannot express a fallback.
  "--wordmark": "--rk-wordmark-font",
};

/** One app theme preset: a light or dark base palette plus optional accent overrides. */
export interface TokenPreset {
  id: string;
  mode: "light" | "dark";
  /** Base token values, e.g. `{ "--ink": "#101216", "--paper": "#F2F3F5" }`. */
  tokens: Readonly<Record<string, string>>;
  /** Accent overrides keyed by accent id; merged over `tokens` when selected. */
  accents?: Readonly<Record<string, Readonly<Record<string, string>>>>;
}

/** The token record for a preset with an optional accent. Unknown accent ids use the base palette. */
export function presetTokens(preset: TokenPreset, accentId?: string | null): Record<string, string> {
  const accent = accentId != null ? preset.accents?.[accentId] : undefined;
  return { ...preset.tokens, ...(accent ?? {}) };
}

/**
 * Font token hook: maps app font stacks onto the suite's font variables. `mono` -> `--rk-font-mono`
 * (the suite's monospace stack, also aliased as `--font-mono`) and `wordmark` -> `--rk-wordmark-font`.
 */
export function fontTokens(fonts: { mono?: string; wordmark?: string }): Record<string, string> {
  const out: Record<string, string> = {};
  if (fonts.mono) out["--rk-font-mono"] = fonts.mono;
  if (fonts.wordmark) out["--rk-wordmark-font"] = fonts.wordmark;
  return out;
}
