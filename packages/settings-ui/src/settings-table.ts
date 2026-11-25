// Lookups over a host's searchable-setting table: the rows of one tab (for that tab's search) and the tab that holds a
// row (for a deep link). Pure; imports only `./reveal.js`.
import { filterSettingEntries, type SettingEntryScope } from "./reveal.js";

/** The fields these lookups read. */
export interface SectionedSettingEntry<Page extends string = string, Section extends string = string, P extends string = string>
  extends SettingEntryScope<P> {
  id: string;
  label: string;
  keywords?: readonly string[];
  page: Page;
  /** The tab that holds the row; absent for rows outside any tab (a title-bar control). */
  section?: Section;
}

/**
 * The searchable rows of one tab: entries on `page` (default `"settings"`) whose `section` is `section`, in scope for the
 * platform and developer mode. Licence tier is not filtered unless `isPro` is passed: a tab lists the free layout. Each
 * result is `{ id, label, keywords }`, the shape `SettingsSection.entries` takes (a missing `keywords` becomes `[]`).
 */
export function settingsEntriesForSection<Page extends string, Section extends string, P extends string, E extends SectionedSettingEntry<Page, Section, P>>(
  entries: readonly E[],
  section: Section,
  scope: { platform: P; developerMode: boolean; isPro?: boolean },
  page: Page = "settings" as Page,
): { id: string; label: string; keywords: string[] }[] {
  return filterSettingEntries<P, E>(entries, { platform: scope.platform, isPro: scope.isPro ?? false, developerMode: scope.developerMode })
    .filter((entry) => entry.page === page && entry.section === section)
    .map(({ id, label, keywords }) => ({ id, label, keywords: [...(keywords ?? [])] }));
}

/** The tab that holds row `id` on `page` (default `"settings"`), or undefined for a row outside any tab. Not scope-filtered. */
export function settingsSectionFor<Page extends string, Section extends string, E extends SectionedSettingEntry<Page, Section>>(
  entries: readonly E[],
  id: string,
  page: Page = "settings" as Page,
): Section | undefined {
  return entries.find((entry) => entry.page === page && entry.id === id)?.section;
}
