import { createThemeStore, type ThemeChoice } from "@rightkit/react-shell";
import { applyTokens, mix } from "@rightkit/theme";
import { isQaMode } from "./ipc/index.js";
import { hostAppName } from "../../appName.js";

// the host app locked Right Suite identity.
// Source: /brand SR, backed by D:\Claude\.claude\rules\brands.md.

type ThemeMode = "light" | "dark";

interface ThemeTokens {
  ink: string;
  panel: string;
  panel2: string;
  paper: string;
  muted: string;
  muted2: string;
  line: string;
  primary: string;
  primaryHi: string;
  secondary: string;
}

export interface ThemePreset {
  id: string;
  name: string;
  mode: ThemeMode;
  tokens: ThemeTokens;
}

export const THEMES: ThemePreset[] = [
  {
    id: "scraperight-dark",
    name: `${hostAppName()} Dark`,
    mode: "dark",
    tokens: {
      // the host app dark — near-black, re-locked 2026-07-19. Supersedes the
      // warm-graphite/espresso direction: at ~8% saturation that read as dirty
      // grey rather than warm brown on a phone screen. The neutral base is what
      // lets marker gold carry the identity alone.
      //
      // Retired, never reinstate: acid #E8FF2E accent, espresso #1C1712 base.
      ink: "#0F0F10",
      panel: "#17171A",
      // Not in the brand doc, which specifies bg/surface/border only — derived
      // to sit between surface and border for a second elevation step.
      panel2: "#1F1F23",
      paper: "#F2F0EC",
      muted: "#969390",
      muted2: "#6E6B68", // derived: dimmer companion to muted
      line: "#2A2A2E",
      primary: "#FFC24B", // marker gold, 11.9:1 on #0F0F10
      primaryHi: "#FFD37A", // derived: lighter gold for hover/active
      secondary: "#8A8A90",
    },
  },
  {
    id: "scraperight-light",
    name: `${hostAppName()} Light`,
    mode: "light",
    tokens: {
      // the host app light — ivory, locked 2026-07-19, picked from rendered app
      // screens against neutral-paper alternatives.
      //
      // The accent is necessarily deeper here: #FFC24B on paper is 1.5:1, and
      // holding its hue to the 4.5:1 body floor lands on #845C00 (white on that
      // fill is 5.98:1). Gold cannot stay bright on a light background — if a
      // future change wants brighter gold in light mode, the answer is gold as
      // a FILL with dark text on it, never lightening this token past AA.
      ink: "#FBF7EF",
      panel: "#FFFCF5",
      panel2: "#FFFFFF",
      paper: "#231F1A",
      muted: "#6B655C",
      muted2: "#8A8378", // derived: lighter companion to muted
      line: "#E6DECF",
      primary: "#845C00",
      primaryHi: "#6B4A00", // derived: darker gold for hover/active
      secondary: "#6B655C",
    },
  },
];

const DEFAULT_DARK = "scraperight-dark";
const DEFAULT_LIGHT = "scraperight-light";

function themeById(id: string): ThemePreset {
  return THEMES.find((t) => t.id === id) ?? THEMES[0];
}

function resolveTheme(choice: string): ThemePreset {
  // "auto" follows the store's own OS colour-scheme query (store.resolved()).
  if (choice === "auto")
    return themeById(themeStore.resolved() === "dark" ? DEFAULT_DARK : DEFAULT_LIGHT);
  return themeById(choice);
}

function paint(choice: string): void {
  const resolved = resolveTheme(choice);
  document.documentElement.dataset.theme = resolved.mode;
  const t = resolved.tokens;
  // Mechanical write (and previous-value restore) is @rightkit/theme's; the names and values stay the host app own.
  applyTokens({
    "--ink": t.ink,
    "--panel": t.panel,
    "--panel-2": t.panel2,
    "--paper": t.paper,
    "--muted": t.muted,
    "--muted-2": t.muted2,
    "--line": t.line,
    "--line-2": mix(t.line, resolved.mode === "dark" ? "#ffffff" : "#000000", 0.06),
    "--accent": t.primary,
    "--accent-hi": t.primaryHi,
    "--secondary": t.secondary,
    "--warn": resolved.mode === "light" ? "#C4001A" : "#EC6A5C",
    // Locked 2026-07-05: Author body, JetBrains Mono for DATA ONLY (transcripts,
    // counts, paths). Wordmark is Tanker, suite-wide. No surface reads --display.
    "--body": "'Author', -apple-system, BlinkMacSystemFont, sans-serif",
    "--mono": "'JetBrains Mono', ui-monospace, Menlo, Consolas, monospace",
    "--wordmark": "'Tanker', 'Khand', sans-serif",
  });
}

const KEY = "scraperight.theme";

function qaOverride(): ThemeChoice | undefined {
  if (!isQaMode) return undefined;
  const forced = new URLSearchParams(window.location.search).get("theme");
  return forced === "light" || forced === "dark" ? forced : undefined;
}

/** Shared @rightkit/react-shell store owns the persisted choice; the host app
 * owns the palette painted for it. */
export const themeStore = createThemeStore({
  storageKey: KEY,
  qaOverride: qaOverride(),
  // Explicit: the host app paint() always writes data-theme to the resolved mode,
  // so the store keeps the same always-painted root for `system`.
  defaultChoice: "system",
  alwaysPaintDataTheme: true,
});

const toPreset = (choice: ThemeChoice): string =>
  choice === "system" ? "auto" : choice === "light" ? DEFAULT_LIGHT : DEFAULT_DARK;
const toChoice = (preset: string): ThemeChoice =>
  preset === "auto" ? "system" : preset === DEFAULT_LIGHT ? "light" : "dark";

// The store owns the OS listener and notifies subscribers on an OS change while the
// choice is "system", so no matchMedia listener lives here.
themeStore.subscribe((choice) => paint(toPreset(choice)));

/** Paint a preset id ("auto" | "scraperight-light" | "scraperight-dark"). */
export function applyTheme(preset: string): void {
  paint(preset);
}

export function loadThemeChoice(): string {
  return toPreset(themeStore.read());
}

export function saveThemeChoice(preset: string): void {
  themeStore.save(toChoice(preset));
  paint(preset);
}
