# @rightkit/dictation-ui

Dictation pill (event/command interface) and dictation pages over a host-supplied DictationClient.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.1 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/dictation-ui` | Host configuration and the `DictationClient` contract; the pages (`HomePage`, `DictationPage`, `VocabularyPage`, `PolishPage`, `TranscribeSurface`, `DictationHistory`) and their parts (banners, replacements, snippets, API keys); delivery-record mapping for the library (`deliveryRow`); hooks (`useSettings`, `useHomeDashboard`, `useMicDevices`, `useDictationEvent`). |
| `@rightkit/dictation-ui/settings` | The settings sections with no `@rightkit/settings-ui` counterpart and their controls: `MicAudioSettings`, `MicDevicePicker`, `ApplicationSettings`, `SyncProviderSettings`, `DeveloperSettings`, `SpeechRecognitionSettings` (with `ModelCard` parts), `AppleSyncSettings`, `DeviceManagementSettings`, the model-update store, `Toggle`, `SegmentedControl`. Read the same host configuration as the pages. |
| `@rightkit/dictation-ui/pill` | The recording pill and its popups (`PillOverlay`, `HintPopup`, `MiniClipboardPopup`) over the `PillClient` contract. Imports nothing from the pages. |

## Host configuration

The package holds no IPC, no command or event names and no app frame. Each window calls
`configureDictationHost` once before rendering:

```ts
import { configureDictationHost } from "@rightkit/dictation-ui"; // or "@rightkit/dictation-ui/pill" in a pill window

configureDictationHost({
  appName: "Your App",
  client,              // DictationClient: pages (hub window)
  pill,                // PillClient: pill and popup windows
  storageKeys: { promptHintCount: "your-app.promptHintCount" }, // keep existing stored values
  formattingGuideUrl: "https://example.com/formatting",
});
```

- `DictationClient`: `settings` (a `@rightkit/settings-ui` `SettingsBackend`), `history` (a `@rightkit/library-ui`
  `LibrarySource` whose rows come from `deliveryRow`), one method per operation (vocabulary, AI keys, microphones,
  hotkeys, accessibility, entitlement, file transcription) and typed events through `on` (`appState`,
  `deliveryRecorded`, `wake`, `hotkeyStatus`, `nativeDragDrop`, `entitlement`).
- `PillClient`: `action(PillAction)`, geometry (`resize`, `setHitbox`), intro and terminal reset, the popups'
  calls, `log`, and typed events through `on` (`event`, `hover`, `cursor`, `intro`, `deliveryRecorded`,
  `modelPhase`, `hintShow`, `clipboardShow`).

Optional replacements, for a host whose pages must look or behave as its own (every default is the package's markup):

```ts
configureDictationHost({
  components: {
    PageShell,   // ({ page: "dictation" | "polish" | "home" | "history", header?, children }) wrapper of those pages; `header` (History only) is the filter row, which the shell frames above `children`
    Toggle,      // ({ checked, onChange, label, disabled? }) the Polish switches
    Kbd,         // ({ chord }) chords on Home
    OriginIcon,  // ({ item: HistoryItem, size?, className? }) app tile of a Home recent row
  },
  classNames: { rowChip: "..." },        // Home row action buttons
  hooks: { useSettings },                // the settings store the pages read and patch (the `UseSettings` shape)
  restoreFocus: (opener) => { /* ... */ }, // where focus goes when the file-transcription modal closes
});
```

`useSettings` must return `{ data, loaded, error, setError, patch(key, value), get(key, fallback) }` (`UseSettings`);
every page reads and patches settings through it.

`DictationHistory` is the History page. It reads the host configuration for what is particular to the app: `appName`
(copy, and the name of "Export selected"'s file: `<appname>-history-selected-<date>.txt`), `components.PageShell`
(the frame, called with `page: "history"`) and `storageKeys` `skipDeleteConfirm`, `groupSelection` (stored shape
`{ mac, iphone, watch }`) and `recoveryDismissed`. `configureDictationHost` forwards `appName`, `storagePrefix` and
those three keys to `@rightkit/library-ui`'s host configuration, so one call configures both. A host's source-visibility
control (a title bar's checkboxes) uses `useHistorySourceSelection()` (`{ sourceSelection, setSourceSelection }`,
the same choice the page follows) and `HISTORY_SOURCES`. Pro state is `DictationClient.entitlement()` (read once) plus the
`entitlement` event on `on` (changes), so a host with its own licence lifecycle returns its snapshot from the first and
subscribes in the second. A rejected `openPricing()` is logged and swallowed by the pages.

## Tauri host

`@rightkit/dictation-ui/tauri` implements both clients over Tauri `invoke` and `listen`, with the command and event
names HeardRight's web code uses. It is the only entry that imports Tauri. One line per window installs it:

```ts
configureDictationHost({ client: tauriDictationClient({ appName: "Your App", fileNamePrefix: "your-app" }) }); // hub
configureDictationHost({ pill: tauriPillClient() });                                                            // pill, popups
```

Options (all optional): `listen` (how native events reach the window, default Tauri's `listen`; pass the app's
own subscribe when it delivers events through its own channel), `entitlement` (`{ current, subscribe }` from the
app's licence lifecycle; without it `entitlement()` and the `entitlement` event throw "not mapped"),
`settingsKeys` (`fromNative`/`toNative` key mapping for the settings store), `settingsEvent`, `appName`,
`fileNamePrefix`. The app's `src-tauri` must serve the commands the entry calls (listed in `src/tauri`).

An app that only needs the settings screens imports the settings backend alone:
`tauriDictationSettingsBackend({ settingsKeys, settingsEvent, invoke })` returns the `SettingsBackend` that
`tauriDictationClient(...).settings` uses (`invoke` defaults to Tauri's).

The app frame (sidebar, title bar, window controls, palette) is `@rightkit/react-shell`; licence `@rightkit/license`;
updates `@rightkit/app-update`; theme `@rightkit/theme`; legal `@rightkit/legal-ui`; settings screens
`@rightkit/settings-ui`. The voice conversation loop is in `@rightkit/agent-ui-components`.

## Settings sections

`@rightkit/dictation-ui/settings` renders HeardRight's settings sections for any host. They use `configureDictationHost`
like the pages, and add optional groups to `DictationClient` (a section whose group is missing throws a message
naming it):

| Group | Methods | Native calls of `tauriDictationClient` |
|---|---|---|
| `app` | `launchAtLoginEnabled()`, `setLaunchAtLogin(enabled)`, `rotateHistoryKey()`, `confirm(message, { title, kind })` | `launch_at_login_enabled`, `set_launch_at_login { enabled }`, `rotate_history_key`, the dialog plugin's `ask` |
| `speech` | `localAsrModelStatus(backend)`, `downloadLocalAsrModel(backend)`, `whisperModelStatus()`, `downloadWhisperModel()`, `prewarmEngine()`, `dismissModelUpdate()` | `local_asr_model_status_for { backend }`, `download_local_asr_model { backend }`, `whisper_model_status`, `download_whisper_model`, `prewarm_engine`, `dismiss_model_update` |
| `developer` | `wakeLabStatus()`, `setWakeLabEnabled(enabled)`, `diagnosticCaptureAudioPresent()`, `openDiagnosticCapturesFolder()`, `recompileCoreMlCache()`, `sendDiagnostics()`, `exportDiagnostics(includeNativeDumps)` | `get_wake_lab_status`, `set_wake_lab_enabled { enabled }`, `diagnostic_capture_audio_present`, `open_diagnostic_captures_folder`, `recompile_coreml_cache`, `send_diagnostics`, `export_diagnostics { includeNativeDumps }` |
| `sync` | `syncProvider()`, `setSyncProvider(provider)`, `cloudSyncStatus()`, `setCloudSyncEnabled(enabled)`, `setKeychainSyncEnabled(enabled)`, `setProResolutionEnabled(enabled)`, `proBridgeStatus()` | `get_settings` (`sync_provider`), `apply_settings_patch { patch: { sync_provider } }`, `cloud_sync_status`, `cloud_sync_set_enabled { enabled }`, `pro_bridge_set_keychain_sync_enabled { enabled }`, `pro_bridge_set_pro_resolution_enabled { enabled }`, `pro_bridge_status` |
| `account` | `sessionStatus()`, `devices()`, `renameDevice(id, label)`, `removeDevice(id)` | none: the `account` option of `tauriDictationClient`, answered by the app's licence layer (rejections are `Error`s carrying the wording to show) |

New events on `DictationClient.on`: `localAsrModel` (`local-asr-model`) and `whisperModel` (`whisper-model`), the model install progress.

- `MicAudioSettings({ loaded, inputDevice, soundEffectsOn, soundEffectsVolume, patch })`, `MicDevicePicker`: input
  device and sound effects (`DictationClient.micDevices`).
- `ApplicationSettings({ settings })`: launch at login (`app`), "Install updates automatically" (`auto_update_install`
  through the `SettingsContext`) and the history encryption key row. Copy uses `hostAppName()`.
- `SyncProviderSettings({ enabled })`: the provider choice, rendered only while the host's flag `enabled` is true.
- `DeveloperSettings({ loaded, developerMode, captureAudio, streamingInsertion, patch })`: developer mode, diagnostics, experiments. The route test needs the host's recognition-setup state: `hooks.useRecognitionSetup({ active, setError })` (returns an object with `phase`) and `components.RecognitionSetupStep({ setup, context: "developer" })`; without both the button is not shown.
- `AppleSyncSettings({ developerMode? })` (macOS only; renders nothing elsewhere): the one "Sync with Apple devices" switch that drives three channels (transcripts, iCloud Keychain, Pro resolution), a one-line summary and, in developer mode, the per-channel breakdown. After a change it dispatches the window event `CLOUD_SYNC_CHANGED_EVENT` so a sidebar row showing the sync status re-reads it.
- `DeviceManagementSettings()`: the devices on the signed-in account (rename, remove with confirmation); renders nothing while signed out. Copy uses `hostAppName()`.
- `SpeechRecognitionSettings({ loaded, language, asrBackend, patch })`: the local model and Whisper cards. Pro state is `DictationClient.entitlement()` and the `entitlement` event; "Download update" and the card's download use the shared model-update store (`useModelUpdate`, `startModelUpdateDownload`, `openModelUpdatePrompt`, `dismissModelUpdate`, `hideModelUpdate`), which a host's own update prompt reads too. `TDT_LANGUAGES` is the language list of the local model card.
- `Toggle`, `SegmentedControl`: the switch and segmented control of these sections. `components.Toggle` (host) replaces
  the switch they render. Tailwind hosts add `node_modules/@rightkit/dictation-ui/dist/settings/**/*.js` to their
  content globs.

## Peer dependencies

`react`

## Host contract

Hosts bundle this package with Vite. Tailwind hosts add `node_modules/@rightkit/dictation-ui/dist/**/*.js` to their
content globs. The pill and pages use `hr-*` CSS classes the host stylesheet defines.
