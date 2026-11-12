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

The app frame (sidebar, title bar, window controls, palette) is `@rightkit/react-shell`; licence `@rightkit/license`;
updates `@rightkit/app-update`; theme `@rightkit/theme`; legal `@rightkit/legal-ui`; settings screens
`@rightkit/settings-ui`. The voice conversation loop is in `@rightkit/agent-ui-components`.

## Peer dependencies

`react`

## Host contract

Hosts bundle this package with Vite. Tailwind hosts add `node_modules/@rightkit/dictation-ui/dist/**/*.js` to their
content globs. The pill and pages use `hr-*` CSS classes the host stylesheet defines.
