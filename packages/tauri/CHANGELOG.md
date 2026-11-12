# Changelog

## 0.4.1

Additive. The existing pickers are unchanged.

- `@rightkit/tauri/dialog`: `pickSavePath(options?)` resolves the chosen path or `null` (wraps the plugin's `save`), and `ask(message, options?)` resolves `true` only on confirm (wraps `ask`). Options pass through, as the pickers' do. Types `SaveOptions` and `AskOptions` are exported from the entry. The plugin's `save` and `ask` are declared in the ambient shape `src/plugin-dialog.d.ts`. Type checks in `type-tests/contract.ts`.

## 0.4.0 — unpublished staging

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-foundation-shell-ui.md`, section 6) judged this package the wrong shape: it exported one constructor per app and a table of apps, so a shared library named its consumers and had to be republished for each new app. 0.4.0 keeps one product-neutral constructor and moves the per-app values into the apps. 0.3.1 was never published; its entries below ship in this version.

Decision: redraw here, not fold into `@rightkit/tauri-shell`. `@rightkit/tauri-shell` is the bridge for one crate's `rk_shell_*` commands and events and has no dependencies. This package covers commands owned elsewhere (`rightkit-device` identity, `@rightkit/logs` command names, app-registered diagnostics, the `rightkit-app-update` fetch bridge), the app's own command registry (`defineTauriCommands`) and the plugin-dialog pickers. Folding would give tauri-shell a `@rightkit/logs` dependency and two peers without removing a duplicate API; the only shared piece is the `Invoke` type.

Added:
- `logsOnlyRightKitTauriCommands`: the command map for a host that answers diagnostics from the log store (`diagnostics.collect/clear` call `rightkit_logs_collect/clear`). It is the one non-standard map any app used (formerly the HeardRight and CodeRight entries).

The one constructor is `createRightKitTauriClient({ invoke, commands?, deviceGetOrCreate?, argumentNames?, updates? })`, unchanged. Each app keeps its own `RightKitTauriAppConfig` value.

Deprecated (still exported and behaving as in 0.3.x; removed in 0.5.0):
- `createViewRightTauriClient`, `createScrapeRightTauriClient`, `createMailRightTauriClient`: use `createRightKitTauriClient({ invoke, deviceGetOrCreate: standardRightKitDeviceGetOrCreate })`.
- `createHeardRightTauriClient`, `createCodeRightTauriClient`: use `createRightKitTauriClient({ invoke, commands: logsOnlyRightKitTauriCommands, deviceGetOrCreate: standardRightKitDeviceGetOrCreate })`.
- `createRightSuiteAppTauriClient(app, config)` and `rightSuiteTauriAppConfigs`: keep the app's values in the app and pass them to `createRightKitTauriClient`.

Note when migrating: `createRightKitTauriClient` has no default `deviceGetOrCreate` (without it `device.getOrCreate()` rejects), while every per-app constructor set `standardRightKitDeviceGetOrCreate`. Pass it explicitly.

Breaking: none. The per-app constructors are thin wrappers over `createRightKitTauriClient` with their 0.3.x values, in `src/deprecated-app-clients.ts`; no other source file names an app.

## 0.3.1 — unpublished staging

- Removed (Adrian, 2026-11-03: end-to-end tests only): the three unit test files under `src/`, the `test` script and the `vitest` devDependency. `typecheck:contracts` stays.

- Issue #448: `createNativeFetch({ invoke, command?, timeoutMs? })` returns a `fetch`-compatible function over the native fetch bridge (`FetchBridge` in `rightkit-app-update`), so an app needs neither `tauri-plugin-http` nor a webview `connect-src` for its own servers. It builds a `Request` from the usual `fetch` arguments, sends `{ request: { url, method, headers, bodyBase64?, timeoutMs? } }` to the command (`RIGHTKIT_FETCH_COMMAND` = `rightkit_app_update_fetch`, registered by the app's host behind its caller policy) and answers a real `Response` (final URL after native redirects; no body for 204, 205 and 304). Failures reject with `NativeFetchError` (a `TypeError`, closed `code`, fixed text `NATIVE_FETCH_MESSAGES`); an aborted `signal` is honoured before and after the native call. The native host owns the allowlist. Additive, Breaking: none. The boot-stage half of #448 is `@rightkit/app-update/native` `reportStage` and `@rightkit/app-update/react` `BootProbationReporter`. Needs the rightkit-tauri-shell command registration. Not compiled. The default command is `rightkit_app_update_fetch`, the name `rightkit-tauri-shell` registers (feature `app-update`); request and response DTOs are the camelCase shapes of `FetchRequest` and `FetchResponse`.

## 0.3.0 — unpublished staging

Breaking:
- Remove renderer `license.get/set/delete`, licence command maps & `argumentNames.licenseSet`, including app-specific raw licence routes.
- Remove `TLicense`; generic order is `<TDevice, TLogBundle, TLogEvent, TDiagnostics>`.
- No licence operations remain: this package has no `@rightkit/license` dependency or import, and no licence client or command.

Device atomic `getOrCreate`, logs, diagnostics & update passthrough behavior remain unchanged.

Added (#1106, typed app command registry):
- `defineTauriCommands<Spec>({ invoke, commands, mapError? })`: an app declares its own `invoke` commands once as an interface of async methods plus a runtime map (`name: true`, or `name: 'rust_name'`); the map is exhaustive against the spec at compile time, names are validated, and every command goes through one error mapper (default `TauriCommandError` with `command` and `cause`). `declaredTauriCommandNames`, `tauriHandlerCommandNames(rustSource)` and `missingTauriHandlers(declared, rustSources)` check that each declared name is registered in a Rust `generate_handler![]` list (for the app's end-to-end suite or build script; comments are ignored). Additive; `createRightKitTauriClient` is unchanged. Not built here.

Added (CutRight gap 8, native file dialogs):
- `@rightkit/tauri/dialog`: `pickFolder`, `pickFile`, `pickFiles` over `@tauri-apps/plugin-dialog` (new optional peer, `^2.0.0`). `directory` and `multiple` are set by the function; every other plugin option passes through unchanged; cancel resolves `null`; a plugin result of the wrong shape throws. No `saveFile`: its behaviour is unproven. The main entry stays free of the plugin.
- Tests: `src/dialog.test.ts`, `src/dialog-export.test.ts` (real Vite resolution of the `./dialog` export map; needs `dist/`), contract types in `type-tests/contract.ts`.

Removed before publication (PK05-7):
- The staged `dialog-qa` entry (the `rightkit-qa` + `development` export condition on `./dialog`, `src/dialog-qa.ts`, its tests and contract types) is removed: its only candidate consumer is the unpublished rightkit-qa `dialog` module, and no app packet names a consumer for it. `./dialog` resolves `dist/dialog.js` under every condition. If PK14-1 later keeps the rightkit-qa `dialog` module with a named consumer, the QA entry returns in a later minor together with it.

## 0.2.3 (2026-10-19)

- Accepts @rightkit/app-update 0.4.x as a peer; peer identity stays @rightkit/app-update.

## 0.2.2 (2026-10-17)

- Accept @rightkit/app-update 0.3.x as a peer (range >=0.2.5 <0.4); no behaviour change.

## 0.2.1 — 2026-10-15

- Peer dependency renamed: `@rightkit/updates` `^0.2.5` to `@rightkit/app-update` `^0.2.5` (`946f71ef`). 0.2.0 on npm still declared the old name because the rename did not bump the version. No API change.

## 0.2.0 — 2026-10-09

Breaking:
- `@rightkit/updates` moved from `dependencies` (pinned `0.2.4`) to `peerDependencies` (`^0.2.5`). Apps must install `@rightkit/updates` themselves; it is used only as the `AutoUpdateClient` type (`ca41ba38`).

Changed:
- `@rightkit/logs` `^0.1.4` is now a dependency. The `rightkit_logs_write`, `rightkit_logs_collect` and `rightkit_logs_clear` command names come from its `RIGHTKIT_LOGS_WRITE_COMMAND`, `RIGHTKIT_LOGS_COLLECT_COMMAND` and `RIGHTKIT_LOGS_CLEAR_COMMAND` constants instead of string literals. The command strings are unchanged (`ca41ba38`).
- Public types are unchanged (`ca41ba38`).

## 0.1.1 — 2026-08-28

- Dependency bump only: `@rightkit/updates` `workspace:0.2.3` to `workspace:0.2.4` (`343654bf`).

## 0.1.0 — 2026-07-14

- First version under the name `@rightkit/tauri`: typed host adapters for shared Right Suite Tauri commands (`1cb6b4c4`). Commits through `64de23bf` landed on this version without a version bump.
