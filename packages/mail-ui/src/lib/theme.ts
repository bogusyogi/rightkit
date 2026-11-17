import { applyBodyFont, createThemeStore, type ThemeChoice } from "@rightkit/react-shell";
import { applyTokens, assertCustomProperty } from "@rightkit/theme";
import { shellSetTheme } from "./ipc/system.js";

export type ThemeMode = "light" | "dark";
export interface ThemePreset {
  id: string;
  mode: ThemeMode;
  tokens: Readonly<Record<string, string>>;
}
export interface FontOption {
  id: string;
  stack: string;
}
export interface MailAppearanceConfig {
  presets: readonly ThemePreset[];
  fonts: readonly FontOption[];
  defaults: { dark: string; light: string; font: string };
  storageKeys: { mode: string; preset: string; font: string };
}

type Selection = ThemePreset | "auto";
type Appearance = {
  config: MailAppearanceConfig;
  choice: Selection;
  font: FontOption;
  themeStore: ReturnType<typeof createThemeStore>;
};
let appearance: Appearance | undefined;

function configured(): Appearance {
  if (!appearance) throw new Error("Mail appearance must be configured before use");
  return appearance;
}

function getItem(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function setItem(key: string, value: string): boolean {
  try {
    localStorage.setItem(key, value);
    return true;
  } catch {
    // A choice is still applied for this session when persistence is unavailable.
    return false;
  }
}

function preset(config: MailAppearanceConfig, id: string | null): ThemePreset | undefined {
  return config.presets.find((item) => item.id === id);
}

function font(config: MailAppearanceConfig, id: string | null): FontOption {
  return config.fonts.find((item) => item.id === id) ?? config.fonts.find((item) => item.id === config.defaults.font)!;
}

function modeOf(choice: Selection): ThemeChoice {
  return choice === "auto" ? "system" : choice.mode;
}

function readSelection(config: MailAppearanceConfig): Selection {
  const { mode, preset: presetKey } = config.storageKeys;
  const storedPreset = getItem(presetKey);
  if (storedPreset === "auto") return "auto";
  const selected = preset(config, storedPreset);
  if (selected) return selected;

  const storedMode = getItem(mode);
  const legacy = preset(config, storedMode);
  if (legacy) {
    if (setItem(presetKey, legacy.id)) setItem(mode, legacy.mode);
    return legacy;
  }
  if (storedMode === "auto") setItem(mode, "system");
  if (storedMode === "auto" || storedMode === "system") return "auto";
  return preset(config, config.defaults[storedMode === "light" ? "light" : "dark"])!;
}

/** Configure before reading the mail store. */
export function configureMailAppearance(config: MailAppearanceConfig) {
  if (appearance) throw new Error("Mail appearance is already configured");
  const { defaults, storageKeys } = config;
  const reserved = new Set(["auto", "system", "light", "dark"]);
  if (
    preset(config, defaults.dark)?.mode !== "dark" ||
    preset(config, defaults.light)?.mode !== "light" ||
    !config.fonts.some((item) => item.id === defaults.font) ||
    new Set(config.presets.map((item) => item.id)).size !== config.presets.length ||
    new Set(config.fonts.map((item) => item.id)).size !== config.fonts.length ||
    config.presets.some((item) => !item.id || reserved.has(item.id)) ||
    config.fonts.some((item) => !item.id) ||
    Object.values(storageKeys).some((key) => !key) ||
    new Set(Object.values(storageKeys)).size !== 3
  ) {
    throw new Error("Invalid mail appearance configuration");
  }
  for (const item of config.presets) {
    for (const token of Object.keys(item.tokens)) {
      assertCustomProperty(token);
      if (token === "--rk-font-body") throw new Error("Use fonts for the body font");
    }
  }

  const choice = readSelection(config);
  const themeStore = createThemeStore({
    storageKey: storageKeys.mode,
    defaultChoice: "dark",
    // Persistence belongs to the combined choice. Seed the shell from that choice even if migration failed.
    storage: {
      getItem: () => modeOf(choice),
      setItem: () => {},
      removeItem: () => {},
    },
  });
  const state = { config, themeStore, choice, font: font(config, getItem(storageKeys.font)) };
  appearance = state;

  let restoreTokens: (() => void) | undefined;
  function paintPreset() {
    const selected = state.choice === "auto" ? preset(config, defaults[themeStore.resolved()])! : state.choice;
    restoreTokens?.();
    restoreTokens = applyTokens(selected.tokens);
    void shellSetTheme(selected.mode).catch(() => {});
  }

  // Covers explicit saves & the shell store's system-media listener.
  themeStore.subscribe(paintPreset);
  function bootTheme() {
    themeStore.apply(themeStore.read());
    paintPreset();
    applyBodyFont(state.font.stack);
  }
  return {
    themeStore: {
      ...themeStore,
      save: (mode: ThemeChoice) => setThemeChoice(mode === "system" ? "auto" : defaults[mode]),
    },
    bootTheme,
  };
}

export function loadThemeChoice(): string {
  const { choice } = configured();
  return choice === "auto" ? choice : choice.id;
}

export function subscribeThemeChoice(listener: (id: string) => void): void {
  configured().themeStore.subscribe(() => listener(loadThemeChoice()));
}

export function setThemeChoice(id: string): void {
  const state = configured();
  const { config, themeStore } = state;
  state.choice = id === "auto" ? id : preset(config, id) ?? preset(config, config.defaults.dark)!;
  const mode = modeOf(state.choice);
  if (setItem(config.storageKeys.preset, loadThemeChoice())) setItem(config.storageKeys.mode, mode);
  themeStore.save(mode);
}

export function loadFontChoice(): string {
  return configured().font.id;
}

export function setFontChoice(id: string): string {
  const state = configured();
  state.font = font(state.config, id);
  setItem(state.config.storageKeys.font, state.font.id);
  applyBodyFont(state.font.stack);
  return state.font.id;
}
