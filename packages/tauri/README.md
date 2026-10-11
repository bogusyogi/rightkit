# @rightkit/tauri

Typed renderer-side adapters for the shared Right Suite Tauri command surface. The client takes the app's real `invoke` function and, optionally, a command map (default `standardRightKitTauriCommands`). It also exports typed constructors for ViewRight, ScrapeRight, HeardRight, MailRight and CodeRight, so their command and argument names are defined in one place.

| Item | Value |
|---|---|
| Where it runs | App webview UI (renderer side of Tauri IPC) |
| Rust pair | `rightkit-tauri` (native Tauri license, device, authorization and diagnostics services) |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

Updater behavior comes from `@rightkit/updates`: an `AutoUpdateClient` passed as `updates` is exposed unchanged as `client.updates`, and this package implements none.

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/tauri` | `createRightKitTauriClient`, `createRightSuiteAppTauriClient`, `rightSuiteTauriAppConfigs`, `standardRightKitTauriCommands`, `standardRightKitDeviceGetOrCreate`, `createViewRightTauriClient`, `createScrapeRightTauriClient`, `createHeardRightTauriClient`, `createMailRightTauriClient`, `createCodeRightTauriClient`; types `Invoke`, `RightKitTauriCommands`, `RightKitTauriClientConfig`, `RightKitTauriAdapterConfig`, `RightKitTauriAppConfig`, `RightKitTauriClient` |

The client exposes `license.get/set/delete`, `device.getOrCreate`, `logs.write/collect/clear`, `diagnostics.collect/clear` and an optional `updates` client.

`createRightKitTauriClient` also takes `deviceGetOrCreate` and `argumentNames` (`licenseSet`, default `value`; `logsWrite`, default `events`). Without `deviceGetOrCreate`, `device.getOrCreate()` throws. A blank command name throws when the client is created.

## Dependencies

| Dependency | Range | Note |
|---|---|---|
| `@rightkit/logs` | `^0.1.4` | Runtime import of the `rightkit_logs_*` command-name constants |

Peer dependencies: `@rightkit/updates` `^0.2.5`, used only as a type (`AutoUpdateClient`). Install it alongside this package in the app.

## Example

```ts
import { invoke } from "@tauri-apps/api/core";
import { createCodeRightTauriClient } from "@rightkit/tauri";

const native = createCodeRightTauriClient({ invoke });
const license = await native.license.get();
const deviceId = await native.device.getOrCreate();
```

`createRightSuiteAppTauriClient("viewright" | "scraperight" | "heardright" | "mailright" | "coderight", { invoke })` is the same thing with the app name as an argument.

## Known issues

- None open.
