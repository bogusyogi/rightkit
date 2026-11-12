# Changelog

## Unreleased

- Added: `./tauri` entry with `tauriSystemHost({ appName, storagePrefix }, options?)` (the whole `SystemHost`), `tauriSystemClient()`, `tauriAppsClient()` and `tauriSystemSubscribe(options?)`: the 0.1.x commands and argument shapes, the `metrics_*`, `activity_lease`, `cleanup_history` and `initial_app` commands the views called directly, and the 9 events, with `listen` and `events` options. Dependency added: `@tauri-apps/api`, used by that entry only.

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review, pending the owner's confirmation.

- BREAKING, transport injected: no `@tauri-apps/api` import and no command name is left in the package. `api` and
  `appsApi` are the host's `SystemClient` and `AppsClient` (typed interfaces), configured once with
  `configureSystemHost({ client, apps, subscribe, appName, storagePrefix })`; native events arrive through the
  typed `subscribe` (`SystemEventMap`). Cleanup's private second client over the same commands is gone (it uses
  `api.cleanup*`, with `cleanupHistory` added and its result types merged into the client's); metrics, the
  activity lease, `initial_app` and the window-visibility event go through the clients too.
- BREAKING, product strings host-supplied: "Pulse" in Cleanup, Activity, Storage, Drive health and Apps is
  `appName`; the `pulse.hub.*` local-storage keys are `storagePrefix` + the view's key.
- BREAKING, non-system modules out:
  - deleted (Pulse keeps its own copies in its hub; the owning packages are `@rightkit/agent-ui-*` for chat,
    `@rightkit/onboarding` for onboarding, `@rightkit/legal-ui` for legal): `views/Chat`, `chat.css`,
    `onboarding/*`, `lib/legal`, `lib/onboardingSettings`, `lib/rk` (logging and device clients created at
    import time), and the unused nearby-sharing client (`shareApi`; nearby sharing is `@rightkit/send-ui`'s);
  - moved to `@rightkit/agent-ui-approvals` (`src/remote`), the Pulse hub screens that are their only users:
    the device-link, remote-approval, remote-unlock, browser-login, secure-box and notch-setting clients and
    types, `views/LauncherSettings`, `views/NearbySettings`, `views/settingsKit`, `lib/qr`, `lib/chatKind`,
    `membrane.css`, `settings.css`.
- Dependencies removed: `@tauri-apps/api`, `@rightkit/legal-ui`, `@rightkit/logs`, `@rightkit/onboarding`,
  `@rightkit/tauri`, `@rightkit/tauri-shell`. Exports: `./host` added; `./lib/qr`, `./onboarding/gate`,
  `./views/LauncherSettings`, `./views/NearbySettings`, `./views/settingsKit`, `./views/membrane.css`,
  `./views/settings.css` removed.
- Monitor and Overview are not added: they sit in `@rightkit/agent-ui-approvals` (`src/remote`) with Pulse's other
  hub screens; the description no longer names them.

## 0.1.0

- First release. System monitor views (activity, storage, cleanup, duplicates, drive health, apps, chat) and chart helpers for desktop utility apps.
