// the host app-only feature (VS Code theme import, activation, persistence): @rightkit/theme 0.3.1 has no imported-theme module, so nothing here duplicates a published RightKit capability.
import { createContext, type ReactNode, useContext, useEffect, useMemo } from "react";

import {
  applyImportedTheme,
  getActiveImportedTheme,
  renderedModeFor,
  type ImportedTheme,
  type ImportedThemeStoreState,
  type ThemeMode,
  useImportedThemeStore,
} from "@rightkit/agent-ui-core/state/importedThemeStore";

/**
 * Hermes lineage: themes/context.tsx.
 * The provider is deliberately separate from the host app existing themeStore:
 * the latter owns the locked shell base, while this provider owns imported
 * palette selection and live accent retinting.
 */

export interface ImportedThemeContextValue {
  activeTheme: ImportedTheme | null;
  availableThemes: ImportedTheme[];
  mode: ThemeMode;
  renderedMode: "light" | "dark";
  setActiveTheme: (id: string | null) => void;
  setMode: (mode: ThemeMode) => void;
  installFromVscodeText: ImportedThemeStoreState["installFromVscodeText"];
  remove: (id: string) => void;
}

const ImportedThemeContext = createContext<ImportedThemeContextValue>({
  activeTheme: null,
  availableThemes: [],
  mode: "dark",
  renderedMode: "dark",
  setActiveTheme: () => undefined,
  setMode: () => undefined,
  installFromVscodeText: () => {
    throw new Error("ImportedThemeProvider is not mounted.");
  },
  remove: () => undefined,
});

function selectTheme(state: Pick<ImportedThemeStoreState, "themes" | "activeThemeId">): ImportedTheme | null {
  return getActiveImportedTheme(state);
}

export function ImportedThemeProvider({ children }: { children: ReactNode }) {
  const themes = useImportedThemeStore((state) => state.themes);
  const activeThemeId = useImportedThemeStore((state) => state.activeThemeId);
  const mode = useImportedThemeStore((state) => state.mode);
  const setActiveTheme = useImportedThemeStore((state) => state.setActiveTheme);
  const setMode = useImportedThemeStore((state) => state.setMode);
  const installFromVscodeText = useImportedThemeStore((state) => state.installFromVscodeText);
  const remove = useImportedThemeStore((state) => state.remove);
  const reload = useImportedThemeStore((state) => state.reload);

  const activeTheme = useMemo(() => selectTheme({ themes, activeThemeId }), [themes, activeThemeId]);
  const renderedMode = activeTheme
    ? renderedModeFor(mode === "dark" ? activeTheme.darkColors ?? activeTheme.colors : activeTheme.colors, mode)
    : "dark";

  useEffect(() => {
    applyImportedTheme(activeTheme, mode);
  }, [activeTheme, mode]);

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (!event.key || event.key.startsWith("cr.imported-theme-")) reload();
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [reload]);

  const value = useMemo<ImportedThemeContextValue>(
    () => ({
      activeTheme,
      availableThemes: Object.values(themes),
      mode,
      renderedMode,
      setActiveTheme,
      setMode,
      installFromVscodeText,
      remove,
    }),
    [activeTheme, themes, mode, renderedMode, setActiveTheme, setMode, installFromVscodeText, remove],
  );

  return <ImportedThemeContext.Provider value={value}>{children}</ImportedThemeContext.Provider>;
}

export function useImportedTheme(): ImportedThemeContextValue {
  return useContext(ImportedThemeContext);
}
