# Dictation and library UI redraw: design

Date: 2026-11-11. Branch `claude/busy-hypatia-e30h7l`. Issue: rightkit#1572.

Follows the shape review of `@rightkit/dictation-ui` (`family-remaining.md`) and `@rightkit/library-ui` (`family-content-ingest-mail.md`), both judged **wrong**. Taken on the recommended option, pending the owner's confirmation. HeardRight is the reference; ScrapeRight is the reference for the reader.

## 1. Units

| Unit | Package (version) | Entry | Holds |
|---|---|---|---|
| Dictation pill | `@rightkit/dictation-ui` 0.2.0 | `./pill` | `PillOverlay`, `HintPopup`, `MiniClipboardPopup`, the pill hooks, the hint catalog. Imports nothing from the pages, so a pill window bundles the pill only. |
| Dictation pages | `@rightkit/dictation-ui` 0.2.0 | `.` | Home, Dictation, Vocabulary, Polish (with API keys), replacements, snippets, Transcribe a file, the History page as `DictationHistory`, hotkey and accessibility banners. |
| Library list and reader | `@rightkit/library-ui` 0.2.0 | `.` | One contract (`LibrarySource`, `LibraryRow`, `LibraryDetail`), the list from HeardRight's History page, the reader from ScrapeRight's Detail reader. No IPC. |
| ScrapeRight queue | `@rightkit/library-ui` 0.2.0 | `./queue` | Paste bar, queue board, job cards, workbench preview, ScrapeRight's Detail screen (now over the library reader). Left here: no queue UI package exists. |
| Voice conversation | `@rightkit/agent-ui-components` 0.1.1 | `.` | `VoiceConversation`, `useVoiceConversation`, `useMicRecorder`, moved from `dictation-ui/src/voice`. Its only user was this package's composer. |

Dependencies now run one way: `dictation-ui` → `library-ui` → (`react-shell`, `platform-ui`, `media-ui`, `doc-render`). `library-ui` no longer depends on `dictation-ui`; `agent-ui-components` no longer depends on `dictation-ui`.

The published name `@rightkit/dictation-ui` is kept for the pill and pages. 0.1.0 (the whole HeardRight front end) should be deprecated on the registry once 0.2.0 is published, pointing to 0.2.0, `library-ui` 0.2.0 and `agent-ui-components` 0.1.1.

## 2. Public APIs

### `DictationClient` (pages)

Supplied by the host through `configureDictationHost({ client })`. No command or event names in the package.

- `settings: SettingsBackend` — the `@rightkit/settings-ui` port; `useSettings()` here is a typed view over settings-ui's `useSettings`.
- `history: LibrarySource` — the delivery history as a library source; rows come from `deliveryRow(item)`, queries map with `deliveryQueryArgs(query)`.
- `on(event, handler)` over `DictationEventMap`: `appState`, `deliveryRecorded`, `wake`, `hotkeyStatus`, `nativeDragDrop`, `entitlement`.
- Operations: `appState`, `run(startDictation|stopDictation|cancelDictation)`, `frontendReady?`, `recentHistory`, `updateHistoryItem`, `copyText`; vocabulary (`listVocabulary`, `listVocabularySuggestions`, `addVocabulary`, `importVocabulary`, `removeVocabulary`, `acceptVocabularySuggestion`, `dismissVocabularySuggestion`); AI keys (`aiKeyStatus`, `validateAiKey`, `setAiKeys`); `micDevices`; hotkeys (`hotkeyConflict`, `registerHotkeys`); macOS accessibility (`accessibilityGranted?`, `activateShortcuts?`, `openAccessibilitySettings?`); entitlement (`entitlement()`, `openPricing`); files (`pickMediaFile`, `transcribeFile`, `saveTextFile`, `openFilesSettings?`).

HeardRight's adapter maps them one to one: `get_app_state`, `start_dictation`/`stop_dictation`/`cancel_dictation`, `hub_frontend_ready`, `get_recent_history`, `update_history_item`, `copy_text_to_clipboard`, `list_vocabulary`, `list_vocab_suggestions`, `add_vocabulary`, `import_vocabulary`, `remove_vocabulary`, `accept_vocab_suggestion`, `dismiss_vocab_suggestion`, `get_l3_key_status`, `validate_l3_key`, `set_l3_api_keys`, `list_mic_devices`, `get_hotkey_conflict`, `register_hotkeys_now`, `macos_accessibility_status`, `activate_macos_shortcuts`, `open_macos_accessibility_settings`, `transcribe_file`, `write_text_file` (after its save dialog), `open_macos_files_settings`; events `app-state-changed`, `delivery-recorded`, `wake-fired`, `hotkey-registration-status`, `native-drag-drop`, `license-changed`.

### Pill events and commands (`PillClient`)

Supplied through `configureDictationHost({ pill })` in each pill window.

- Commands: `action(PillAction)` with `startDictation`, `cancelDictation`, `stopDictation`, `stopAndSend`, `openHubHome`, `openHubHistory`, `openClipboard`; `resize`, `setHitbox`, `takeIntro`, `revealPill`, `terminalErrorGeneration`, `recoverTerminalError(generation)`, `recentHistory`, `showPromptHint`, `copyText`, `updateHistoryItem`; clipboard popup `clipboardItems`, `dismissClipboard`, `resizeClipboard`, `clipboardReady`; hint popup `takeHintPayload`, `dismissHint`, `resizeHint`, `hintReady`, `setScreenshotDestination`; `log(event, message)`.
- Events (`PillEventMap`): `event` (the recording state, `PillEvent`), `hover`, `cursor`, `intro`, `deliveryRecorded`, `modelPhase`, `hintShow`, `clipboardShow`.

HeardRight's mapping: `start_dictation`, `cancel_dictation`, `pill_stop_dictation`, `pill_stop_and_send`, `open_hub_home`, `open_hub_history`, `open_pill_clipboard`, `pill_resize`, `pill_hitbox`, `take_pill_intro`, `reveal_pill`, `terminal_error_generation`, `recover_terminal_error`, `get_recent_history`, `show_prompt_hint`, `get_pill_clipboard_items`, `dismiss_pill_clipboard`, `pill_clipboard_resize`, `pill_clipboard_ready`, `take_pill_hint_payload`, `dismiss_pill_hint`, `pill_hint_resize`, `pill_hint_ready`, `apply_settings_patch`, `frontend_log`; events `pill://event`, `pill://hover`, `pill://cursor`, `pill://intro`, `delivery-recorded`, `local-asr-model`, `pill-hint://show`, `clipboard://show`.

### `LibrarySource` (list and reader)

```ts
interface LibrarySource {
  query(q: LibraryQuery): Promise<{ rows: LibraryRow[]; total: number; hasMore: boolean }>;
  copyText(text: string): Promise<void>;
  subscribe?(onChange): Promise<Unsubscribe>;
  remove?(id); update?(id, text): Promise<boolean>;
  exportAll?(): Promise<number | null>; saveText?(name, contents): Promise<boolean>;
  retention?: { options; get(); set(value): Promise<number> };
  recovery?: { status(); exportDiagnostics() };
  originIcon?(origin): Promise<string | null>;
  detail?(id): Promise<LibraryDetail>;
}
```

`LibraryRow`: `id`, `text`, `altText?`, `timestampMs`, `origin?` (`label`, `detail?`, `iconKey?`, `kind`), `status?`, `badges?`, `durationMs?`, `group?`, `readOnly?`. `LibraryQuery`: `search`, `fromMs`, `untilMs`, `origin`, `variant`, `offset`, `limit`. `LibraryDetail`: `id`, `title`, `text`, `timedText?`, `markdown?`, `timedMarkdown?`, `srt?`, `mediaUrl?`/`mediaKind?`, `images?` (URLs the host resolved), `sourceUrl?`. Optional capabilities hide their controls. Screens: `LibraryScreen` (props `source`, `search`, `variants`, `groups`, `groupSelection`, `labels`) and `LibraryReader` (`source`, `id`).

## 3. Deleted as duplicates, and the owner now

| Deleted (0.1.0 path) | Owner |
|---|---|
| `dictation-ui/lib/license*`, `lib/license/*`, `lib/licenseLifecycle`, `LicenseLifecycleDialog`; `library-ui/queue/lib/license`, `views/settings/useLicense` | `@rightkit/license` (entitlement reaches the pages as `DictationClient.entitlement()`, the queue as a `pro` prop) |
| `dictation-ui/lib/autoUpdates`, `heardRightAutoUpdateClient`, `relaunchForUpdate`, `SidebarUpdateStatus`; `library-ui/queue/lib/updateAuth` | `@rightkit/app-update` |
| `dictation-ui/lib/theme`; `library-ui/queue/lib/theme` | `@rightkit/theme` (and the host's palette) |
| `dictation-ui/lib/legal`, `@rightkit-host` legal aliases | `@rightkit/legal`, `@rightkit/legal-ui` |
| `dictation-ui/components/Sidebar`, `TitleBar`, `WindowControls`, `PageShell`, `CommandPalette`, `usePaletteCommands`, `usePaletteFocusRestore`, `renderCrash`, `lib/fieldStore`; `library-ui/queue/components/DesktopWorkspace`, `TitleBar`, `NavTabs` | `@rightkit/react-shell` (`AppShell`, `Titlebar`, `CaptionControls`, `PageShell`, `CommandPalette`, `ErrorBoundary`) |
| `dictation-ui/lib/settingsRegistry`, `hooks/settingsSync`, the store in `hooks/useSettings`; `library-ui/queue/views/settings/prefs` | `@rightkit/settings-ui` |
| `dictation-ui/components/ui/Toggle`, `settings/SegmentedControl` | `@rightkit/react-shell` `Toggle`, `SegmentedControl` |
| `dictation-ui/components/Kbd` (rendering), `hooks/usePlatform` | `@rightkit/platform-ui` (`Kbd`, `getPlatform`) |
| `dictation-ui/voice/components/Icon` | `@rightkit/agent-ui-core` `Icon` |
| `dictation-ui/lib/tauri`, `tauriBridge`, `hooks/useTauriEvent`, `lib/logs`; `library-ui/queue/lib/ipc/qaMock`, `qaGate` | the host (clients and QA mocks are app code) |
| `library-ui/queue/views/Library`, `LibraryList`, `LibraryToolbar`, `lib/historyPage`, `lib/desktopSources` | `LibraryScreen` over the host's `LibrarySource` |
| `library-ui/queue/lib/queueView.countWords` | the reader's `countWords` |

HeardRight app code with no place in either package was deleted, not moved: `FeatureReel`, `featureReelState`, `HubLoadingHint`, `CommandControls`, `HoverTooltip`, `trayIconDataUri`, `modelUpdate`, `legacyPromptHint`, `lib/config`, `syncProviderFlag`.

Moved, not deleted: `AppIcon` → `library-ui` `OriginIcon`; the generic half of `historyUtils` → `library-ui/list/format`, the dictation-record half → `dictation-ui/history/deliveryRows`; `useHistorySourceSelection` → `library-ui` `useGroupSelection`; HeardRight's History page → `library-ui` `LibraryScreen` (+ `dictation-ui` `DictationHistory`); `Transcribe`, `TranscribeModal` → `dictation-ui`; `src/voice` → `agent-ui-components`.

## 4. Host configuration

- `configureDictationHost({ appName, client, pill, storagePrefix, storageKeys, formattingGuideUrl })`. Storage keys: `promptHintCount`, `promptHintSeen`, `promptHintDisabled` (default `rk-dictation.<key>`). HeardRight keeps its stored latches by passing `hr_prompt_hint_ai_recordings_v1`, `hr_prompt_hint_shown_v1`, `hr_prompt_hint_disabled`.
- `configureLibraryHost({ appName, storagePrefix, storageKeys })`. Keys: `skipDeleteConfirm`, `groupSelection`, `recoveryDismissed` (default `rk-library.<key>`). HeardRight passes `heardright.skipDeleteHistoryConfirm`, `heardright.historySourceSelection.v1`, `hr.historyRecoveryDismissed.v1`.
- Event names are the host's: the clients expose typed events, the host maps them to its native events (including the old `heardright://renderer-event` bridge, which is now the host's).
- Theme codes are the host's; neither package reads or writes a theme.
- The hint catalog names the product as `{app}`; the Dictation page links the formatting guide only when the host passes its URL.

## 5. Deferred

1. HeardRight adopts: write its `DictationClient`, `PillClient` and `LibrarySource` adapters (mapping in section 2), pass its storage keys, then delete its 83 twins of the 0.1.0 files.
2. ScrapeRight adopts: a `LibrarySource` over `get_history_page`/`get_transcript` replacing the deleted library list; pass `pro` and paste-bar defaults; its own theme, licence, title bar, settings and QA mock stay in the app.
3. Queue UI: `./queue` still calls ScrapeRight's command names (`submit_playlist`, `sign_in`, `get_transcript`, ...). Move it to a queue UI package (with a `QueueSource` contract) when one is made.
4. CSS: the pill and pages use `hr-*` classes, the reader and queue `claude-*` classes; renaming needs a stylesheet migration in each host. `dictation-ui/lib/ui` is HeardRight's class table; it could move to the host's theme once the pages take class names from the shell.
5. The list's settings-search ids stay `history.*` (`data-setting-id`) so HeardRight's settings search keeps finding them.
6. Deprecate `@rightkit/dictation-ui` 0.1.0 on npm after 0.2.0 is published.
7. `@rightkit/agent-ui-core` does not build on the branch (`state/legal.ts`, a legal receipt type), unrelated to this redraw.
