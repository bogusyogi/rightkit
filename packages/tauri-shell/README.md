# @rightkit/tauri-shell

Typed renderer bridge for the `rightkit-tauri-shell` Tauri crate. It exposes window controls, menu and tray events, deep links, autostart, rich clipboard, notifications, presence and idle state, quit drain, and engine calls. The package has no runtime dependencies.

| Item | Value |
|---|---|
| Where it runs | App webview UI (renderer side of the Tauri IPC) |
| Rust pair | `rightkit-tauri-shell` (webview hardening, window state, native menu, tray, single instance, deep links, autostart, clipboard, engine wiring) |
| Version | 0.1.2, unpublished (0.1.1 is the published version; see CHANGELOG.md) |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/tauri-shell` | `createShell(transport?, options?)`, `tauriTransport()`, `EVENTS`, `asShellError`, `DEFAULT_QUIT_TIMEOUT_MS`; types `ShellBridge`, `ShellTransport`, `ShellStatus`, `ShellCapabilities`, `QuitHandler`, `QuitOptions`, `QuitReport`, `ShellOptions`, `DeepLink`, `WindowSpec`, `MenuEntry`, `NotificationRequest`, `NotifyOutcome`, `PresenceState`, `IdleStatus`, `EngineEvent`, and others |

`createShell()` returns an object with `ready()`, `status()`, `capabilities()`, `quit(options?)`, `onQuitRequested(handler)`, `openExternal(url)`, `window.do(action)`, `menu` (`setEnabled`, `setChecked`, `setState`, `onClick`), `tray`, `deepLinks`, `onSecondInstance`, `onStall`, `autostart`, `clipboard`, `notifications`, `presence`, `idle`, `paths`, `openPaths`, `associations`, `print(ui?)`, `onPrintError`, `recovery`, `surface`, `preview`, `legal`, `update`, `engine`, and `bridge()`. `bridge()` returns a `ShellBridge` with `window.minimize()`, `window.toggleMaximize()`, `window.close()`, `openExternal(url)`, `onMenuAction(handler)` and `setNativeTheme(theme)`; `onMenuAction` and `setNativeTheme` are not on the `createShell()` object itself.

## Example

```ts
import { createShell } from "@rightkit/tauri-shell";

const shell = createShell(); // throws outside a Tauri webview
await shell.ready();         // call once after subscribing; queued deep links are delivered
await shell.openExternal("https://example.com");
```

`openExternal` accepts only `http`, `https` and `mailto`. The host refuses other schemes.

## Host requirement: register the shell commands (Rust apps)

The `rightkit-tauri-shell` crate does not register its own `rk_shell_*` commands. Tauri takes one invoke handler per builder, so the app must register them in its builder with `rightkit_tauri_shell::handler!`, which includes the shell commands plus the app's own:

```rust
tauri::Builder::default()
    .plugin(rightkit_tauri_shell::Shell::builder("myapp").app_name("MyApp").build().plugin())
    .invoke_handler(rightkit_tauri_shell::handler![my_app_command])
    .run(tauri::generate_context!())
```

If an app does not do this, every shell command call from this package fails with `command not found` (for example `rk_shell_ready` or `rk_shell_quit`), and the app's own commands still work. `capabilities()` is the exception: it reports both capabilities as `false`.

## Quit drain

Use this to flush work (for example queued sends) before the app exits.

```ts
const off = await shell.onQuitRequested(async () => {
  await flushOutbox(); // may reject; the error is reported and quit still proceeds
});

// Renderer-initiated quit: drains, then calls rk_shell_finish_quit.
const report = await shell.quit();           // { outcome, pending, errors }
await shell.quit({ force: true });           // immediate rk_shell_quit, no handlers run
await shell.quit({ timeoutMs: 3000 });       // per-call budget

off();                                       // unsubscribe
```

Rules:

- Handlers run concurrently. The quit proceeds only after every handler settles or the budget elapses. The default budget is `DEFAULT_QUIT_TIMEOUT_MS` (10 000 ms); `createShell(transport, { quitTimeoutMs })` changes the default, and `quit({ timeoutMs })` overrides it per call. A negative, `NaN` or infinite value is refused with `invalid`: `createShell` throws it and `quit` rejects with it.
- A rejecting handler does not stop the quit. Its error is normalised to `ShellError` and listed in `QuitReport.errors`. Reports with a timeout or an error go to `createShell(transport, { onQuitReport })` (default `console.error`).
- A host quit request (tray Quit, Dock, Cmd+Q) runs the same drain and finish, once even if several requests arrive.
- `QuitReport.outcome`: `drained`, `timed-out` (with `pending` handlers still running), `forced`, or `immediate` (host lacks the drain).
- Without the host capability, `onQuitRequested` rejects with `unsupported`, and `quit()` calls `rk_shell_quit` at once as before.

## Custom tray events

```ts
// Menu items: any id in the tray spec arrives through onEvent as { kind: 'menu', id }.
await shell.tray.onEvent((e) => { if (e.kind === 'menu' && e.id === 'compose') openCompose(); });

// App-defined events, such as one the app's own Rust code emits with app.emit("tray-compose", ()).
const off = await shell.tray.onCustom("tray-compose", () => openCompose());
```

The shell's tray emits only menu and click events. `onCustom` needs no host support. Names under `rightkit-shell://` are reserved and rejected with `invalid`.

## Notifications and Do-Not-Disturb

`notifications.post` is suppressed (`{ delivered: false, suppressed: "dnd" }`) while the app's manual or automatic Do-Not-Disturb is on. To post regardless:

```ts
await shell.notifications.post({ title: "Send failed", body: "Retry from the outbox", bypassDoNotDisturb: true });
```

- Default `false`: the request is sent exactly as in 0.1.0.
- Requires the `notifyBypassDnd` capability. Without it the call rejects with `unsupported` and nothing is posted. Check with `(await shell.capabilities()).notifyBypassDnd`.
- Platforms: rightkit-tauri-shell posts notifications natively on macOS and Windows; other platforms report `unsupported`. The bypass applies on those native platforms only.
- The bypass covers the shell's Do-Not-Disturb. It does not override the OS Focus mode, which can still hold banners.

## Capabilities and host support

`shell.capabilities()` returns `{ quitDrain, notifyBypassDnd }`. A host without `rk_shell_capabilities` reports both as `false`. Features gated by a capability fail with a typed `unsupported` error that names what is missing.

| Feature | Renderer side (this package) | Host support needed in `rightkit-tauri-shell` |
|---|---|---|
| Quit drain | `onQuitRequested`, `quit`, `quitTimeoutMs` | `rk_shell_capabilities` returns `quitDrain: true` (only when the app opts in with `ShellBuilder::quit_drain(true)` or `quit_drain_bound(d)`); `rk_shell_finish_quit` command (sets exiting, then `app.exit(0)`); `rightkit-shell://quit-requested` event emitted on a host quit request instead of exiting, with `prevent_exit`; the host exits after its own bound (default 15 s, at most 120 s) if the renderer never calls finish |
| Notification bypass | `notifications.post({ bypassDoNotDisturb })` | `rk_shell_capabilities` returns `notifyBypassDnd: true`; `rk_shell_notify` request field `bypassDoNotDisturb` (serde default `false`) skips the `effective_dnd()` check |
| Custom tray events | `tray.onCustom` | none |

`rk_shell_capabilities` is read-only and returns `{ quitDrain: bool, notifyBypassDnd: bool }`. The `rightkit-tauri-shell` crate implements both; it always reports `notifyBypassDnd: true` and reports `quitDrain: true` only when the app opts in. On a host without the support, the capability-gated calls reject with `unsupported`.

## Known issues

- `createShell` throws when `__TAURI_INTERNALS__` is absent. Pass a custom `ShellTransport` for tests; `tests/shell.test.mjs` shows one.
- The quit drain is off unless the host opts in with `ShellBuilder::quit_drain(true)`. Until then `onQuitRequested` rejects with `unsupported` and `quit()` quits at once.

## Menu state, print, open paths and file associations

These wrap host commands that `rightkit_tauri_shell::handler!` registers; they need a host built from the crate minor that carries them.

```ts
// Check items: `{ kind: 'check', id, label, checked }` in a menu spec; submenus: `{ kind: 'submenu', label, entries }`.
await shell.menu.setChecked("view.grid", true);
await shell.menu.setState([{ id: "file.save", enabled: false }, { id: "view.grid", checked: false }]);

await shell.print();            // the calling window; shell.print("system") picks the Windows system dialog
await shell.onPrintError((message) => report(message));

// Files the app was asked to open. Subscribe first, then call ready(): queued paths come back once.
await shell.openPaths.onOpen(({ paths }) => open(paths));
const first = await shell.openPaths.ready();

// Default-handler registration. The file groups are native state; the renderer sends only group ids.
const status = await shell.associations.status();
const result = await shell.associations.set(["documents"]); // result.userActionRequired: Windows opened Default apps
await shell.associations.openDefaultApps();
```

`window.open(spec)` takes every `WindowSpec` field the host knows: `minWidth`, `minHeight`, `decorations`, `center`, `fullscreen`, `skipTaskbar`, `x`, `y`, `parent`, `resizable`, `shadow`, `visible`, besides label, title, path, size, `focused` and `alwaysOnTop`.

## Surface, preview, legal receipt and update host commands

These wrap commands the crate registers through `rightkit_tauri_shell::handler!`. The preview and legal receipt commands need the crate's `host-services` feature; the update commands need `update`. Each command name is exported as a constant (`SURFACE_COMMAND`, `PREVIEW_COMMANDS`, `LEGAL_RECEIPT_COMMANDS`, `UPDATE_COMMANDS`, `UPDATE_PROGRESS_EVENT`), so another package can import the name instead of repeating it. `LEGAL_RECEIPT_COMMANDS` is identical to the constant in `@rightkit/onboarding`.

```ts
// Window surfaces: the ids are native config (ShellBuilder::surface); the host shapes the calling window.
const report = await shell.surface.set("setup"); // { surface, resized, revealed, skipped }

// Preview documents: the iframe URL is built by the host, never supplied by the renderer.
const { token, url } = await shell.preview.register("mdx", html);
await shell.preview.revoke(token);
await shell.preview.revokeAll();

// Legal receipt: send the declaration (no time); the host checks it against its manifest, stamps native UTC time, stores it.
const record = await shell.legal.read(); // LegalReceiptRecord | null
await shell.legal.create({ version, documentsHash, documents, eligibilityBasis: "individual", surface: "first_run" });
await shell.legal.reset();
```

Host rejections have the shape `{ code, message, details? }` (`HostError`); `asShellError` passes them through. There is no legal `write`: the renderer never supplies a stored record.

`shell.update` is the raw transport for the five `rightkit_update_*` commands and the `rightkit://update-progress` event (`check`, `status`, `download(handleId)`, `install(handleId)`, `cancel(handleId)`, `onProgress`). Its types (`UpdateHostCheck`, `UpdateHostStatus`, `UpdateHostDownload`, `UpdateHostInstall`, `UpdateHostProgress`, `UpdateHostError`) are closed: fixed fields, closed enums and message codes, and no URL, header, token or path. The replies are not validated here; `@rightkit/app-update/native` validates them and owns the policy and message text, so use it for the update UI.
