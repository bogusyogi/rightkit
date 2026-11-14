# Changelog

## 0.1.4

- New host option `importedThemeRetintsBrandRed` (default `true`, the previous output). When a host sets it to `false` with `setHostOptions({ importedThemeRetintsBrandRed: false })`, `applyImportedTheme` neither sets nor clears `--color-brand-red` and `--color-brand-red-2`; it retints only `--color-accent`, `--color-focus` and `--color-on-accent`.

## 0.1.3

Lets the host app replace more of its own frontend with this package. Every default is the previous behaviour; a host opts in.

- Host options, set once with `setHostOptions(partial)` (new, `appName`; `hostOption(key)` reads one). Defaults: `defaultPaletteLabel` "Default" (the default palette's label in `listPalettes()` and the Appearance tab fallback), `attentionHistoryLabel` "history preserved", `assistantExportLabel` "Assistant" (heading of an assistant turn in an exported session), `webhookSecretEnv` "RIGHTKIT_WEBHOOK_SECRET" (pre-filled secret binding of a new webhook trigger), `sandboxTierEnv` "RIGHTKIT_SANDBOX_TIER" (name shown in the sandbox tier panel), `newSessionOptionsPanel` false (the sidebar's "Session options" panel). Importable as `@rightkit/agent-ui-core/appName`; the hooks, components, workbench and settings packages re-export this module, so one call configures all of them (`setHostAppName` too). `agent-ui-approvals` and `agent-ui-web` keep their own copy.
- `listPalettes()` in `state/themeStore` returns `PALETTES` with the default palette labelled by `defaultPaletteLabel`; `PALETTES` itself is unchanged.
- `host/tauriHost` (new subpath, not in the root entry): `invoke`, `listen`, `UnlistenFn`, `getAppVersion`, `closeCurrentWindow`, `setBadgeCount`, `relaunchApp`. The package's own `@tauri-apps/api` and `@tauri-apps/plugin-process` imports go through it; no behaviour change.
- `lib/visibleInterval` (new): `setVisibleInterval(callback, ms)` returns a stop function; ticks are skipped while the window is hidden and one runs when it becomes visible.
- `lib/sessionTitle` (new): `sessionDisplayTitle(session)` shows the first message for a placeholder title ("CLI session", "New session", ...). `taskSwitcherTitle` uses it.
- `lib/daemon/transport` (new): `daemonFetch(config, input, init)` adds the bearer token and, only after `setDeclaredDaemonRoutes(operations)` (default: none), reports a request to a route the engine does not declare with `console.error`. The SSE, workspace, browser workspace, CLI provider, driver, delivery and permission clients now call it instead of `fetch`; default behaviour only differs by the token header on a request that had none.
- `InferenceAuthority` is `"direct" | "config"`; `SessionRow` gains optional `providerId`, `turnActive`, `preview`; `patchSession` accepts `providerId`.
- Incompatible-daemon check, off by default: `configureSupportedDaemonContract({ contractVersion: {min,max}, protocolVersion: {min,max}})` makes `getHealth` throw `IncompatibleDaemonError` (also exported with `isCompatibleDaemon`, `isIncompatibleDaemonError`, `getSupportedDaemonContract`) for a `/health` outside the range; the connection error projection classifies it `incompatible`. `HealthResponse` gains optional `version`, `contractVersion`, `protocolVersion`.
- The event stream loads a dormant session once (`postSessionsByIdResume`) when `/events` answers 404, then reconnects. Streamed transcript rows and the render cache key tolerate an assistant message without `toolCalls`.
- `qaBuild` and `config-host` document how a host maps its own QA and daemon-address environment names onto `__RIGHTKIT_QA__`, `VITE_RIGHTKIT_QA`, `VITE_RIGHTKIT_DAEMON_URL` and `VITE_RIGHTKIT_DAEMON_TOKEN` in its bundler `define`; no runtime flag.

## 0.1.2

- `state/proBridge` calls the shared `createProBridgeClient` of `@rightkit/license` instead of its own `invoke` calls. Same commands, arguments and best-effort behaviour; exports unchanged.

## 0.1.1 (unpublished)

- The legal store reads the receipt through `@rightkit/onboarding`'s `LegalReceiptPort`, whose `read()` may return a historical receipt (`{ record, current: false }`); that value is superseded, so the store keeps `null` and the gate asks again. Fixes the TS2322 type error that stopped the package from building.

## 0.1.0

- First release. Coding-agent UI core: daemon client, session and transcript types, stores and helpers.
