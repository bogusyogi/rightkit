# Changelog

## 0.3.0 — unpublished staging

Breaking:
- Remove renderer `license.get/set/delete`, licence command maps & `argumentNames.licenseSet`, including app-specific raw licence routes.
- Remove `TLicense`; generic order is `<TDevice, TLogBundle, TLogEvent, TDiagnostics>`.
- No licence operations remain: this package has no `@rightkit/license` dependency or import, and no licence client or command.

Device atomic `getOrCreate`, logs, diagnostics & update passthrough behavior remain unchanged.

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
