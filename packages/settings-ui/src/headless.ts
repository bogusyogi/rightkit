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
  useSettingsHit,
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
  SettingsGroupOptions,
  SettingsItemOptions,
  SettingsHitOptions,
  SettingsHitState,
  SettingsHiddenProps,
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

// Headless-safe additions (0.7.x): no DOM, no stylesheet, only `react` or nothing.
export { settingsPaletteCommands, useSettingsPaletteCommands } from "./palette-commands.js";
export type { PaletteSettingEntry, SettingsPaletteCommand, SettingsPaletteOptions } from "./palette-commands.js";
export { settingsEntriesForSection, settingsSectionFor } from "./settings-table.js";
export type { SectionedSettingEntry } from "./settings-table.js";
export { useFeedbackForm, feedbackErrorText } from "./feedback-form.js";
export type { FeedbackForm, FeedbackStatus, UseFeedbackFormOptions } from "./feedback-form.js";
export { useKeyPanelDisclosure } from "./key-panel.js";
export type { KeyPanelDisclosure, UseKeyPanelDisclosureOptions } from "./key-panel.js";
export { useLaunchAtLogin } from "./launch-at-login.js";
export type { LaunchAtLogin, UseLaunchAtLoginOptions } from "./launch-at-login.js";
export { mapLegacyValue, flagKeyChoiceValue, writeFlagKeyChoice, withSelection } from "./setting-choices.js";
export type { FlagKeyChoice, WriteFlagKeyChoiceOptions } from "./setting-choices.js";
export { onEnterKey, removalDialogProps, useFocusWhen } from "./form-handlers.js";
export type { RemovalDialogProps } from "./form-handlers.js";
export { bridgeChannelLabels } from "./device-sync.js";
export type { BridgeChannelLabels } from "./device-sync.js";
export { micSelectionLabel } from "./mic-device-options.js";
export type { MicSelectionLabels } from "./mic-device-options.js";
export { useResolvedMicLabel } from "./mic-devices.js";
export { WHISPER_LANGUAGE_CODES, DEFAULT_LANGUAGE_NAME_OVERRIDES, languageDisplayName, languageOptions } from "./languages.js";
export { useConfirmedAction } from "./confirmed-action.js";
export type { ConfirmedAction, ConfirmedActionMessages, ConfirmedActionResult, UseConfirmedActionOptions } from "./confirmed-action.js";
