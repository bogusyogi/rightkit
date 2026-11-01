import { applyBodyFont, createThemeStore } from "@rightkit/react-shell";
import { applyBrand, applyTokens, prefersDark, type BrandFile } from "@rightkit/theme";
import { hostAppName } from "../appName";

export type LegacyThemeChoice = "auto" | "ember" | "ember-light";
export type ShellThemeChoice = "system" | "light" | "dark";

// RightKit uses --ink for text. the host app historical --ink token was
// chrome, so product surfaces use --hr-ink through this adapter.
export const RIGHTKIT_BRAND: BrandFile = {
  name: hostAppName(),
  code: "heardright",
  colors: { brand: "#D9390D", "brand-action": "#D9390D", "brand-ink": "#FFFFFF" },
  dark: { brand: "#FF5630", "brand-action": "#FF5630", "brand-ink": "#1B1714" },
  tokens: {
    light: {
      chrome: "#E4DDD2", plane: "#EFE9DF", elevated: "#FCFBF8", panel: "#FCFBF8", sunk: "#EFE9DF",
      ink: "#211B16", "ink-2": "#564C41", "ink-3": "#6F6355", line: "#CDC1B0", "line-strong": "#B7A996", hairline: "#CDC1B0",
      ok: "#2F7D43", warn: "#A85B00", bad: "#B83225",
    },
    dark: {
      chrome: "#12100F", plane: "#211B18", elevated: "#2D2521", panel: "#2D2521", sunk: "#211B18",
      ink: "#F3EEEA", "ink-2": "#AAA7A5", "ink-3": "#979492", line: "#4E4036", "line-strong": "#675448", hairline: "#4E4036",
      ok: "#64C878", warn: "#FFA640", bad: "#C04A3D",
    },
  },
  wordmark: "'Tanker', 'Hanken Grotesk', system-ui, sans-serif",
};

export const DEFAULT_DARK: LegacyThemeChoice = "ember";
export const DEFAULT_LIGHT: LegacyThemeChoice = "ember-light";
export const DEFAULT_FONT = "system";

export function shellThemeFromLegacy(choice: string | undefined): ShellThemeChoice {
  return choice === "ember-light" ? "light" : choice === "auto" ? "system" : "dark";
}

export function legacyThemeFromShell(choice: ShellThemeChoice): LegacyThemeChoice {
  return choice === "light" ? DEFAULT_LIGHT : choice === "system" ? "auto" : DEFAULT_DARK;
}

function resolvedMode(choice: string): "light" | "dark" {
  if (choice === "ember-light" || choice === "light") return "light";
  if (choice === "auto" || choice === "system") {
    return prefersDark() ? "dark" : "light";
  }
  return "dark";
}

export function applyTheme(choice: string): void {
  const mode = resolvedMode(choice);
  const dark = mode === "dark";
  const r = document.documentElement;
  applyBrand(RIGHTKIT_BRAND, mode, r);
  r.dataset.theme = mode;
  r.dataset.themeMode = mode;
  const values: Record<string, string> = {
    // --ink/--ink-2 had only their dark values (index.css), so the wordmark and Settings title/tabs went pale in light.
    "--ink": dark ? "#F3EEEA" : "#211B16", "--ink-2": dark ? "#AAA7A5" : "#564C41", "--ink-3": dark ? "#979492" : "#6F6355",
    "--hr-ink": dark ? "#12100F" : "#E4DDD2", "--sidebar": dark ? "#12100F" : "#E4DDD2", "--canvas": dark ? "#211B18" : "#EFE9DF", "--paper": dark ? "#F3EEEA" : "#211B16",
    "--accent": dark ? "#FF5630" : "#D9390D", "--accent-hover": dark ? "#FF6E4D" : "#C33108", "--accent-contrast": dark ? "#1B1714" : "#FFFFFF", "--accent-cool": dark ? "#78B4FF" : "#3B82F6", "--accent-cool-contrast": dark ? "#041022" : "#FFFFFF",
    "--muted": dark ? "#AAA7A5" : "#564C41", "--muted-2": dark ? "#A09C99" : "#6F6355", "--line": dark ? "#4E4036" : "#CDC1B0", "--secondary": dark ? "#9DB4A0" : "#445D4E",
    "--surface-1": dark ? "#211B18" : "#EFE9DF", "--surface-2": dark ? "#2D2521" : "#FCFBF8", "--surface-3": dark ? "#3C322C" : "#E4DDD2", "--surface-hover": dark ? "#483D36" : "#D8CFC2",
    "--focus-ring": `0 0 0 2px ${dark ? "#131010" : "#FFFFFF"}, 0 0 0 4px ${dark ? "#FF5630" : "#D9390D"}`,
  };
  applyTokens(values, r);
}

const FONTS: Array<{ id: string; stack: string }> = [
  { id: "system", stack: '-apple-system, "SF Pro Text", "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif' },
  { id: "hanken", stack: "'Hanken Grotesk', system-ui, sans-serif" },
  { id: "inter", stack: "'Inter', system-ui, sans-serif" },
  { id: "source-serif", stack: "'Source Serif 4', Georgia, serif" },
  { id: "geist", stack: "'Geist', system-ui, sans-serif" },
  { id: "jetbrains", stack: "'JetBrains Mono', ui-monospace, monospace" },
];

export function applyFont(id: string): void {
  const font = FONTS.find((f) => f.id === id) ?? FONTS[0];
  // The owner sets `--rk-font-body`; `--font-body` (index.css) resolves to it.
  applyBodyFont(font.stack);
}

/**
 * The one theme store of the window. the host app persists the choice natively (settings
 * `app_theme`), so the store keeps nothing of its own (`storage: null`); App.tsx mirrors the saved
 * choice into it. The app shell and the Settings Appearance section both bind this store, so there
 * is no second, private `rightkit.theme` store.
 */
export const themeStore = createThemeStore({
  storageKey: "heardright.theme",
  storage: null,
  defaultChoice: "dark",
});
// the host app brand and surface tokens follow the one store, whoever changes it.
themeStore.subscribe((choice) => applyTheme(legacyThemeFromShell(choice)));
