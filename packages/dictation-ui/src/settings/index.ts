// @rightkit/dictation-ui/settings: the settings sections of a dictation app that have no counterpart in
// `@rightkit/settings-ui`'s standard sections (speech recognition, audio, application, Apple sync, sync provider,
// developer tools, devices), and the controls they are built from. They read the host through the same
// `configureDictationHost` as the pages (`DictationClient` groups `app`, `speech`, `developer`, `sync`, `account`).

export * from "./client.js";
export { Toggle, SectionToggle, type ToggleProps } from "./Toggle.js";
export { SegmentedControl, type SegmentedOption } from "./SegmentedControl.js";
export { MicDevicePicker } from "./MicDevicePicker.js";
export { MicAudioSettings } from "./MicAudioSettings.js";
export { ApplicationSettings } from "./ApplicationSettings.js";
export { HistoryKeyRotationRow } from "./HistoryKeyRotationRow.js";
export { SyncProviderSettings } from "./SyncProviderSettings.js";
export { DeveloperSettings } from "./DeveloperSettings.js";
export { SpeechRecognitionSettings } from "./speech/SpeechRecognitionSettings.js";
export * from "./speech/ModelCard.js";
export { MODELS, PICK_LANGUAGES, TDT_LANGUAGES, WHISPER_LANGUAGE_COUNT, BOX_BASE, type DlState, type WhisperDlState } from "./speech/options.js";
export {
  dismissModelUpdate,
  formatDownloadSize,
  getModelUpdateState,
  hideModelUpdate,
  modelUpdatePhaseLabel,
  openModelUpdatePrompt,
  startModelUpdateDownload,
  subscribeModelUpdate,
  useModelUpdate,
  type ModelUpdateState,
} from "./modelUpdate.js";
export { AppleSyncSettings, CLOUD_SYNC_CHANGED_EVENT } from "./AppleSyncSettings.js";
export { DeviceManagementSettings } from "./DeviceManagementSettings.js";
export { LicenseSettings } from "./LicenseSettings.js";
export { AccountSettings } from "./AccountSettings.js";
