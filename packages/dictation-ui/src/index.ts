// @rightkit/dictation-ui: the dictation pages over a host-supplied `DictationClient`, and (on the
// `./pill` entry) the recording pill over a `PillClient`. No IPC, no app frame: the shell, licence,
// updates, theme and legal screens come from their own packages.

export * from "./host.js";

// Pages
export { default as HomePage } from "./pages/Home.js";
export { default as DictationPage } from "./pages/Dictation.js";
export { default as VocabularyPage } from "./pages/Vocabulary.js";
export { default as PolishPage } from "./pages/Polish.js";
export { TranscribeSurface } from "./pages/Transcribe.js";
export { default as TranscribeModal } from "./components/TranscribeModal.js";
export { default as ApiKeysPanel } from "./pages/polish/ApiKeysPanel.js";
export * from "./pages/polish/PolishCards.js";
export { DictationHistory } from "./history/DictationHistory.js";
export * from "./history/deliveryRows.js";
export * from "./history/useHistorySourceSelection.js";

// Page parts
export { default as AccessibilityBanner } from "./components/AccessibilityBanner.js";
export { default as ErrorBanner } from "./components/ErrorBanner.js";
export { default as HotkeyConflictBanner } from "./components/HotkeyConflictBanner.js";
export { default as ReplacementsSettings } from "./components/settings/ReplacementsSettings.js";
export { default as SnippetsSettings } from "./components/settings/SnippetsSettings.js";
export * from "./components/settings/CompactSettingsGroup.js";
export * from "./components/settings/DictationSection.js";

// Hooks and helpers
export * from "./hooks/useDictationEvent.js";
export * from "./hooks/useHomeDashboard.js";
export * from "./hooks/useMicDevices.js";
export * from "./hooks/useSettings.js";
export * from "./lib/chords.js";
export * from "./lib/homeMetrics.js";
export * from "./lib/modelPrep.js";
export * from "./lib/privacyDisclosure.js";
export * from "./lib/pushToTalk.js";
export * from "./lib/sounds.js";
export * from "./lib/time.js";
export * from "./lib/ui.js";
export * from "./types/settings.js";
export * from "./types/vocabulary.js";
