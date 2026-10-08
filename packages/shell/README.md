# @rightkit/shell

Typed renderer bridge for the `rightkit-shell` Tauri crate. It exposes window controls, menu and tray events, deep links, autostart, rich clipboard, notifications, presence and idle state, and engine calls. The package has no runtime dependencies.

| Item | Value |
|---|---|
| Where it runs | App webview UI (renderer side of the Tauri IPC) |
| Rust pair | `rightkit-shell` (webview hardening, window state, native menu, tray, single instance, deep links, autostart, clipboard, engine wiring) |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/shell` | `createShell(transport?)`, `tauriTransport()`, `EVENTS`, `asShellError`; types `ShellBridge`, `ShellTransport`, `ShellStatus`, `DeepLink`, `WindowSpec`, `MenuEntry`, `NotifyOutcome`, `PresenceState`, `IdleStatus`, `EngineEvent`, and others |

`createShell()` returns an object with `ready()`, `status()`, `quit()`, `openExternal(url)`, `window.do(action)`, `menu`, `tray`, `deepLinks`, `onSecondInstance`, `onStall`, `autostart`, `clipboard`, `notifications`, `presence`, `idle`, `paths`, `recovery`, `engine`, `onMenuAction`, and `setNativeTheme`.

Peer dependencies: none.

## Example

```ts
import { createShell } from "@rightkit/shell";

const shell = createShell(); // throws outside a Tauri webview
await shell.ready();         // call once after subscribing; queued deep links are delivered
await shell.openExternal("https://example.com");
```

`openExternal` accepts only `http`, `https` and `mailto`. The host refuses other schemes.

## Known issues

- The package has no test files. Its `src/` is one module, and `dist/` is built with `tsc` only.
- `createShell` throws when `__TAURI_INTERNALS__` is absent. Pass a custom `ShellTransport` for tests.
