import type { TokenPreset } from "@rightkit/theme";
import { hostAppName } from "../host.js";

export type ThemeMode = "light" | "dark";

export interface ThemeTokens {
  ink: string; // app background (canvas)
  panel: string; // sidebar / bars
  panel2: string; // elevated surfaces (menu, modal)
  paper: string; // primary text
  muted: string; // secondary text
  muted2: string; // metadata / disabled
  line: string; // borders
  primary: string; // main accent — ribbon, buttons, active states
  primaryHi: string; // accent hover
  secondary: string; // sparing accent — inline code, links, wordmark dot
  chrome: string; // window chrome: titlebar / sidebar / document bars
  chrome2: string; // elevated chrome: popovers / hover panels
  chromePaper: string; // primary text on chrome
  chromeMuted: string; // secondary text on chrome
  chromeMuted2: string; // disabled / metadata on chrome
  chromeLine: string; // chrome borders
  chromeHover: string; // chrome row hover / active wash
  danger: string; // destructive-action colour
  dangerHi: string; // destructive hover
}

export interface ThemePreset {
  id: string;
  name: string;
  mode: ThemeMode;
  tokens: ThemeTokens;
}

export interface AccentPreset {
  id: string;
  name: string;
  dark: string;
  light: string;
}

export const THEMES: ThemePreset[] = [
  {
    id: "viewright-paper",
    name: "Paper",
    mode: "light",
    tokens: {
      ink: "#FFFDF8",
      panel: "#FFFFFF",
      panel2: "#F3EEE6",
      paper: "#211C18",
      muted: "#6E665F",
      muted2: "#5A544D",
      line: "#E2DAD1",
      primary: "#0C6AA5",
      primaryHi: "#095785",
      secondary: "#0C6AA5",
      chrome: "#EAE4D9",
      chrome2: "#E0D8CB",
      chromePaper: "#211C18",
      chromeMuted: "#5E574F",
      chromeMuted2: "#655D56",
      chromeLine: "#CBC1B2",
      chromeHover: "#D8CFC0",
      danger: "#B3402A",
      dangerHi: "#D4503A",
    },
  },
  {
    id: "black-grey",
    name: "Black & Grey",
    mode: "dark",
    tokens: {
      ink: "#101216",
      panel: "#16181D",
      panel2: "#1A1D23",
      paper: "#F2F3F5",
      muted: "#A3A7AF",
      muted2: "#858B95",
      line: "#292D35",
      primary: "#C7CBD2",
      primaryHi: "#E4E6EA",
      secondary: "#8A9099",
      chrome: "#050608",
      chrome2: "#0B0C10",
      chromePaper: "#F5F6F7",
      chromeMuted: "#A1A6AF",
      chromeMuted2: "#858B95",
      chromeLine: "#1B1E24",
      chromeHover: "#181B21",
      danger: "#E05050",
      dangerHi: "#F06060",
    },
  },
  {
    id: "ink-copper",
    name: "Copper",
    mode: "dark",
    tokens: {
      ink: "#1B1814",
      panel: "#241F19",
      panel2: "#2C261F",
      paper: "#FAF8F4",
      muted: "#9A9184",
      muted2: "#938D86",
      line: "#3A372F",
      primary: "#C46A3D",
      primaryHi: "#D9803F",
      secondary: "#9DB4A0",
      chrome: "#0F0F0E",
      chrome2: "#191714",
      chromePaper: "#FAF8F4",
      chromeMuted: "#A79F91",
      chromeMuted2: "#858078",
      chromeLine: "#2E2A23",
      chromeHover: "#241E18",
      danger: "#E05050",
      dangerHi: "#F06060",
    },
  },
  {
    id: "obsidian-iris",
    name: "Obsidian",
    mode: "dark",
    tokens: {
      ink: "#211C29",
      panel: "#30283A",
      panel2: "#3B3246",
      paper: "#F1EDF4",
      muted: "#B0A7BC",
      muted2: "#AAA3B2",
      line: "#4B4158",
      primary: "#A78BFA",
      primaryHi: "#C0ACFC",
      secondary: "#67D0D0",
      chrome: "#131118",
      chrome2: "#211C29",
      chromePaper: "#F5F1F8",
      chromeMuted: "#BBB2C6",
      chromeMuted2: "#91899B",
      chromeLine: "#352E3E",
      chromeHover: "#2A2433",
      danger: "#E05050",
      dangerHi: "#F06060",
    },
  },
];

/** The four shipped themes — the single picker set (Settings, Aa popover, palette, appearance menu). */
export const BASE_THEME_IDS = [
  "viewright-paper",
  "black-grey",
  "ink-copper",
  "obsidian-iris",
] as const;

/** Each choice owns a dark- and light-body value so the same intent remains readable. */
export const ACCENT_PRESETS: AccentPreset[] = [
  // A getter: the name reads hostAppName() when displayed, not at import, so setHostAppName() at host startup takes effect.
  {
    id: "blue",
    get name() {
      return `${hostAppName()} blue`;
    },
    dark: "#3FA3DE",
    light: "#0C6AA5",
  },
  { id: "copper", name: "Copper", dark: "#C46A3D", light: "#9D5530" },
  { id: "iris", name: "Iris", dark: "#A78BFA", light: "#6D4BC3" },
  { id: "amber", name: "Amber", dark: "#E0A458", light: "#87570E" },
  { id: "sienna", name: "Sienna", dark: "#D47A4A", light: "#A5552B" },
  { id: "grey", name: "Grey", dark: "#C7CBD2", light: "#535A65" },
  { id: "teal", name: "Teal", dark: "#5EC8C8", light: "#217A7A" },
  { id: "sage", name: "Sage", dark: "#9DB4A0", light: "#4F6B5B" },
];

export const DEFAULT_DARK = "black-grey";
export const DEFAULT_LIGHT = "viewright-paper";

export function themeById(id: string): ThemePreset {
  return THEMES.find((t) => t.id === id) ?? THEMES[0];
}

/** A theme as a published `TokenPreset`: the host app flat token names (`--ink`, `--copper`, ...) as the
 *  base palette. `presetTokens(preset)` returns the record `applyTokens` writes; accents are merged
 *  per mode by `accentPreset`. */
export function tokenPreset(theme: ThemePreset): TokenPreset {
  const t = theme.tokens;
  return {
    id: theme.id,
    mode: theme.mode,
    tokens: {
      "--ink": t.ink,
      "--reader-surface": t.ink,
      "--reader-text": t.paper,
      "--reader-line": t.line,
      "--panel": t.panel,
      "--panel-2": t.panel2,
      "--paper": t.paper,
      "--muted": t.muted,
      "--muted-2": t.muted2,
      "--line": t.line,
      "--copper": t.primary,
      "--copper-hi": t.primaryHi,
      "--accent-2": t.secondary,
      "--danger": t.danger,
      "--danger-hi": t.dangerHi,
      "--chrome": t.chrome,
      "--chrome-2": t.chrome2,
      "--chrome-paper": t.chromePaper,
      "--chrome-muted": t.chromeMuted,
      "--chrome-muted-2": t.chromeMuted2,
      "--chrome-line": t.chromeLine,
      "--chrome-hover": t.chromeHover,
    },
  };
}

/** Accent choices for one theme mode, keyed by accent id, writing `variable` (`--copper` or `--accent-2`). */
export function accentPreset(
  theme: ThemePreset,
  variable: "--copper" | "--accent-2",
): TokenPreset {
  return {
    ...tokenPreset(theme),
    accents: Object.fromEntries(
      ACCENT_PRESETS.map((accent) => [accent.id, { [variable]: accent[theme.mode] }]),
    ),
  };
}
