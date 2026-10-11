# Changelog

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
