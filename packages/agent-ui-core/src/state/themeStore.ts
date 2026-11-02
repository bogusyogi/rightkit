import { create } from "zustand";
import { applyStoredThemeSync, createThemeStore } from "@rightkit/react-shell";
import { applyTokens, presetTokens, type TokenPreset } from "@rightkit/theme";
import paletteData from "./palettes.json" with { type: "json" };

export type Theme = "dark" | "light";
export type Palette = "coderight" | "heardright" | "viewright" | "scraperight" | "mailright";

type SiblingPalette = Exclude<Palette, "coderight">;
type PaletteData = Record<SiblingPalette, { label: string; dark: Record<string, string>; light: Record<string, string> }>;
const SIBLINGS = paletteData as PaletteData;

export const PALETTES: { id: Palette; label: string }[] = [
  { id: "coderight", label: "Default" },
  ...(Object.keys(SIBLINGS) as SiblingPalette[]).map((id) => ({ id, label: SIBLINGS[id].label })),
];

/**
 * Sibling palettes are RightKit token presets (`@rightkit/theme` `applyTokens` / `presetTokens`), one
 * per palette and mode. the host app is the :root default in index.css (no preset applied). A light preset
 * layers its overrides over the palette's dark tokens, the same cascade the retired CSS blocks had.
 */
const PRESETS: Record<SiblingPalette, Record<Theme, TokenPreset>> = Object.fromEntries(
  (Object.keys(SIBLINGS) as SiblingPalette[]).map((id) => [
    id,
    {
      dark: { id, mode: "dark", tokens: SIBLINGS[id].dark },
      light: { id, mode: "light", tokens: { ...SIBLINGS[id].dark, ...SIBLINGS[id].light } },
    },
  ]),
) as Record<SiblingPalette, Record<Theme, TokenPreset>>;

const THEME_KEY = "cr.theme.v1";
const PALETTE_KEY = "cr.palette.v1";

function hasDom(): boolean {
  return typeof document !== "undefined" && !!document.documentElement;
}

function readStoredPalette(): Palette {
  try {
    const v = localStorage.getItem(PALETTE_KEY);
    return PALETTES.some((p) => p.id === v) ? (v as Palette) : "coderight";
  } catch {
    return "coderight";
  }
}

/**
 * Shared with RightKit's ShellProvider (`themeStore` prop) so both sides agree on the choice.
 * the host app contract is that dark is the index.css :root default (attribute absent) and only
 * light sets data-theme="light", which is RightKit's `darkRemovesAttribute`.
 */
const THEME_OPTIONS = {
  storageKey: THEME_KEY,
  darkRemovesAttribute: true,
  defaultChoice: "dark" as const,
};

export const shellThemeStore = createThemeStore(THEME_OPTIONS);

/** Palette mode for a choice; "system" resolves through the store's own OS query, the same one that paints data-theme. */
function toTheme(choice: "light" | "dark" | "system"): Theme {
  return shellThemeStore.resolved(choice);
}

let undoPalette: (() => void) | null = null;

function applyPalette(palette: Palette, theme: Theme) {
  // the host app is the default :root palette (nothing applied); siblings apply a RightKit token preset.
  if (!hasDom()) return;
  undoPalette?.();
  undoPalette = null;
  if (palette === "coderight") return;
  undoPalette = applyTokens(presetTokens(PRESETS[palette][theme]));
}

// Before first render: the stored theme lands on <html> synchronously (RightKit's boot step).
applyStoredThemeSync(THEME_OPTIONS);
const INITIAL_THEME = toTheme(shellThemeStore.read());
const INITIAL_PALETTE = readStoredPalette();
applyPalette(INITIAL_PALETTE, INITIAL_THEME);
// A theme change made through RightKit's ShellProvider re-layers the active palette's mode.
let activePalette: Palette = INITIAL_PALETTE;
shellThemeStore.subscribe((choice) => applyPalette(activePalette, toTheme(choice)));

interface ThemeStoreState {
  palette: Palette;
  setPalette: (palette: Palette) => void;
}

/** Palette choice only. The light/dark choice is RightKit's theme store (`useTheme()`). */
export const useThemeStore = create<ThemeStoreState>((set) => ({
  palette: INITIAL_PALETTE,
  setPalette(palette) {
    activePalette = palette;
    applyPalette(palette, toTheme(shellThemeStore.read()));
    try {
      localStorage.setItem(PALETTE_KEY, palette);
    } catch {
      /* ignore */
    }
    set({ palette });
  },
}));
