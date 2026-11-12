# @rightkit/system-ui

System views for desktop utility apps: Storage (disk map, search, file actions, compress), Cleanup, Duplicates,
Drive health, Apps (inventory, updates, uninstall, startup items) and Activity, with the chart and live-metrics
helpers. The views hold no IPC and no product: they render over typed clients the host supplies.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.1 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/system-ui` | The views, the clients (`api`, `appsApi`) and their types, the host contract, chart and metrics helpers. |
| `@rightkit/system-ui/host` | `configureSystemHost`, `systemHost`, `SystemHost`, `SystemEventMap`. |
| `@rightkit/system-ui/api` | `SystemClient`, `AppsClient`, the data model and formatting helpers (`bytes`, `ago`, `tone`, ...). |
| `@rightkit/system-ui/metrics`, `/activityLease`, `/visible`, `/components/Chart`, `/views/Storage` | Live metrics, the activity lease, visibility-aware intervals, charts, the Storage view and its `Bar`. |
| `@rightkit/system-ui/tauri` | `tauriSystemHost`, `tauriSystemClient`, `tauriAppsClient`, `tauriSystemSubscribe`: the Tauri implementation (the only entry that imports `@tauri-apps/api`). |
| `@rightkit/system-ui/styles.css`, `/views/monitor.css` | Stylesheets (Node cannot import CSS from JS; hosts import `styles.css` once). |

## Host contract

Call `configureSystemHost` once per window, before rendering:

| Field | What the host supplies |
|---|---|
| `client` | `SystemClient`: storage scan and search, file identity and actions, disks, processes, cleanup, duplicates, drive health, activity, metrics and the activity lease, over the host's own transport. |
| `apps` | `AppsClient`: the app inventory, updates, startup items and the app to open at start. |
| `subscribe` | Typed events (`SystemEventMap`): index changes, app rows, icons, update jobs, leftovers, and the window's `visibility` (polling pauses while hidden). |
| `appName` | The product name in user-facing copy. |
| `storagePrefix` | Prefix of the local-storage keys the views keep their selection under. |

`api` and `appsApi` are the configured clients; a call before `configureSystemHost` throws.

### Tauri host

```ts
import { configureSystemHost } from "@rightkit/system-ui/host";
import { tauriSystemHost } from "@rightkit/system-ui/tauri";
configureSystemHost(tauriSystemHost({ appName: "Your App", storagePrefix: "your-app.hub." }));
```

The commands and arguments are the ones 0.1.x called (the `SystemClient` and `AppsClient` commands and 9 events); the
app's `src-tauri` must serve them. Options: `listen` (how events reach the window, default Tauri's) and `events` (native
event names; the visibility event defaults to `hub-visibility`). That one call is also all a host needs for
`@rightkit/agent-ui-approvals`' Monitor, Overview and Settings screens, which read `api` and `appsApi` from here and
throw until `configureSystemHost` has run.

## Not here

Chat, nearby sharing, launcher settings, onboarding, legal, QR and the device-link, remote-unlock, browser-login
and secure-box clients are not system UI: see CHANGELOG 0.2.0 for where each went.

## Testing

```sh
pnpm --dir packages/system-ui typecheck
```
