import { useEffect, useMemo } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { useSettings } from "../hooks/useSettings.js";
import { sectionClient } from "../settings/client.js";
import { dictationClient } from "../host.js";
import { focusConnectedOrPageDestination, revealSetting, type SettingEntry, type ShellPlatform } from "./settingsRegistry.js";

/** A palette entry, as `@rightkit/react-shell`'s command palette takes it. */
export interface PaletteCommand {
  id: string;
  label: string;
  hint?: string;
  keywords?: string[];
  restoreFocusAfterRun?: boolean;
  run: () => void;
}


// The shell's command palette unmounts without returning focus, so after
// Escape/outside-click (or a palette navigation) focus drops to <body>.
// Remember the last element focused outside the palette; when the palette
// leaves the DOM and nothing holds focus, send focus back to it, else to the
// current page heading (#50).
export function usePaletteFocusRestore(): void {
  useEffect(() => {
    let opener: HTMLElement | null = null;
    const onFocusIn = (event: FocusEvent) => {
      const target = event.target;
      if (target instanceof HTMLElement && !target.closest(".rk-palette")) opener = target;
    };
    document.addEventListener("focusin", onFocusIn);
    let paletteOpen = false;
    const observer = new MutationObserver(() => {
      const open = document.querySelector(".rk-palette") !== null;
      if (paletteOpen && !open) {
        const saved = opener;
        window.setTimeout(() => {
          const active = document.activeElement;
          if (!active || active === document.body) focusConnectedOrPageDestination(saved);
        }, 0);
      }
      paletteOpen = open;
    });
    observer.observe(document.body, { childList: true, subtree: true });
    return () => {
      document.removeEventListener("focusin", onFocusIn);
      observer.disconnect();
    };
  }, []);
}

// Command palette entries: every registered setting row (selecting one navigates to its page and reveals the row), plus
// the global dictation actions. Kept in a useMemo so the palette's own useMemo is not invalidated on every render. Callers
// must invoke this hook above any early return (rules of hooks).
export function usePaletteCommands<Page extends string>(options: {
  /** The host's registry filter (`createSettingsRegistry(...).settingsForPlatform`). */
  settingsForPlatform: (platform: ShellPlatform, isPro: boolean, developerMode: boolean) => SettingEntry<Page>[];
  /** Page names for the hint ("Home › Dictate into any app"); a page without one shows its key. */
  pageLabels: Partial<Record<Page, string>>;
  handleTabChange: (next: Page, settingId?: string) => void;
}): PaletteCommand[] {
  const { settingsForPlatform, pageLabels, handleTabChange } = options;
  const platform = getPlatform() === "unknown" ? "linux" : getPlatform();
  const isPro = sectionClient("license").monitor.get().isPro;
  // Developer rows render only in developer mode, so the palette lists them only then too.
  const developerMode = useSettings().get("developer_mode", false);
  const settings = useMemo(
    () => settingsForPlatform(platform as ShellPlatform, isPro, developerMode),
    [settingsForPlatform, platform, isPro, developerMode],
  );
  return useMemo(
    () => [
      ...settings.map((s) => ({
        id: `setting:${s.id}`,
        label: s.label,
        hint: `${pageLabels[s.page] ?? s.page} › ${s.group}`,
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
        run: () => void dictationClient().run("startDictation").catch(() => {}),
      },
      {
        id: "act:cancel",
        label: "Cancel dictation",
        hint: "Action",
        restoreFocusAfterRun: true,
        run: () => void dictationClient().run("cancelDictation").catch(() => {}),
      },
    ],
    [handleTabChange, settings, pageLabels],
  );
}
