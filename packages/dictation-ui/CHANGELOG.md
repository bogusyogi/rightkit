# Changelog

## 0.2.1

- Fix: History's app filter matches. `deliveryRow` sets `origin.filterKey` to the key the delivery store's `app_name` filter matches (`deliveryFilterKey`: the file label for a file transcription, else the raw process name, window title or "Unknown app"), so `deliveryQueryArgs` sends that key as `app` instead of the friendly name. Needs `@rightkit/library-ui` 0.2.1 (`LibraryOrigin.filterKey`).

## 0.2.0 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-remaining.md`; design in `reports/dictation-library-ui-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

The review judged this package the wrong shape: one app's whole front end (sidebar, title bar, licence, auto-update, theme, legal, settings registry, pill, pages) with a second app's voice loop beside it. It is redrawn as two units under the published name: the dictation pill (entry `./pill`) with an event/command interface, and the dictation pages over a typed `DictationClient` the host supplies. The voice conversation moved out.

Added:
- `host`: `configureDictationHost({ appName, client, pill, storagePrefix, storageKeys, formattingGuideUrl })`, `DictationClient` (settings through the `@rightkit/settings-ui` `SettingsBackend`, history as a `@rightkit/library-ui` `LibrarySource`, one method per operation, typed events through `on`), `PillClient` (pill actions, geometry, intro, terminal reset, popups, typed events through `on`). No IPC and no command or event names in the package.
- `./pill` entry: `PillOverlay`, `HintPopup`, `MiniClipboardPopup` and the pill hooks; it imports nothing from the pages.
- `DictationHistory` (the History page as `@rightkit/library-ui`'s `LibraryScreen` with the Dictations/Cancelled variants, device groups and transcript wording) and `history/deliveryRows` (`deliveryRow`, `deliveryQueryArgs`, `historyDevice`, `isLocallyDeletable`, `targetDisplay`, ...).
- `TranscribeSurface` and `TranscribeModal`, moved from `@rightkit/library-ui`.
- `useDictationEvent`, `lib/chords` (the dictation chords; rendering is `Kbd` from `@rightkit/platform-ui/react`).

Removed (breaking), each a copy of code another package owns:
- Licence: `lib/license*`, `lib/license/*`, `lib/licenseLifecycle`, `LicenseLifecycleDialog` → `@rightkit/license`; the pages read entitlement from `DictationClient.entitlement()`.
- Auto-update: `lib/autoUpdates`, `lib/heardRightAutoUpdateClient`, `lib/relaunchForUpdate`, `SidebarUpdateStatus` → `@rightkit/app-update`.
- Theme: `lib/theme` → `@rightkit/theme` and the host.
- Legal: `lib/legal`, the `@rightkit-host` legal aliases → `@rightkit/legal` / `@rightkit/legal-ui`.
- App frame: `Sidebar`, `TitleBar`, `WindowControls`, `PageShell`, `CommandPalette`, `usePaletteCommands`, `usePaletteFocusRestore`, `renderCrash`, `lib/fieldStore` → `@rightkit/react-shell` (`AppShell`, `Titlebar`, `CaptionControls`, `PageShell`, `CommandPalette`, `ErrorBoundary`) and the host.
- Settings: `lib/settingsRegistry`, `hooks/settingsSync` and the settings store in `useSettings` → `@rightkit/settings-ui` (`useSettings` over the host's `SettingsBackend`; this package's `useSettings` is a typed view of it). `components/settings/SegmentedControl`, `components/ui/Toggle` → `@rightkit/react-shell`. `Kbd` → `@rightkit/platform-ui/react`. `hooks/usePlatform` → `@rightkit/platform-ui`.
- IPC: `lib/tauri`, `lib/tauriBridge` (with the `heardright://renderer-event` bridge), `hooks/useTauriEvent`, `lib/logs`.
- HeardRight app code with no place in a dictation package: `FeatureReel`, `featureReelState`, `HubLoadingHint`, `CommandControls`, `HoverTooltip`, `assets/trayIconDataUri`, `lib/modelUpdate`, `lib/legacyPromptHint`, `lib/config`, `lib/syncProviderFlag`.
- `appName` (`setHostAppName`): use `configureDictationHost({ appName })`.
- `src/voice` (`VoiceConversation`, `useVoiceConversation`, `useMicRecorder`, and its copy of the agent icon set) moved to `@rightkit/agent-ui-components` 0.1.1, which used it.
- History helpers moved next to the list in `@rightkit/library-ui` (`AppIcon` → `OriginIcon`, date and filter helpers → `list/format`); the dictation-record half is `history/deliveryRows`.
- The `./components/*`, `./hooks/*`, `./lib/*`, `./pages/*`, `./voice/*` subpath exports: the entries are `.` and `./pill`.

Changed (breaking):
- Storage keys come from the host (`storageKeys`; default `rk-dictation.<key>`): the prompt-hint latches were `hr_prompt_hint_*`. The History keys (`heardright.skipDeleteHistoryConfirm`, `heardright.historySourceSelection.v1`, `hr.historyRecoveryDismissed.v1`) are `@rightkit/library-ui` host keys.
- Pill actions are typed (`PillAction`: `startDictation`, `cancelDictation`, `stopDictation`, `stopAndSend`, `openHubHome`, `openHubHistory`, `openClipboard`); the host maps them to its commands.
- The hint catalog names the product as `{app}`, filled from the host name.
- The Dictation page links the formatting guide only when the host passes `formattingGuideUrl`.

Not changed here, recorded: the `hr-*` CSS class names the pill and pages use (they need a stylesheet migration in the host), and `lib/ui`'s class table.

Deprecation: 0.1.0 on the registry is the whole HeardRight front end; once 0.2.0 is published, 0.1.0 should be deprecated with a pointer to 0.2.0, `@rightkit/library-ui` 0.2.0 and `@rightkit/agent-ui-components` 0.1.1.

## 0.1.0

- First release. Dictation UI: recording pill, microphone and hotkey hooks, voice conversation and dictation pages.
