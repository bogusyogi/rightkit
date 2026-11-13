# Changelog

## 0.2.3

Added: `DictationHistory` now renders HeardRight's History screen, so an app can delete its own copy. Needs the matching `@rightkit/library-ui` (its `Unreleased` options).

- Markup, wording and files equal HeardRight's: the toolbar frame and height, the `ErrorBanner` error line, the `ui` button and input classes, "Foreign-device transcript is read-only here", "Collapse transcript" / "Expand transcript", the Delete-all tooltip, "Recorded on <device>", "No history items match your search.", "N of M transcript deletes failed.", the app filter (options sorted by the raw key, shown as the friendly name), and "Export selected": file `<appname>-history-selected-<date>.txt` (from `appName`), filter "Plain text", content with `Target:` (the raw app key) and `Transcript:`.
- `deliveryRow` no longer adds the device as a badge (only "Cancelled" is one; the device is the row's glyph) and sets `groupLabel`. `deliveryLabels()` returns the new wording. `HISTORY_SOURCES` (`mac`, `iphone`, `watch` and their labels) and `useHistorySourceSelection()` (`{ groupSelection, sourceSelection, setSourceSelection }`, `SourceSelection`) are new: the device choice a host's title bar renders, stored in the host's `groupSelection` key and followed by the page.
- Host: `configureDictationHost` forwards `appName`, `storagePrefix` and the storage keys `skipDeleteConfirm`, `groupSelection` and `recoveryDismissed` (new `DictationStorageKey` members) to `@rightkit/library-ui`'s host configuration, so one call configures both. `components.PageShell` is also called for the History page: `page: "history"` (new `DictationPageName` member) and a new `header` prop, the filter row, which the shell frames above `children`. Without a host `PageShell` the page uses the list's own frame.
- `tauriDictationClient`: the save dialog of `saveTextFile` (History's "Export selected", file transcription's .txt) names the `.txt` filter "Plain text" (was "TXT"); other extensions are unchanged.
- Not changed: the app filter key of a synced (`cloud_sync`) row stays the raw process name, window title or "Unknown app" (`deliveryFilterKey`). HeardRight's `targetLabel` sends the device name ("iPhone") for it, but the delivery store's `app_name` is `history_target_label` (`rightkit-transcript-library`), which has no synced arm, so that key matches nothing.

## 0.2.2

- Added: opt-in host options so an app can render the pages with its own look and settings store; defaults are unchanged. `configureDictationHost({ components: { PageShell, Toggle, Kbd, OriginIcon }, classNames: { rowChip }, hooks: { useSettings }, restoreFocus })` with the exported types `DictationComponents`, `DictationClassNames`, `DictationHooks`, `DictationPageShellProps`, `DictationToggleProps`, `DictationOriginIconProps`, `DictationPageName`. `useSettings` (now routed through `hooks.useSettings`; the package's own is `usePackageSettings`) serves Home, Polish and Vocabulary; `PageShell` wraps Dictation, Polish and Home; `restoreFocus(opener)` replaces the TranscribeModal's focus return.
- Fixed: a rejected `DictationClient.openPricing()` (Polish and file-transcription "buy a Pro license" buttons) is logged with `console.warn` instead of surfacing as an unhandled rejection.

- Added: `tauriDictationSettingsBackend(options?)` and `TauriSettingsOptions` from `./tauri`, the settings backend of `tauriDictationClient` on its own (`get_settings_snapshot` / `apply_settings_patch_at`, string-revision handles, `revision_conflict` mapping, window event fan-out, `settingsKeys` key mapping). For an app that renders the settings screens without the hub; it lives in its own module, so it does not pull in the client's other commands. New option `invoke` (default Tauri's) for an app with its own IPC bridge. `tauriDictationClient` uses it internally; its behaviour is unchanged.

## 0.2.1

- Added: `./tauri` entry with `tauriDictationClient(options?)` and `tauriPillClient(options?)`, the Tauri implementation of `DictationClient` and `PillClient`. It makes the native calls HeardRight's web code makes (about 55 commands, 13 events), so an app installs it with one line per window instead of hand-writing the transport. The event subscription is an option (`listen`, default Tauri's) for apps that route events to windows themselves. Pro entitlement and the settings key mapping are host options. New dependencies: `@rightkit/tauri`, `@tauri-apps/api`, `@tauri-apps/plugin-dialog`; no other entry imports them.

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
