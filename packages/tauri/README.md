# @rightkit/tauri

Typed renderer adapters for shared RightKit device, logs & diagnostics commands, plus the app command registry (`defineTauriCommands`), the native fetch bridge (`createNativeFetch`) and native file dialogs. One constructor, `createRightKitTauriClient`, takes the app's `invoke` and the command names its host registers as configuration. The package names no app.

| Item | Value |
|---|---|
| Where it runs | App webview UI (renderer side of Tauri IPC) |
| Rust pair | `rightkit-device` (native device identity) & app log/diagnostics handlers |
| Version | see `package.json` (npm) |
| License | MIT OR Apache-2.0 |

Updater behavior comes from `@rightkit/app-update`: an `AutoUpdateClient` passed as `updates` is exposed unchanged as `client.updates`, and this package implements none.

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/tauri` | `createRightKitTauriClient`, `standardRightKitTauriCommands`, `logsOnlyRightKitTauriCommands`, `standardRightKitDeviceGetOrCreate`, `defineTauriCommands`, `declaredTauriCommandNames`, `tauriHandlerCommandNames`, `missingTauriHandlers`, `TauriCommandError`, `createNativeFetch`, `NativeFetchError`, `NATIVE_FETCH_MESSAGES`, `RIGHTKIT_FETCH_COMMAND`; types `Invoke`, `RightKitTauriCommands`, `RightKitTauriClientConfig`, `RightKitTauriAdapterConfig`, `RightKitTauriAppConfig`, `RightKitTauriClient`. Deprecated, removed in 0.5.0: `createViewRightTauriClient`, `createScrapeRightTauriClient`, `createHeardRightTauriClient`, `createMailRightTauriClient`, `createCodeRightTauriClient`, `createRightSuiteAppTauriClient`, `rightSuiteTauriAppConfigs` |
| `@rightkit/tauri/window` | `tauriWindowControls` (caption-button adapter; the app passes how to reach its window, e.g. `getCurrentWindow` of its own `@tauri-apps/api`) |
| `@rightkit/tauri/dialog` | `pickFolder`, `pickFile`, `pickFiles`, `pickSavePath`, `ask` (native dialogs; see below) |

Clients expose `device.getOrCreate`, `logs.write/collect/clear`, `diagnostics.collect/clear` & optional `updates`.

`createRightKitTauriClient` also takes `deviceGetOrCreate` & `argumentNames.logsWrite` (default `events`). Without `deviceGetOrCreate`, `device.getOrCreate()` throws. Blank command names throw during client creation.

## Dependencies

| Dependency | Range | Note |
|---|---|---|
| `@rightkit/logs` | `^0.1.4` | Runtime import of the `rightkit_logs_*` command-name constants |

Peer dependency: `@rightkit/app-update` `>=0.2.5 <0.5`, used only as a type (`AutoUpdateClient`). Install it alongside this package.

## Example

```ts
import { invoke } from "@tauri-apps/api/core";
import {
  createRightKitTauriClient,
  standardRightKitDeviceGetOrCreate,
  type RightKitTauriAppConfig,
} from "@rightkit/tauri";

// The app's own values: the command names its native host registers.
const appConfig = {
  deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
} satisfies RightKitTauriAppConfig;

const native = createRightKitTauriClient({ invoke, ...appConfig });
const deviceId = await native.device.getOrCreate();
```

A host that answers diagnostics from the log store passes `commands: logsOnlyRightKitTauriCommands`.

## Migrate to 0.4.0

The per-app constructors still work in 0.4.0 and are removed in 0.5.0. Replace them with the one constructor and the app's own values:

| 0.3.x | 0.4.0 |
|---|---|
| `createViewRightTauriClient`, `createScrapeRightTauriClient`, `createMailRightTauriClient` `({ invoke })` | `createRightKitTauriClient({ invoke, deviceGetOrCreate: standardRightKitDeviceGetOrCreate })` |
| `createHeardRightTauriClient`, `createCodeRightTauriClient` `({ invoke })` | `createRightKitTauriClient({ invoke, commands: logsOnlyRightKitTauriCommands, deviceGetOrCreate: standardRightKitDeviceGetOrCreate })` |
| `createRightSuiteAppTauriClient(app, config)`, `rightSuiteTauriAppConfigs[app]` | the app's own `RightKitTauriAppConfig` value spread into `createRightKitTauriClient` |

`createRightKitTauriClient` has no default `deviceGetOrCreate`; pass it, or `device.getOrCreate()` rejects. `updates` and type arguments are unchanged.

Why here and not in `@rightkit/tauri-shell`: that package is the bridge for the `rightkit-tauri-shell` crate's own `rk_shell_*` commands and has no dependencies. The commands here belong to other owners (`rightkit-device`, `@rightkit/logs`, app diagnostics, the `rightkit-app-update` fetch bridge), so the two packages do not duplicate an API.

## Native file dialogs (`@rightkit/tauri/dialog`)

```ts
import { pickFolder, pickFile, pickFiles, pickSavePath, ask } from "@rightkit/tauri/dialog";

const project = await pickFolder({ title: "Open project" }); // string | null
const csv = await pickFile({ filters: [{ name: "CSV", extensions: ["csv"] }] }); // string | null
const media = await pickFiles({ title: "Add media" }); // string[] | null
const target = await pickSavePath({ defaultPath: "history.json" }); // string | null; the app writes the file
const sure = await ask("Delete this export?", { kind: "warning" }); // boolean; true only on confirm
```

Thin wrappers over `@tauri-apps/plugin-dialog` (install it in the app; it is an optional peer). Options pass to the plugin unchanged except `directory` and `multiple`, which the function decides. Cancel is `null` for the pickers and `pickSavePath`; `ask` resolves `false` on cancel. `pickSavePath` only chooses a path: the app writes the file. Titles, extensions and button labels stay in the app.

There is no QA variant of this entry: the unpublished `dialog-qa` module was removed before 0.3.0 because no consumer is named for the rightkit-qa `dialog` module. An app's QA answers its native pickers in its own native code or test seam, not through this import.

## Migrate to 0.3.0

Remove `client.license.get/set/delete`, `commands.license` & `argumentNames.licenseSet`. Constructor & client type arguments are now `<TDevice, TLogBundle, TLogEvent, TDiagnostics>`; remove former first `TLicense` argument.

Use `@rightkit/license` 0.3.0 native invoke client with `createLicenseClient({ invoke, listen })`. Register its native Rust licence handlers; native code owns storage, refresh & entitlement verification. Renderer receives safe licence views. Remove app handlers that return or accept stored licence tokens; no compatibility fallback remains.

Generic `invoke` injection & custom log/diagnostic command names remain supported. Apps own native command registration & removal.
