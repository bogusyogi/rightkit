// @rightkit/dictation-ui/tauri: the Tauri `DictationClient` and `PillClient`, the one place this package
// imports Tauri. Each window installs its client once, before rendering:
//
//   import { configureDictationHost } from "@rightkit/dictation-ui";
//   import { tauriDictationClient, tauriPillClient } from "@rightkit/dictation-ui/tauri";
//   configureDictationHost({ client: tauriDictationClient() });   // hub window
//   configureDictationHost({ pill: tauriPillClient() });          // pill and popup windows
//
// `tauriDictationSettingsBackend(options?)` is the settings backend of the hub client on its own, for an app that
// only needs the settings screens.
//
// Both clients make exactly the native calls the HeardRight web code makes: the commands and arguments below
// are its `invoke` calls, the events are its `listen` calls. Pass `listen` when the app delivers events
// through its own channel instead of Tauri's event system.
export { tauriDictationClient, type TauriDictationOptions } from "./dictationClient.js";
export { tauriDictationSettingsBackend, type TauriSettingsOptions } from "./settingsBackend.js";
export { tauriPillClient } from "./pillClient.js";
export type { ListenFn, TauriClientOptions } from "./events.js";
