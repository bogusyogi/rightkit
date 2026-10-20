# @rightkit/tauri

Typed renderer adapters for shared Right Suite device, logs & diagnostics commands. Supply app `invoke` & optionally a command map (default `standardRightKitTauriCommands`). Typed constructors cover ViewRight, ScrapeRight, HeardRight, MailRight & CodeRight.

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
| `@rightkit/tauri` | `createRightKitTauriClient`, `createRightSuiteAppTauriClient`, `rightSuiteTauriAppConfigs`, `standardRightKitTauriCommands`, `standardRightKitDeviceGetOrCreate`, `createViewRightTauriClient`, `createScrapeRightTauriClient`, `createHeardRightTauriClient`, `createMailRightTauriClient`, `createCodeRightTauriClient`; types `Invoke`, `RightKitTauriCommands`, `RightKitTauriClientConfig`, `RightKitTauriAdapterConfig`, `RightKitTauriAppConfig`, `RightKitTauriClient` |
| `@rightkit/tauri/dialog` | `pickFolder`, `pickFile`, `pickFiles` (native file dialogs; see below) |

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
import { createCodeRightTauriClient } from "@rightkit/tauri";

const native = createCodeRightTauriClient({ invoke });
const deviceId = await native.device.getOrCreate();
```

`createRightSuiteAppTauriClient("viewright" | "scraperight" | "heardright" | "mailright" | "coderight", { invoke })` is the same thing with the app name as an argument.

## Native file dialogs (`@rightkit/tauri/dialog`)

```ts
import { pickFolder, pickFile, pickFiles } from "@rightkit/tauri/dialog";

const project = await pickFolder({ title: "Open project" }); // string | null
const csv = await pickFile({ filters: [{ name: "CSV", extensions: ["csv"] }] }); // string | null
const media = await pickFiles({ title: "Add media" }); // string[] | null
```

Thin wrappers over `@tauri-apps/plugin-dialog` (install it in the app; it is an optional peer). Options pass to the plugin unchanged except `directory` and `multiple`, which the function decides. Cancel is `null`. There is no `saveFile`. Titles, extensions and callbacks stay in the app.

There is no QA variant of this entry: the unpublished `dialog-qa` module was removed before 0.3.0 because no consumer is named for the rightkit-qa `dialog` module. An app's QA answers its native pickers in its own native code or test seam, not through this import.

## Migrate to 0.3.0

Remove `client.license.get/set/delete`, `commands.license` & `argumentNames.licenseSet`. Constructor & client type arguments are now `<TDevice, TLogBundle, TLogEvent, TDiagnostics>`; remove former first `TLicense` argument.

Use `@rightkit/license` 0.3.0 native invoke client with `createLicenseClient({ invoke, listen })`. Register its native Rust licence handlers; native code owns storage, refresh & entitlement verification. Renderer receives safe licence views. Remove app handlers that return or accept stored licence tokens; no compatibility fallback remains.

Generic `invoke` injection & custom log/diagnostic command names remain supported. Apps own native command registration & removal.
