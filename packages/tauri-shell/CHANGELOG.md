# Changelog

## 0.1.2 — unpublished staging

- Repairs shared Tauri command adapter behavior.
- Additive (PK08-2): wrappers for the commands the host minor carrying PK04-2 and PK08-2 registers through `handler!`. The Rust crate in the tree (`crates/rightkit-tauri-shell`, 0.4.0) does not register these yet, so the wrappers reject until that minor lands. `menu.setChecked(id, checked)` (`rk_shell_set_menu_item_checked`), `menu.setState(updates)` (`rk_shell_set_menu_state`), `print(ui?)` (`rk_shell_print`) with `onPrintError`, `openPaths.ready()` / `openPaths.onOpen` (`rk_shell_open_paths_ready`, `rightkit-shell://open-paths`), `associations.status(groups?)`, `associations.set(groups?, timeoutMs?)` and `associations.openDefaultApps()` (`rk_shell_associations_*`, `rk_shell_open_default_apps`), `autostart.openLoginItemsSettings()` (`rk_shell_open_login_items_settings`).
- Additive: `MenuEntry` gains `check` (`checked`) and `submenu` (`entries`); `WindowSpec` carries every host field (`minWidth`, `minHeight`, `decorations`, `center`, `fullscreen`, `skipTaskbar`, `x`, `y`, `parent`, `resizable`, `shadow`, `visible`); new types `MenuItemState`, `PrintUi`, `AssociationStatus`, `AssociationSetResult`, `AssociationGroupStatus`, `HandlerStatus`; `EVENTS.openPaths`, `EVENTS.printError`.
- Needs a host built from the `rightkit-tauri-shell` minor that carries these commands. That minor (0.5.0 candidate, with its modules) is not in the tree at this commit: `crates/rightkit-tauri-shell/Cargo.toml` is 0.4.0. Against an older host the new calls reject with `Command ... not found`. Publishes after the tauri-shell minor that carries the host registration (PK04-2).
- Additive (PK08-3, PK10-2, PK13-2): `surface.set(surface)` (`rk_shell_set_surface`, answers `SurfaceReport`); `preview.register(kind, html)`, `preview.revoke(token)`, `preview.revokeAll()` (`rightkit_preview_*`, host-services); `legal.read()`, `legal.create(request)`, `legal.reset()` (`rightkit_legal_receipt_*`, host-services; `create` sends the declaration only, no time, and drops any extra field); `update.check/status/download/install/cancel/onProgress` (`rightkit_update_*`, `rightkit://update-progress`, feature `update`).
- Additive: exported command constants `SURFACE_COMMAND`, `PREVIEW_COMMANDS`, `LEGAL_RECEIPT_COMMANDS` (identical to `@rightkit/onboarding`), `UPDATE_COMMANDS`, `UPDATE_PROGRESS_EVENT`; types `SurfaceReport`, `HostError`, `PreviewHandle`, `LegalReceiptRecord`, `LegalReceiptCreateRequest`, `LegalDeclaredDocument`, `LegalEligibilityBasis`, and the closed update DTOs `UpdateHostCheck`, `UpdateHostStatus`, `UpdateHostProgress`, `UpdateHostDownload`, `UpdateHostInstall`, `UpdateHostError` with their enums. No URL, header or token appears in any update type.
- `shell.update` is an unvalidated transport; `@rightkit/app-update/native` validates replies and owns the message text. Needs a host built from the tauri-shell minor that registers these commands; against an older host the calls reject with `Command ... not found`.
- No secret getter, settings or licence wrapper is added here: settings-ui's `/shell-adapter` owns the settings and secrets calls (write-only secrets, no read).

## Unreleased

- Renamed from @rightkit/shell; @rightkit/shell@0.1.1 remains published under the old name.

## 0.1.1

Additive. Existing API and wire behaviour are unchanged unless a new option is used.

- `shell.quit(options?)` now resolves a `QuitReport`. Default runs the quit drain handlers, then calls `rk_shell_finish_quit`. `{ force: true }` calls `rk_shell_quit` at once, as before. `{ timeoutMs }` overrides the drain budget.
- `shell.onQuitRequested(handler)` registers an async-capable before-quit drain handler and returns an unsubscribe. It rejects with `unsupported` until the host has the `quitDrain` capability.
- `createShell(transport, { quitTimeoutMs, onQuitReport })`: default drain budget (10 000 ms) and a reporter for timeouts and handler errors (default `console.error`).
- `DEFAULT_QUIT_TIMEOUT_MS`, `EVENTS.quitRequested` (`rightkit-shell://quit-requested`).
- `shell.capabilities()` probes `rk_shell_capabilities` once. A host without that command reports no capabilities.
- `tray.onCustom(name, handler)` listens to an app-defined event name. Names under `rightkit-shell://` are rejected as `invalid`.
- `notifications.post({ bypassDoNotDisturb: true })` posts while Do-Not-Disturb is on. Needs the `notifyBypassDnd` capability, otherwise it rejects with `unsupported` and posts nothing. `false` or absent sends the request exactly as 0.1.0 did.
- Tests: `pnpm test` (Node test runner, `tests/shell.test.mjs`).

Host support required for the new capability-gated items (not in `rightkit-shell` yet): `rk_shell_capabilities`, `rk_shell_finish_quit`, the `rightkit-shell://quit-requested` event, and `bypassDoNotDisturb` in `rk_shell_notify`. See README.

## 0.1.0

- Initial typed renderer bridge for the `rightkit-shell` crate.
