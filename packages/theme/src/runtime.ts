import { resolveBrand, type BrandFile, type Mode } from "./brand.js";
import { modeVars } from "./css.js";
import { isHex } from "./color.js";
import { SUITE_ALIASES, aliasTokens, applyTokens, type StyleTarget } from "./tokens.js";

export type { StyleTarget } from "./tokens.js";

export interface ApplyBrandOptions {
  /**
   * Also write the unprefixed suite aliases (`--ink`, `--panel`, `--danger`, ...) inline. Off by
   * default: an app that owns those names would have them overwritten. Turn it on only when the app
   * relies on the flat names and does not set them itself.
   */
  applySuiteAliases?: boolean;
}

/**
 * Apply a brand at runtime (user-chosen accent, enterprise rebrand) by setting the same variables the generator emits, on
 * `root`'s inline style. Both modes are applied through the accent pair; the per-mode variables follow `mode`.
 * Call again with the resolved mode when the theme changes. Returns a function that removes everything it set.
 */
export function applyBrand(
  brand: BrandFile,
  mode: Mode,
  root: StyleTarget | null = typeof document === "undefined" ? null : document.documentElement,
  options: ApplyBrandOptions = {},
): () => void {
  if (!root) return () => {};
  const vars = modeVars(brand, mode);
  const light = modeVars(brand, "light");
  // The accent pair is mode-independent in shell.css; keep it complete whichever mode is applied.
  vars["--rk-brand"] = light["--rk-brand"];
  vars["--rk-brand-dark"] = resolveBrand(brand).palette.dark["brand-action"];
  // modeVars emits the wordmark font for light only; carry it into dark so applyBrand matches generateCss.
  if (light["--rk-wordmark-font"] !== undefined) vars["--rk-wordmark-font"] = light["--rk-wordmark-font"];
  const aliases = options.applySuiteAliases ? aliasTokens(SUITE_ALIASES) : {};
  applyTokens({ ...vars, ...aliases }, root);
  return () => {
    for (const k of Object.keys(vars)) root.style.removeProperty(k);
    for (const k of Object.keys(aliases)) root.style.removeProperty(k);
  };
}

/** Normalise a user-picked accent into a minimal BrandFile (for an appearance accent picker). */
export function brandFromAccent(name: string, accent: string): BrandFile {
  if (!isHex(accent)) throw new Error(`Invalid accent colour: ${accent}`);
  return { name, colors: { brand: accent } };
}

export interface ThemeAttributeOptions {
  /**
   * "Dark is the default" contract (CodeRight): the app's stylesheet treats a missing `data-theme` as dark, so a resolved
   * dark theme removes the attribute and only light is written. Default `false`: dark writes `data-theme="dark"`.
   */
  darkRemovesAttribute?: boolean;
}

/** The `data-theme` value for a resolved theme, or `null` when the attribute should be absent. */
export function themeAttributeValue(resolved: "light" | "dark", options: ThemeAttributeOptions = {}): string | null {
  return resolved === "dark" && options.darkRemovesAttribute ? null : resolved;
}

/** Minimal element surface for [`applyThemeAttribute`]. */
export interface ThemeAttributeTarget {
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
}

/** Set or remove `data-theme` on `root` for a resolved theme (see [`ThemeAttributeOptions`]). */
export function applyThemeAttribute(
  resolved: "light" | "dark",
  root: ThemeAttributeTarget | null = typeof document === "undefined" ? null : document.documentElement,
  options: ThemeAttributeOptions = {},
): void {
  if (!root) return;
  const value = themeAttributeValue(resolved, options);
  if (value === null) root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", value);
}

/** Options for [`themeBootScript`]: the consolidated first-paint contract shared with `@rightkit/react-shell`. */
export interface ThemeBootOptions extends ThemeAttributeOptions {
  /** Choice used when nothing valid is stored or storage throws. Default `"system"`. */
  defaultChoice?: "system" | "light" | "dark";
  /**
   * `true` (default): `system` resolves through `matchMedia` and `data-theme` is always written.
   * `false`: `system` removes `data-theme` and CSS decides from the OS query alone.
   */
  alwaysPaintDataTheme?: boolean;
}

/**
 * Inline script text for `<head>`: applies the stored light/dark/system choice to `data-theme` before first
 * paint so the window never flashes the wrong theme. Consolidated with `@rightkit/react-shell`'s
 * `themeBootScript`: identical first paint for the same storage key and options — stored choice wins,
 * throwing/unreadable storage falls back to `defaultChoice`, `system` resolves through `matchMedia`
 * (unless `alwaysPaintDataTheme: false` removes the attribute), and `darkRemovesAttribute` removes
 * the attribute for a resolved dark theme. Never throws. Generating the script is SSR-safe (a pure string).
 */
export function themeBootScript(storageKey: string, options: ThemeBootOptions = {}): string {
  const json = (v: unknown) => JSON.stringify(v).replace(/</g, "\\u003c").replace(/\u2028/g, "\\u2028").replace(/\u2029/g, "\\u2029");
  return (
    `(function(){try{var k=${json(storageKey)},c=${json(options.defaultChoice ?? "system")},p=${options.alwaysPaintDataTheme === false ? "false" : "true"},d=${options.darkRemovesAttribute === true ? "true" : "false"};` +
    `try{var v=localStorage.getItem(k);if(v==="light"||v==="dark"||v==="system")c=v}catch(_){}` +
    `var r=document.documentElement;` +
    `if(c==="system"){if(!p){r.removeAttribute("data-theme");return}` +
    `c=matchMedia("(prefers-color-scheme: dark)").matches?"dark":"light"}` +
    `if(c==="dark"&&d){r.removeAttribute("data-theme");return}r.setAttribute("data-theme",c)}catch(_){}})();`
  );
}

const PREFERS_DARK_QUERY = "(prefers-color-scheme: dark)";

function prefersDarkList(): MediaQueryList | null {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return null;
  try {
    return window.matchMedia(PREFERS_DARK_QUERY);
  } catch {
    return null;
  }
}

/** True when the OS asks for a dark colour scheme. `false` where there is no `window` or `matchMedia` (SSR, tests). */
export function prefersDark(): boolean {
  return prefersDarkList()?.matches ?? false;
}

/**
 * Call `cb(isDark)` whenever the OS colour scheme changes. Returns an unsubscribe function (safe to call more than once).
 * Does not call `cb` on subscribe: read `prefersDark()` for the current value. Where there is no `window` or `matchMedia`
 * (SSR), nothing is subscribed and the returned function does nothing.
 */
export function onPrefersDarkChange(cb: (dark: boolean) => void): () => void {
  const list = prefersDarkList();
  if (!list) return () => {};
  const handler = (e: MediaQueryListEvent) => cb(e.matches);
  if (typeof list.addEventListener === "function") {
    list.addEventListener("change", handler);
    return () => list.removeEventListener("change", handler);
  }
  // Safari before 14 only has the deprecated addListener/removeListener.
  list.addListener(handler);
  return () => list.removeListener(handler);
}
