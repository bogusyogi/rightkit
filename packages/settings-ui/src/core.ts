// @rightkit/settings-ui/core: the panel shell, primitives, search, backends and types. Imports only `react`: no
// @rightkit/license, logs, theme, updates or app-shell (guarded by core-entry.test.ts).
export * from "./types.js";
export * from "./backends.js";
export * from "./useSettings.js";
export * from "./preferences.js";
export { SettingsGroup, SettingRow, ProgressBar, formatBytes, SelectControl, PathControl, IndexProgressLine } from "./primitives.js";
export type { SettingRowProps, SettingsGroupProps, SelectControlProps, PathControlProps, IndexProgressLineProps } from "./primitives.js";
export { SettingsPanel, SectionFrame, mergeSections, orderSections, searchSections, revealSetting } from "./SettingsPanel.js";
export type { SettingsPanelProps, NavItemState, SearchMode, SettingsClassNames } from "./SettingsPanel.js";
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
export { prepPercent } from "./model-prep.js";
export { createModelUpdateStore } from "./model-update-store.js";
export type {
  ModelUpdatePhase,
  ModelUpdateState,
  ModelUpdateStatus,
  ModelUpdateStore,
  ModelUpdateStoreOptions,
} from "./model-update-store.js";
export { useMicDevices } from "./mic-devices.js";
export type { UseMicDevices } from "./mic-devices.js";
export { revealSettingWhenMounted, filterSettingEntries } from "./reveal.js";
export type { RevealSettingOptions, SettingEntryScope } from "./reveal.js";
export { useRemovalFlow, focusRemoveButton } from "./removal.js";
export type { RemovalFlow, UseRemovalFlowOptions } from "./removal.js";
export { useKeyValueList } from "./key-value-list.js";
export type { KeyValueList, UseKeyValueListOptions } from "./key-value-list.js";
export { useVocabulary } from "./vocabulary.js";
export type { Vocabulary, VocabularyPort, VocabularySuggestionLike, VocabularyTerm, UseVocabularyOptions } from "./vocabulary.js";
export { useProviderKeys } from "./provider-keys.js";
export type { ProviderKeyCheck, ProviderKeysMessages, ProviderKeysPort, UseProviderKeysOptions } from "./provider-keys.js";
export { useDeviceSync, syncStatusLabel, summarizeSync } from "./device-sync.js";
export type { DeviceSyncPort, SyncBridgeStatus, SyncLabels, SyncTone, TranscriptSyncStatus, UseDeviceSyncOptions } from "./device-sync.js";
export { useSpeechModels } from "./speech-models.js";
export type {
  AltModelDownloadState,
  AltModelEvent,
  AltModelStatus,
  LocalModelEvent,
  LocalModelStatusLike,
  ModelDownloadState,
  ModelPhase,
  SharedModelProgress,
  SpeechModels,
  SpeechModelsOptions,
  SpeechModelsPort,
} from "./speech-models.js";
export { parseSavedMicDevice, micDeviceSettingValue, resolveMicSelection, SYSTEM_DEFAULT_MIC } from "./mic-device-options.js";
export type { MicDeviceLike, MicDeviceOption, MicDeviceSelection, SavedMicDeviceParts } from "./mic-device-options.js";
export { useModelUpdateTriggers } from "./model-update-triggers.js";
export type { ModelUpdateAvailablePayload, UseModelUpdateTriggersOptions } from "./model-update-triggers.js";
export { useDeveloperTools } from "./developer-tools.js";
export type {
  CacheNotice,
  DeveloperTools,
  DeveloperToolsMessages,
  DeveloperToolsPort,
  DiagnosticsStatus,
  ExperimentalFlagStatus,
  UseDeveloperToolsOptions,
} from "./developer-tools.js";
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
export { useSettingsPage, visibleTabSections, settingPatcher, settingString, settingStringOrUndefined, settingFlag, settingNumber, settingOr } from "./settings-page.js";
export type { SettingsPageState, UseSettingsPageOptions } from "./settings-page.js";
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
