// Turns a host's searchable-setting table into command-palette entries: one command per row that opens the row's page and
// reveals the row. Pure, plus one memoising hook. Imports only `react`; the `Command` shape is structural so a host can
// pass the result straight to `@rightkit/react-shell`'s command palette without this package depending on it.
import { useMemo } from "react";
import { filterSettingEntries, type SettingEntryScope } from "./reveal.js";

/** The fields `settingsPaletteCommands` reads from each table entry. */
export interface PaletteSettingEntry<Page extends string = string, P extends string = string> extends SettingEntryScope<P> {
  /** Matches the row's `data-setting-id` attribute. */
  id: string;
  label: string;
  keywords?: readonly string[];
  /** The host page that holds the row. */
  page: Page;
  /** Group name shown after the page name in the hint. */
  group: string;
}

/** A palette command; structurally the same as `Command` in `@rightkit/react-shell`. */
export interface SettingsPaletteCommand {
  id: string;
  label: string;
  hint?: string;
  keywords?: readonly string[];
  restoreFocusAfterRun?: boolean;
  run: () => void;
}

export interface SettingsPaletteOptions<Page extends string, P extends string> {
  /** Platform, licence tier and developer mode; rows whose control does not exist in this scope are left out. */
  scope: { platform: P; isPro: boolean; developerMode?: boolean };
  /** Display name per page for the hint (`"Settings › Audio"`). A page without a name shows its key. */
  pageLabels?: Partial<Record<Page, string>>;
  /** Opens `page`. `settingId` is passed only when the page is `settingsPage`, which needs it to pick a tab. */
  navigate: (page: Page, settingId?: string) => void;
  /** Reveals the row after navigating (the host's `revealSettingWhenMounted` with its own flash class). */
  reveal: (settingId: string) => void;
  /** The page whose navigation takes the row id. Default `"settings"`. */
  settingsPage?: Page;
  /** Prefix of each command id. Default `"setting:"`. */
  idPrefix?: string;
  /** Separator between page name and group in the hint. Default `" › "`. */
  hintSeparator?: string;
  /** Commands appended after the setting commands (global actions). */
  actions?: readonly SettingsPaletteCommand[];
}

/**
 * One command per row in scope, then `actions`. Choosing a row calls `navigate(page, id)` (the id only for
 * `settingsPage`) and then `reveal(id)`. Rebuild only when the inputs change, so a palette that memoises its own list is
 * not invalidated on every render: use `useSettingsPaletteCommands`.
 */
export function settingsPaletteCommands<Page extends string, P extends string, E extends PaletteSettingEntry<Page, P>>(
  entries: readonly E[],
  options: SettingsPaletteOptions<Page, P>,
): SettingsPaletteCommand[] {
  const { scope, pageLabels, navigate, reveal, settingsPage = "settings" as Page, idPrefix = "setting:", hintSeparator = " › ", actions = [] } = options;
  const rows = filterSettingEntries<P, E>(entries, scope);
  return [
    ...rows.map((entry): SettingsPaletteCommand => ({
      id: `${idPrefix}${entry.id}`,
      label: entry.label,
      hint: `${pageLabels?.[entry.page] ?? entry.page}${hintSeparator}${entry.group}`,
      keywords: entry.keywords,
      run: () => {
        navigate(entry.page, entry.page === settingsPage ? entry.id : undefined);
        reveal(entry.id);
      },
    })),
    ...actions,
  ];
}

/**
 * `settingsPaletteCommands` in a `useMemo` over the platform, licence tier, developer mode, `navigate`, `reveal`,
 * `actions` and `entries`. Pass stable `navigate`, `reveal` and `actions` (a module function, `useCallback`, a constant
 * array) or the list is rebuilt each render. Call it above any early return.
 */
export function useSettingsPaletteCommands<Page extends string, P extends string, E extends PaletteSettingEntry<Page, P>>(
  entries: readonly E[],
  options: SettingsPaletteOptions<Page, P>,
): SettingsPaletteCommand[] {
  const { scope, pageLabels, navigate, reveal, settingsPage, idPrefix, hintSeparator, actions } = options;
  const { platform, isPro, developerMode = false } = scope;
  return useMemo(
    () =>
      settingsPaletteCommands(entries, {
        scope: { platform, isPro, developerMode },
        pageLabels,
        navigate,
        reveal,
        settingsPage,
        idPrefix,
        hintSeparator,
        actions,
      }),
    [entries, platform, isPro, developerMode, pageLabels, navigate, reveal, settingsPage, idPrefix, hintSeparator, actions],
  );
}
