import { resolveBrand, type BrandFile, type Mode } from "./brand.js";
import { modeVars } from "./css.js";
import { isHex } from "./color.js";

/** Minimal element surface so this stays testable without a DOM. */
export interface StyleTarget {
  style: { setProperty(name: string, value: string): void; removeProperty(name: string): string };
}

/**
 * Apply a brand at runtime (user-chosen accent, enterprise rebrand) by setting the same variables the generator emits, on
 * `root`'s inline style. Both modes are applied through the accent pair; the per-mode variables follow `mode`.
 * Call again with the resolved mode when the theme changes. Returns a function that removes everything it set.
 */
export function applyBrand(brand: BrandFile, mode: Mode, root: StyleTarget | null = typeof document === "undefined" ? null : document.documentElement): () => void {
  if (!root) return () => {};
  const vars = modeVars(brand, mode);
  const light = modeVars(brand, "light");
  // The accent pair is mode-independent in shell.css; keep it complete whichever mode is applied.
  vars["--rk-brand"] = light["--rk-brand"];
  vars["--rk-brand-dark"] = resolveBrand(brand).palette.dark["brand-action"];
  for (const [k, v] of Object.entries(vars)) root.style.setProperty(k, v);
  return () => {
    for (const k of Object.keys(vars)) root.style.removeProperty(k);
  };
}

/** Normalise a user-picked accent into a minimal BrandFile (for an appearance accent picker). */
export function brandFromAccent(name: string, accent: string): BrandFile {
  if (!isHex(accent)) throw new Error(`Invalid accent colour: ${accent}`);
  return { name, colors: { brand: accent } };
}

/**
 * Inline script text for `<head>`: applies the stored light/dark choice to `data-theme` before first paint so the window never
 * flashes the wrong theme. `system` (or nothing stored) leaves the attribute off and the OS decides. Pair with
 * `@rightkit/app-shell`'s theme store, which owns the same storage key afterwards.
 */
export function themeBootScript(storageKey: string): string {
  const key = JSON.stringify(storageKey);
  return `(function(){try{var v=localStorage.getItem(${key});if(v==="light"||v==="dark")document.documentElement.setAttribute("data-theme",v);}catch(e){}})();`;
}
