import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  applyImportedTheme,
  useImportedThemeStore,
} from "@rightkit/agent-ui-core/state/importedThemeStore";
import { ImportedThemeProvider, useImportedTheme } from "./ImportedThemeProvider.js";

const theme = {
  id: "vsc-test",
  label: "Test",
  description: "Imported from VS Code",
  source: "vscode" as const,
  colors: {
    background: "#101010",
    foreground: "#eeeeee",
    accent: "#ff0055",
    surface: "#202020",
    border: "#505050",
    muted: "#aaaaaa",
    destructive: "#ff3333",
  },
};

function Probe() {
  const { activeTheme, availableThemes, renderedMode } = useImportedTheme();
  return <output data-testid="theme">{`${activeTheme?.label ?? "none"}:${availableThemes.length}:${renderedMode}`}</output>;
}

describe("ImportedThemeProvider", () => {
  beforeEach(() => {
    useImportedThemeStore.setState({ themes: { [theme.id]: theme }, activeThemeId: theme.id, mode: "dark" });
    applyImportedTheme(null, "dark");
  });

  afterEach(() => cleanup());

  it("exposes the active theme and applies its live retint", () => {
    render(<ImportedThemeProvider><Probe /></ImportedThemeProvider>);
    expect(screen.getByTestId("theme")).toHaveTextContent("Test:1:dark");
    expect(document.documentElement.dataset.importedTheme).toBe(theme.id);
    expect(document.documentElement.style.getPropertyValue("--color-accent")).toBe(theme.colors.accent);
  });

  it("repaints when the selected imported theme changes", () => {
    render(<ImportedThemeProvider><Probe /></ImportedThemeProvider>);
    act(() => {
      useImportedThemeStore.getState().setMode("light");
    });
    expect(screen.getByTestId("theme")).toHaveTextContent("Test:1:light");
    expect(document.documentElement.dataset.importedThemeMode).toBe("light");
  });
});
