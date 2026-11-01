// the host app-only feature (VS Code theme import, activation, persistence): @rightkit/theme 0.3.1 has no imported-theme module, so nothing here duplicates a published RightKit capability.
import { ensureContrast as ensureContrastHex, luminance, mix as mixHex, prefersDark, readableOn } from "@rightkit/theme";
import { create } from "zustand";

/**
 * Hermes lineage: themes/context.tsx + themes/install.ts.
 *
 * the host app keeps its locked black/grey surfaces authoritative. Imported themes
 * therefore retint the accent chain and expose the converted palette through
 * `--cr-imported-*` variables instead of replacing the shell background.
 */

export type ThemeMode = "light" | "dark" | "system";
export type RenderedThemeMode = "light" | "dark";

export interface ImportedThemeColors {
  background: string;
  foreground: string;
  accent: string;
  surface: string;
  border: string;
  muted: string;
  destructive: string;
}

export interface ImportedTheme {
  id: string;
  label: string;
  description: string;
  source: "vscode" | "marketplace" | "manual";
  colors: ImportedThemeColors;
  darkColors?: ImportedThemeColors;
}

export interface VscodeColorTheme {
  name?: string;
  type?: string;
  include?: string;
  colors?: Record<string, unknown>;
  tokenColors?: unknown;
}

export interface MarketplaceThemeResult {
  extensionId: string;
  displayName: string;
  themes: Array<{ label?: string; uiTheme?: string; contents: string }>;
}

// RightKit's colour maths returns upper-case hex; the stored and applied values stay lower-case as before.
const mix = (from: string, to: string, amount: number): string => mixHex(from, to, amount).toLowerCase();
const ensureContrast = (color: string, background: string, min: number): string =>
  ensureContrastHex(color, background, min).toLowerCase();

const STORAGE_KEY = "cr.imported-themes.v1";
const ACTIVE_KEY = "cr.imported-theme-active.v1";
const MODE_KEY = "cr.imported-theme-mode.v1";
export const MARKETPLACE_ID_RE = /^[\w-]+\.[\w-]+$/;
const DEFAULT_IMPORTED_COLORS: ImportedThemeColors = {
  background: "#0b0b0a",
  foreground: "#f2f1ed",
  accent: "#e53855",
  surface: "#1c1c1a",
  border: "#444440",
  muted: "#9a978f",
  destructive: "#fa423e",
};

function storage(): Storage | null {
  return typeof globalThis.localStorage === "undefined" ? null : globalThis.localStorage;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hexToRgb(value: string): [number, number, number] | null {
  const clean = value.replace(/^#/, "");
  if (!/^[0-9a-f]{6}$/i.test(clean)) return null;
  return [0, 2, 4].map((index) => Number.parseInt(clean.slice(index, index + 2), 16)) as [number, number, number];
}

function rgbToHex(rgb: [number, number, number]): string {
  return `#${rgb.map((channel) => Math.round(Math.max(0, Math.min(255, channel))).toString(16).padStart(2, "0")).join("")}`;
}

function normalizeHex(input: unknown, backdrop = "#000000"): string | null {
  if (typeof input !== "string") return null;
  let clean = input.trim().replace(/^#/, "");
  if (clean.length === 3 || clean.length === 4) clean = clean.split("").map((part) => part + part).join("");
  if (!/^[0-9a-f]{6}([0-9a-f]{2})?$/i.test(clean)) return null;
  const rgb = hexToRgb(`#${clean.slice(0, 6)}`);
  if (!rgb) return null;
  if (clean.length === 6) return rgbToHex(rgb);
  const alpha = Number.parseInt(clean.slice(6), 16) / 255;
  const base = hexToRgb(backdrop) ?? [0, 0, 0];
  return rgbToHex([base[0] + (rgb[0] - base[0]) * alpha, base[1] + (rgb[1] - base[1]) * alpha, base[2] + (rgb[2] - base[2]) * alpha]);
}

function readStoredThemes(): Record<string, ImportedTheme> {
  try {
    const raw = storage()?.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed)) return {};
    return Object.fromEntries(Object.entries(parsed).filter(([, value]) => isValidImportedTheme(value))) as Record<string, ImportedTheme>;
  } catch {
    return {};
  }
}

function persistThemes(themes: Record<string, ImportedTheme>): void {
  try {
    storage()?.setItem(STORAGE_KEY, JSON.stringify(themes));
  } catch {
    // The in-memory store remains usable when storage is unavailable.
  }
}

function persistValue(key: string, value: string | null): void {
  try {
    if (value === null) storage()?.removeItem(key);
    else storage()?.setItem(key, value);
  } catch {
    // Best effort, matching the donor's boot-time storage behavior.
  }
}

function readMode(): ThemeMode {
  try {
    const value = storage()?.getItem(MODE_KEY);
    return value === "light" || value === "dark" || value === "system" ? value : "dark";
  } catch {
    return "dark";
  }
}

function readActiveTheme(): string | null {
  try {
    return storage()?.getItem(ACTIVE_KEY) ?? null;
  } catch {
    return null;
  }
}

export function isValidImportedTheme(value: unknown): value is ImportedTheme {
  if (!isRecord(value) || typeof value.id !== "string" || typeof value.label !== "string" || !isRecord(value.colors)) return false;
  const colors = value.colors;
  return ["background", "foreground", "accent", "surface", "border", "muted", "destructive"].every((key) => typeof colors[key] === "string");
}

export function vscodeThemeSlug(name: string): string {
  const slug = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48);
  return `vsc-${slug || "theme"}`;
}

/** VS Code theme files are commonly JSONC, so accept comments and trailing commas. */
export function parseVscodeTheme(text: string): VscodeColorTheme {
  const stripped = text
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:"'\\])\/\/[^\n\r]*/g, "$1")
    .replace(/,(\s*[}\]])/g, "$1");
  const parsed: unknown = JSON.parse(stripped);
  if (!isRecord(parsed)) throw new Error("Theme file is not a JSON object.");
  return parsed as VscodeColorTheme;
}

function convertVscodeTheme(raw: VscodeColorTheme, source: ImportedTheme["source"] = "vscode", labelOverride?: string): ImportedTheme {
  if (!raw.colors || !isRecord(raw.colors)) throw new Error('Theme has no "colors" map — not a VS Code color theme.');
  const colors = raw.colors;
  const background = normalizeHex(colors["editor.background"]) ?? "#1e1e1e";
  const dark = (raw.type ?? "").toLowerCase().includes("dark") || (!(raw.type ?? "").toLowerCase().includes("light") && luminance(background) < 0.4);
  const foreground = normalizeHex(colors["editor.foreground"], background) ?? (dark ? "#d4d4d4" : "#1f1f1f");
  const pick = (keys: string[], fallback: string) => {
    for (const key of keys) {
      const value = normalizeHex(colors[key], background);
      if (value) return value;
    }
    return fallback;
  };
  const surface = pick(["editorWidget.background", "dropdown.background", "menu.background"], mix(background, foreground, dark ? 0.08 : 0.05));
  const border = pick(["panel.border", "editorGroup.border", "sideBar.border", "contrastBorder"], mix(background, foreground, dark ? 0.16 : 0.14));
  const sidebar = pick(["sideBar.background", "activityBar.background"], mix(background, foreground, dark ? 0.02 : 0.012));
  const accentSource = pick(["button.background", "textLink.activeForeground", "textLink.foreground", "activityBarBadge.background", "badge.background", "focusBorder", "tab.activeBorder"], mix(foreground, background, 0.55));
  const accent = ensureContrast(accentSource, sidebar, 4.5);
  const palette: ImportedThemeColors = {
    background,
    foreground,
    accent,
    surface,
    border,
    muted: pick(["descriptionForeground", "editorLineNumber.foreground", "disabledForeground"], mix(foreground, background, 0.45)),
    destructive: pick(["editorError.foreground", "errorForeground"], "#e25563"),
  };
  const label = (labelOverride ?? raw.name ?? "VS Code Theme").trim();
  const id = vscodeThemeSlug(labelOverride ?? raw.name ?? "theme");
  return { id, label, description: source === "marketplace" ? `VS Code · imported marketplace theme` : "Imported from VS Code", source, colors: palette, darkColors: palette };
}

export function buildImportedThemeFromVscode(text: string, options: { label?: string; source?: ImportedTheme["source"] } = {}): ImportedTheme {
  return convertVscodeTheme(parseVscodeTheme(text), options.source ?? "vscode", options.label);
}

/** Donor-compatible name retained at the the host app seam. */
export const installVscodeThemeFromText = (text: string, options?: { label?: string; source?: ImportedTheme["source"] }): ImportedTheme => {
  const theme = buildImportedThemeFromVscode(text, options);
  useImportedThemeStore.getState().install(theme);
  return theme;
};

export function buildThemeFromMarketplace(result: MarketplaceThemeResult): ImportedTheme {
  if (!result.themes.length) throw new Error(`"${result.extensionId}" does not contribute any color themes.`);
  const variants = result.themes.map((file) => {
    const theme = convertVscodeTheme(parseVscodeTheme(file.contents), "marketplace", file.label || result.displayName);
    const rawType = (parseVscodeTheme(file.contents).type ?? file.uiTheme ?? "").toLowerCase();
    return { theme, light: rawType.includes("light") || !rawType.includes("dark") };
  });
  const fallback = variants[0].theme;
  const light = variants.find((variant) => variant.light)?.theme ?? fallback;
  const dark = variants.find((variant) => !variant.light)?.theme ?? fallback;
  return { ...light, id: vscodeThemeSlug(result.displayName), label: result.displayName, description: `VS Code · ${result.extensionId}`, source: "marketplace", colors: light.colors, darkColors: dark.colors };
}

export async function installVscodeThemeFromMarketplace(id: string, fetcher?: (id: string) => Promise<MarketplaceThemeResult>): Promise<ImportedTheme> {
  const trimmed = id.trim();
  if (!MARKETPLACE_ID_RE.test(trimmed)) throw new Error('Expected a Marketplace id like "publisher.extension".');
  const bridge = fetcher ?? (globalThis as typeof globalThis & { hermesDesktop?: { themes?: { fetchMarketplace?: (id: string) => Promise<MarketplaceThemeResult> } } }).hermesDesktop?.themes?.fetchMarketplace;
  if (!bridge) throw new Error("Marketplace install is only available through a desktop theme bridge.");
  const theme = buildThemeFromMarketplace(await bridge(trimmed));
  useImportedThemeStore.getState().install(theme);
  return theme;
}

export function renderedModeFor(colors: ImportedThemeColors, mode: ThemeMode): RenderedThemeMode {
  // `prefersDark()` (@rightkit/theme) is the OS dark query; it is false without a window or matchMedia.
  const requested = mode === "system" ? (prefersDark() ? "dark" : "light") : mode;
  return luminance(colors.background) > 0.5 ? "light" : requested;
}

function activeColors(theme: ImportedTheme, mode: ThemeMode): ImportedThemeColors {
  return mode === "dark" && theme.darkColors ? theme.darkColors : theme.colors;
}

const RETINT_KEYS = ["--color-accent", "--color-focus", "--color-brand-red", "--color-brand-red-2", "--color-on-accent"];
const IMPORTED_COLOR_KEYS = Object.keys(DEFAULT_IMPORTED_COLORS);

/** Paint only imported retint variables; the locked neutral base is untouched. */
export function applyImportedTheme(theme: ImportedTheme | null, mode: ThemeMode): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  for (const key of RETINT_KEYS) root.style.removeProperty(key);
  for (const key of IMPORTED_COLOR_KEYS) root.style.removeProperty(`--cr-imported-${key}`);
  if (!theme) {
    root.removeAttribute("data-imported-theme");
    root.removeAttribute("data-imported-theme-mode");
    return;
  }
  const colors = activeColors(theme, mode);
  root.dataset.importedTheme = theme.id;
  root.dataset.importedThemeMode = renderedModeFor(colors, mode);
  root.style.setProperty("--color-accent", colors.accent);
  root.style.setProperty("--color-focus", colors.accent);
  root.style.setProperty("--color-brand-red", colors.accent);
  try {
    // The published colour maths throws on a non-hex stored value; the retint then stays on the locked base.
    root.style.setProperty("--color-brand-red-2", mix(colors.accent, colors.background, 0.2));
    root.style.setProperty("--color-on-accent", readableOn(colors.accent, "#ffffff", "#161616"));
  } catch {
    root.style.removeProperty("--color-brand-red-2");
    root.style.removeProperty("--color-on-accent");
  }
  for (const [key, value] of Object.entries(colors)) root.style.setProperty(`--cr-imported-${key}`, value);
}

export interface ImportedThemeStoreState {
  themes: Record<string, ImportedTheme>;
  activeThemeId: string | null;
  mode: ThemeMode;
  install: (theme: ImportedTheme) => void;
  installFromVscodeText: (text: string, options?: { label?: string; source?: ImportedTheme["source"] }) => ImportedTheme;
  remove: (id: string) => void;
  setActiveTheme: (id: string | null) => void;
  setMode: (mode: ThemeMode) => void;
  reload: () => void;
}

export const useImportedThemeStore = create<ImportedThemeStoreState>((set, get) => ({
  themes: readStoredThemes(),
  activeThemeId: readActiveTheme(),
  mode: readMode(),
  install(theme) {
    if (!isValidImportedTheme(theme)) throw new Error("Theme is missing required colors.");
    const themes = { ...get().themes, [theme.id]: theme };
    persistThemes(themes);
    persistValue(ACTIVE_KEY, theme.id);
    set({ themes, activeThemeId: theme.id });
  },
  installFromVscodeText(text, options) {
    const theme = buildImportedThemeFromVscode(text, options);
    get().install(theme);
    return theme;
  },
  remove(id) {
    if (!get().themes[id]) return;
    const themes = { ...get().themes };
    delete themes[id];
    const activeThemeId = get().activeThemeId === id ? null : get().activeThemeId;
    persistThemes(themes);
    if (activeThemeId === null) persistValue(ACTIVE_KEY, null);
    set({ themes, activeThemeId });
  },
  setActiveTheme(id) {
    const activeThemeId = id && get().themes[id] ? id : null;
    persistValue(ACTIVE_KEY, activeThemeId);
    set({ activeThemeId });
  },
  setMode(mode) {
    persistValue(MODE_KEY, mode);
    set({ mode });
  },
  reload() {
    set({ themes: readStoredThemes(), activeThemeId: readActiveTheme(), mode: readMode() });
  },
}));

export function getActiveImportedTheme(state: Pick<ImportedThemeStoreState, "themes" | "activeThemeId"> = useImportedThemeStore.getState()): ImportedTheme | null {
  return state.activeThemeId ? state.themes[state.activeThemeId] ?? null : null;
}

export const IMPORTED_THEME_STORAGE_KEYS = [STORAGE_KEY, ACTIVE_KEY, MODE_KEY] as const;
export { DEFAULT_IMPORTED_COLORS };
