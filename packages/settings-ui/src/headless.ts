/**
 * Headless entry (0.4.2): the search state the styled parts compute, as hooks and no-DOM providers, for a host that keeps its own
 * markup and classes. Imports only `react`: no stylesheet, no styled component, no `SettingsPanel`, no `@rightkit/*` package.
 *
 * Hooks render nothing. Each host element is yours; a row, group or section you render registers and reports through these.
 */
export {
  SettingsSearchProvider,
  useSettingsSearch,
  useSettingsRow,
  useSettingsItem,
  useSettingsGroup,
  useSettingsSection,
  useSettingsSearchField,
  useSettingsSearchStatus,
  useSearchKeys,
  highlightText,
  parseSearchTerms,
  defaultAnnouncement,
  GroupScope as SettingsGroupScope,
  SettingsSectionScope,
} from "./search-core.js";
export type {
  SettingsSearchProviderProps,
  SettingsSearchState,
  SettingsSearchEntry,
  SettingsSearchResult,
  SettingsSearchKind,
  SettingsHighlight,
  SettingsRowOptions,
  SettingsItemOptions,
  SettingsRowState,
  SettingsGroupState,
  SettingsSectionState,
  SettingsSearchStatusOptions,
  SettingsSearchFieldOptions,
  SettingsSearchField,
  SettingsSectionScopeProps,
  EscapeClears,
  SearchKeyOptions,
} from "./search-core.js";
