// @rightkit/settings-ui/core: the panel shell, primitives, search, backends and types. Imports only `react`: no
// @rightkit/license, logs, theme, updates or app-shell (guarded by core-entry.test.ts).
export * from "./types.js";
export * from "./backends.js";
export * from "./useSettings.js";
export { SettingsGroup, SettingRow, ProgressBar, formatBytes } from "./primitives.js";
export type { SettingRowProps, SettingsGroupProps } from "./primitives.js";
export { SettingsPanel, SectionFrame, mergeSections, orderSections, searchSections, revealSetting } from "./SettingsPanel.js";
export type { SettingsPanelProps, NavItemState, SearchMode } from "./SettingsPanel.js";
export {
  SettingsSearchProvider,
  SettingsItem,
  SettingsSectionScope,
  SettingsSearchSection,
  HighlightedText,
  SettingsResultsAnnouncer,
  useSettingsSearch,
  useSearchKeys,
  parseSearchTerms,
  highlightText,
  defaultAnnouncement,
} from "./search.js";
export type {
  SettingsSearchProviderProps,
  SettingsSearchState,
  SettingsSearchEntry,
  SettingsSearchResult,
  SettingsSearchKind,
  SettingsHighlight,
  SettingsItemProps,
  SettingsSectionScopeProps,
  SettingsSearchSectionProps,
  EscapeClears,
  SearchKeyOptions,
} from "./search.js";
export { sectionIcons } from "./icons.js";
