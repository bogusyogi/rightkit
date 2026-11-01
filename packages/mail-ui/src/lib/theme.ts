// the host app theme system. The light/dark/system choice and first paint belong to
// @rightkit/react-shell's theme store; the 11 presets are app data applied as CSS
// custom properties through @rightkit/theme presetTokens.

import { shellSetTheme } from "./ipc/system";
import { applyBodyFont, createThemeStore, type ThemeChoice } from "@rightkit/react-shell";
import { applyTokens, ensureContrast, mix, presetTokens, readableOn, luminance } from "@rightkit/theme";
import { hostAppName } from "../appName";

export type ThemeMode = "light" | "dark";

export interface ThemeTokens {
  ink: string; // app background (canvas)
  panel: string; // sidebar / bars
  panel2: string; // elevated surfaces (menu, popover)
  paper: string; // primary text
  muted: string; // secondary text
  muted2: string; // metadata / disabled
  line: string; // borders
  primary: string; // accent — active states, buttons, wordmark
  primaryHi: string; // accent hover
  secondary: string; // sparing accent
  accentContrast?: string; // readable text/icon color on primary
  // Split-contrast themes (light content + dark chrome) override these for the
  // chrome frame (titlebar + sidebar). Omitted on normal themes → equal the base
  // tokens, so the chrome remap is a no-op.
  chromeText?: string; // text on dark chrome
  chromeMuted?: string; // secondary text on dark chrome
  chromeElev?: string; // elevated surface inside chrome (active row, dropdown)
  chromeLine?: string; // border inside chrome
}

export interface ThemePreset {
  id: string;
  name: string;
  mode: ThemeMode;
  tokens: ThemeTokens;
}

export const THEMES: ThemePreset[] = [
  // ── the host app brand themes (RED accent — default) ──
  {
    id: "mailright-dark",
    name: `${hostAppName()} Dark`,
    mode: "dark",
    tokens: {
      ink: "#17181A", panel: "#0F1011", panel2: "#2A2C2E", paper: "#ECEDEE",
      muted: "#96989C", muted2: "#92959A", line: "#2A2C2E",
      primary: "#FB2C36", primaryHi: "#FF4D55", secondary: "#6FA8DC",
      accentContrast: "#1A1208",
    },
  },
  {
    id: "mailright-light",
    name: `${hostAppName()} Light`,
    mode: "light",
    tokens: {
      ink: "#F7F3EC", panel: "#EDE5D8", panel2: "#FFF8F1", paper: "#1A1B1C",
      muted: "#5E6164", muted2: "#626568", line: "#E7DED2",
      primary: "#DF0022", primaryHi: "#C8001E", secondary: "#245BD0",
      accentContrast: "#FFFFFF",
    },
  },
  // ── Copper themes (opt-in, preserved verbatim) ──
  {
    id: "ink-copper",
    name: "Copper",
    mode: "dark",
    tokens: {
      ink: "#171613", panel: "#0E0D0B", panel2: "#24211C", paper: "#FAF8F4",
      muted: "#968D7F", muted2: "#948B7D", line: "#353027",
      primary: "#CA7446", primaryHi: "#EB965F", secondary: "#98B39B",
    },
  },
  {
    id: "paper-ink",
    name: "Copper Light",
    mode: "light",
    tokens: {
      ink: "#F7F2EA", panel: "#ECE2D5", panel2: "#FFFFFF", paper: "#1D1A15",
      muted: "#5E554B", muted2: "#665D53", line: "#DED3C3",
      primary: "#98502F", primaryHi: "#874527", secondary: "#50664F",
    },
  },
  // ── Additional themes ──
  {
    id: "slate-amber",
    name: "Slate & Amber",
    mode: "dark",
    tokens: {
      ink: "#1E2024", panel: "#14161A", panel2: "#2A2D33", paper: "#E8EAED",
      muted: "#9AA0A8", muted2: "#90969E", line: "#363A40",
      primary: "#E0A458", primaryHi: "#ECB874", secondary: "#6FA8DC",
    },
  },
  {
    id: "obsidian-iris",
    name: "Obsidian & Iris",
    mode: "dark",
    tokens: {
      ink: "#1A1721", panel: "#100E15", panel2: "#25202F", paper: "#ECE8F0",
      muted: "#9B91A8", muted2: "#93899E", line: "#342D3E",
      primary: "#B196FB", primaryHi: "#C9B5FF", secondary: "#5EC8C8",
    },
  },
  {
    id: "daylight-slate",
    name: "Daylight & Slate",
    mode: "light",
    tokens: {
      ink: "#F6F8FB", panel: "#E8ECF2", panel2: "#FFFFFF", paper: "#1B1F26",
      muted: "#515A66", muted2: "#58616D", line: "#DDE3EB",
      primary: "#245BD0", primaryHi: "#1F4EB8", secondary: "#A4492F",
    },
  },
  {
    id: "parchment-sienna",
    name: "Parchment & Sienna",
    mode: "light",
    tokens: {
      ink: "#F5EDE0", panel: "#E7D9C5", panel2: "#FCF7EF", paper: "#2A2017",
      muted: "#665947", muted2: "#665947", line: "#D8CBB8",
      primary: "#8E4622", primaryHi: "#7E3C1E", secondary: "#486554",
    },
  },
  // ── Split-contrast themes: light "paper" content, dark chrome frame ──
  {
    id: "paper-graphite",
    name: "Paper & Graphite",
    mode: "dark", // the window frame / chrome is dark
    tokens: {
      ink: "#F4F1EA", panel: "#1B1C1F", panel2: "#FFFFFF", paper: "#21221F",
      muted: "#5C6066", muted2: "#5C6066", line: "#E4DFD5",
      primary: "#C2410C", primaryHi: "#A9380B", secondary: "#2563EB",
      chromeText: "#EDEDEC", chromeMuted: "#9A9C9F", chromeElev: "#2A2C2F", chromeLine: "#313338",
    },
  },
  {
    id: "mist-slate",
    name: "Mist & Slate",
    mode: "dark",
    tokens: {
      ink: "#EEF1F4", panel: "#171B22", panel2: "#FFFFFF", paper: "#1B1F26",
      muted: "#515A66", muted2: "#606975", line: "#DCE2EA",
      primary: "#2563EB", primaryHi: "#1D52CC", secondary: "#C2410C",
      chromeText: "#E7EAEE", chromeMuted: "#929BA8", chromeElev: "#232A34", chromeLine: "#2C3440",
    },
  },
];

export const DEFAULT_DARK = "mailright-dark";
export const DEFAULT_LIGHT = "mailright-light";

export function themeById(id: string): ThemePreset {
  return THEMES.find((t) => t.id === id) ?? THEMES[0];
}

// ── Light/dark/system choice: the shared react-shell store ──
const MODE_KEY = "mailright.theme";
const PRESET_KEY = "mailright.preset";
export const themeStore = createThemeStore({ storageKey: MODE_KEY, defaultChoice: "dark" });

function getItem(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function setItem(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    /* ignore */
  }
}

/** Builds the CSS variable record for a preset (app aliases + derived tokens) via presetTokens. */
function presetVars(t: ThemeTokens, mode: ThemeMode, id: string): Record<string, string> {
  const raw: Record<string, string> = {
    "--ink": t.ink,
    "--panel": t.panel,
    "--panel-2": t.panel2,
    "--paper": t.paper,
    "--muted": ensureContrast(t.muted, t.ink, 4.5),
    "--muted-2": ensureContrast(t.muted2, t.ink, 4.5),
    "--line": t.line,
    // --line-2: app preset data (M2 exception B3, docs/rightkit-exceptions/M2.md); mix() nudges the line toward the opposite pole.
    "--line-2": mix(t.line, luminance(t.line) < 0.2 ? "#FFFFFF" : "#000000", 0.06),
    "--copper": t.primary,
    "--copper-hi": t.primaryHi,
    "--secondary": t.secondary,
    "--accent-contrast": t.accentContrast ?? readableOn(t.primary, "#FFFFFF", "#1A1208"),
    // Chrome frame vars default to the base tokens unless a split-contrast theme overrides them.
    "--chrome-text": t.chromeText ?? t.paper,
    "--chrome-muted": t.chromeMuted ?? t.muted,
    "--chrome-elev": t.chromeElev ?? t.panel2,
    "--chrome-line": t.chromeLine ?? t.line,
    // The shell's own controls (menus, popovers, focus) follow the preset accent.
    "--rk-accent": t.primary,
    "--rk-accent-ink": t.accentContrast ?? readableOn(t.primary, "#FFFFFF", "#1A1208"),
    "--rk-accent-soft": mix(t.primary, t.panel, 0.86),
  };
  return presetTokens({ id, mode, tokens: raw });
}

/** The preset the choice resolves to: a preset id wins; "auto" follows the stored mode/OS. */
export function resolveTheme(choice: string): ThemePreset {
  if (choice === "auto") return themeById(themeStore.resolved() === "dark" ? DEFAULT_DARK : DEFAULT_LIGHT);
  return themeById(choice);
}

/** Tell the native window light vs dark so macOS renders its chrome to match. No-op outside Tauri. */
function syncWindowAppearance(mode: ThemeMode): void {
  void shellSetTheme(mode).catch(() => {});
}

/** Apply a combined choice: "auto" (follow the OS) or a preset id. Persists nothing. */
export function applyTheme(choice: string): void {
  const preset = resolveTheme(choice);
  applyTokens(presetVars(preset.tokens, preset.mode, preset.id), document.documentElement);
  syncWindowAppearance(preset.mode);
}

/** Persist and apply a combined choice ("auto" or a preset id). */
export function setThemeChoice(choice: string): void {
  if (choice === "auto") {
    setItem(PRESET_KEY, null);
    themeStore.save("system");
    themeStore.apply("system");
  } else {
    const preset = themeById(choice);
    setItem(PRESET_KEY, preset.id);
    themeStore.save(preset.mode);
    themeStore.apply(preset.mode);
  }
  applyTheme(choice);
}

/** The AppearanceMenu choice: picks the default preset for that mode, or "auto" for system. */
export function setModeChoice(mode: ThemeChoice): void {
  setThemeChoice(mode === "system" ? "auto" : mode === "dark" ? DEFAULT_DARK : DEFAULT_LIGHT);
}

/** Combined choice currently stored: a preset id, or "auto". Migrates the old single-key format. */
export function loadThemeChoice(): string {
  const preset = getItem(PRESET_KEY);
  if (preset && THEMES.some((t) => t.id === preset)) return preset;
  const legacy = getItem(MODE_KEY);
  if (legacy && THEMES.some((t) => t.id === legacy)) {
    const p = themeById(legacy);
    setItem(PRESET_KEY, p.id);
    setItem(MODE_KEY, p.mode);
    return p.id;
  }
  if (legacy === "auto") setItem(MODE_KEY, "system");
  return themeStore.read() === "system" ? "auto" : themeStore.read() === "light" ? DEFAULT_LIGHT : DEFAULT_DARK;
}

/** First paint: the store applies data-theme synchronously, then the preset tokens go on. */
export function bootTheme(): void {
  const choice = loadThemeChoice();
  themeStore.apply(themeStore.read());
  applyTheme(choice);
  applyFont(loadFontChoice());
}

// ── Font picker (bundled families in /public/fonts) ──
export interface FontOption {
  id: string;
  name: string;
  stack: string;
}
export const FONTS: FontOption[] = [
  { id: "inter", name: "Inter", stack: "'Inter', system-ui, sans-serif" },
  { id: "source-serif", name: "Source", stack: "'Source Serif 4', Georgia, serif" },
  { id: "newsreader", name: "Newsreader", stack: "'Newsreader', Georgia, serif" },
  { id: "geist", name: "Geist", stack: "'Geist', system-ui, sans-serif" },
  { id: "jetbrains", name: "JetBrains", stack: "'JetBrains Mono', ui-monospace, monospace" },
];

const FONT_KEY = "mailright.font";
export function loadFontChoice(): string {
  return getItem(FONT_KEY) || "inter";
}
export function saveFontChoice(id: string): void {
  setItem(FONT_KEY, id);
}
/** Apply the chosen UI/reading font: `applyBodyFont` sets --rk-font-body, which the --body token follows. */
export function applyFont(id: string): void {
  const f = FONTS.find((x) => x.id === id) ?? FONTS[0];
  applyBodyFont(f.stack);
}
