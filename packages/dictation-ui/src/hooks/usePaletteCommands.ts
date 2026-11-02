import { useMemo } from "react";

import { invoke } from "../lib/tauri.js";
import {
  revealSetting,
  settingsForPlatform,
} from "../lib/settingsRegistry.js";
import type { Command } from "../components/CommandPalette.js";
import type { TabKey } from "../components/Sidebar.js";
import { getLicenseLifecycle } from "../lib/licenseLifecycle.js";
import { useSettings } from "./useSettings.js";
import { getPlatform } from "./usePlatform.js";

const PAGE_LABELS: Partial<Record<TabKey, string>> = {
  home: "Home",
  history: "History",
  controls: "Controls",
  dictation: "Dictation",
  polish: "Polish",
  settings: "Settings",
  about: "About",
};

// Command palette entries: every registered setting row (selecting one
// navigates to its page and reveals the row), plus global dictation actions.
// Kept in a useMemo so CommandPalette's own useMemo is not
// invalidated on every App render. Callers must invoke this hook above any
// early return (rules of hooks) — App relies on that ordering.
export function usePaletteCommands(
  handleTabChange: (next: TabKey, settingId?: string) => void,
): Command[] {
  const platform = getPlatform();
  const isPro = getLicenseLifecycle().isPro;
  // Developer rows render only in developer mode, so the palette lists them only then too.
  const developerMode = useSettings().get("developer_mode", false);
  const settings = useMemo(
    () => settingsForPlatform(platform, isPro, developerMode),
    [platform, isPro, developerMode],
  );
  return useMemo(
    () => [
      ...settings.map((s) => ({
        id: `setting:${s.id}`,
        label: s.label,
        hint: `${PAGE_LABELS[s.page] ?? s.page} › ${s.group}`,
        keywords: s.keywords,
        run: () => {
          handleTabChange(s.page, s.page === "settings" ? s.id : undefined);
          revealSetting(s.id);
        },
      })),
      {
        id: "act:start",
        label: "Start dictation",
        hint: "Action",
        restoreFocusAfterRun: true,
        run: () => void invoke("start_dictation").catch(() => {}),
      },
      {
        id: "act:cancel",
        label: "Cancel dictation",
        hint: "Action",
        restoreFocusAfterRun: true,
        run: () => void invoke("cancel_dictation").catch(() => {}),
      },
    ],
    [handleTabChange, settings],
  );
}
