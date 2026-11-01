import { beforeEach, describe, expect, it } from "vitest";

import {
  applyImportedTheme,
  buildThemeFromMarketplace,
  parseVscodeTheme,
  useImportedThemeStore,
  vscodeThemeSlug,
} from "./importedThemeStore";

const vscode = (type: string, background: string, foreground: string, accent: string) => JSON.stringify({
  type,
  colors: {
    "editor.background": background,
    "editor.foreground": foreground,
    "button.background": accent,
  },
});

describe("imported theme store", () => {
  beforeEach(() => {
    localStorage.clear();
    useImportedThemeStore.setState({ themes: {}, activeThemeId: null, mode: "dark" });
    applyImportedTheme(null, "dark");
  });

  it("accepts JSONC and keeps a stable VS Code slug", () => {
    expect(parseVscodeTheme('{\n // comment\n "colors": { "editor.background": "#123456", },\n}').colors).toEqual({
      "editor.background": "#123456",
    });
    expect(vscodeThemeSlug("Solarized Light")).toBe("vsc-solarized-light");
  });

  it("imports, persists, and selects a palette", () => {
    const theme = useImportedThemeStore.getState().installFromVscodeText(vscode("dark", "#101010", "#f0f0f0", "#ff0055"));
    expect(theme.id).toBe("vsc-theme");
    expect(useImportedThemeStore.getState().activeThemeId).toBe(theme.id);
    expect(JSON.parse(localStorage.getItem("cr.imported-themes.v1") ?? "{}")[theme.id].colors.accent).toBe("#ff0055");

    useImportedThemeStore.getState().reload();
    expect(useImportedThemeStore.getState().themes[theme.id].label).toBe("VS Code Theme");
  });

  it("retints the accent chain without replacing the locked neutral base", () => {
    const theme = useImportedThemeStore.getState().installFromVscodeText(vscode("dark", "#101010", "#f0f0f0", "#ff0055"));
    applyImportedTheme(theme, "dark");
    expect(document.documentElement.dataset.importedTheme).toBe(theme.id);
    expect(document.documentElement.style.getPropertyValue("--color-accent")).toBe("#ff0055");
    expect(document.documentElement.style.getPropertyValue("--color-bg")).toBe("");
  });

  it("folds marketplace light and dark variants into one imported family", () => {
    const theme = buildThemeFromMarketplace({
      extensionId: "x.solarized",
      displayName: "Solarized",
      themes: [
        { uiTheme: "vs", contents: vscode("light", "#ffffff", "#111111", "#0055aa") },
        { uiTheme: "vs-dark", contents: vscode("dark", "#101010", "#eeeeee", "#ff0055") },
      ],
    });
    expect(theme.id).toBe("vsc-solarized");
    expect(theme.colors.background).toBe("#ffffff");
    expect(theme.darkColors?.background).toBe("#101010");
  });
});
