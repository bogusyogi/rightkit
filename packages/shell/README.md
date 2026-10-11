# @rightkit/shell

Typed renderer bridge for the `rightkit-shell` Tauri crate. It exposes window controls, menu and tray events, deep links, autostart, rich clipboard, notifications, presence and idle state, quit drain, and engine calls. The package has no runtime dependencies.

| Item | Value |
|---|---|
| Where it runs | App webview UI (renderer side of the Tauri IPC) |
| Rust pair | `rightkit-shell` (webview hardening, window state, native menu, tray, single instance, deep links, autostart, clipboard, engine wiring) |
| Version | 0.1.1 (see CHANGELOG.md) |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/shell` | `createShell(transport?, options?)`, `tauriTransport()`, `EVENTS`, `asShellError`, `DEFAULT_QUIT_TIMEOUT_MS`; types `ShellBridge`, `ShellTransport`, `ShellStatus`, `ShellCapabilities`, `QuitHandler`, `QuitOptions`, `QuitReport`, `ShellOptions`, `DeepLink`, `WindowSpec`, `MenuEntry`, `NotificationRequest`, `NotifyOutcome`, `PresenceState`, `IdleStatus`, `EngineEvent`, and others |

`createShell()` returns an object with `ready()`, `status()`, `capabilities()`, `quit(options?)`, `onQuitRequested(handler)`, `openExternal(url)`, `window.do(action)`, `menu`, `tray`, `deepLinks`, `onSecondInstance`, `onStall`, `autostart`, `clipboard`, `notifications`, `presence`, `idle`, `paths`, `recovery`, `engine`, and `bridge()`. `bridge()` returns a `ShellBridge` with `window.minimize()`, `window.toggleMaximize()`, `window.close()`, `openExternal(url)`, `onMenuAction(handler)` and `setNativeTheme(theme)`; `onMenuAction` and `setNativeTheme` are not on the `createShell()` object itself.

## Example

```ts
import { createShell } from "@rightkit/shell";

const shell = createShell(); // throws outside a Tauri webview
await shell.ready();         // call once after subscribing; queued deep links are delivered
await shell.openExternal("https://example.com");
```

`openExternal` accepts only `http`, `https` and `mailto`. The host refuses other schemes.

## Host requirement: register the shell commands (Rust apps)

The `rightkit-shell` crate does not register its own `rk_shell_*` commands. Tauri takes one invoke handler per builder, so the app must register them in its builder with `rightkit_shell::handler!`, which includes the shell commands plus the app's own:

```rust
tauri::Builder::default()
    .plugin(rightkit_shell::Shell::builder("myapp").app_name("MyApp").build().plugin())
    .invoke_handler(rightkit_shell::handler![my_app_command])
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
- Platforms: rightkit-shell posts notifications natively on macOS and Windows; other platforms report `unsupported`. The bypass applies on those native platforms only.
- The bypass covers the shell's Do-Not-Disturb. It does not override the OS Focus mode, which can still hold banners.

## Capabilities and host support

`shell.capabilities()` returns `{ quitDrain, notifyBypassDnd }`. A host without `rk_shell_capabilities` reports both as `false`. Features gated by a capability fail with a typed `unsupported` error that names what is missing.

| Feature | Renderer side (this package) | Host support needed in `rightkit-shell` |
|---|---|---|
| Quit drain | `onQuitRequested`, `quit`, `quitTimeoutMs` | `rk_shell_capabilities` returns `quitDrain: true` (only when the app opts in with `ShellBuilder::quit_drain(true)` or `quit_drain_bound(d)`); `rk_shell_finish_quit` command (sets exiting, then `app.exit(0)`); `rightkit-shell://quit-requested` event emitted on a host quit request instead of exiting, with `prevent_exit`; the host exits after its own bound (default 15 s, at most 120 s) if the renderer never calls finish |
| Notification bypass | `notifications.post({ bypassDoNotDisturb })` | `rk_shell_capabilities` returns `notifyBypassDnd: true`; `rk_shell_notify` request field `bypassDoNotDisturb` (serde default `false`) skips the `effective_dnd()` check |
| Custom tray events | `tray.onCustom` | none |

`rk_shell_capabilities` is read-only and returns `{ quitDrain: bool, notifyBypassDnd: bool }`. The `rightkit-shell` crate implements both; it always reports `notifyBypassDnd: true` and reports `quitDrain: true` only when the app opts in. On a host without the support, the capability-gated calls reject with `unsupported`.

## Known issues

- `createShell` throws when `__TAURI_INTERNALS__` is absent. Pass a custom `ShellTransport` for tests; `tests/shell.test.mjs` shows one.
- The quit drain is off unless the host opts in with `ShellBuilder::quit_drain(true)`. Until then `onQuitRequested` rejects with `unsupported` and `quit()` quits at once.
