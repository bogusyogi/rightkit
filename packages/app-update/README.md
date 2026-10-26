# @rightkit/app-update

Runtime update-policy helper for Right Suite Tauri apps. It coordinates the app's updater check and install, classifies a release as `patch` or `update` from the manifest, treats only patches as eligible by default, auto-installs an eligible release only when `shouldAutoInstall` returns true (default: never, so the app calls `installAvailable()`), defers installs while the host's `canInstall` returns false, and reports status through callbacks.

It does not build, sign, publish or upload anything. `@rightkit/release` owns that lane. The package has no direct Tauri dependency. The app injects its updater `check` function, header loaders, eligibility policy and safe-to-install predicate.

| Item | Value |
|---|---|
| Where it runs | App webview UI (Tauri apps) |
| Rust pair | None. No Rust crate in `crates/` implements the updater. `rightkit-device` has no updater code. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/app-update` | `createAutoUpdateClient(config)`, `fetchReleaseStatus(url, fetch?)`, `updateTierFromManifest(rawJson)`, `UPDATE_HOST_CONTRACT_VECTORS`, `createPersistedCheckCache(options)`; types `PersistedCheckCache`, `PersistedCheckCacheOptions`, `PersistedCheckResult`, `CheckCacheStore`, `UpdateTier`, `AutoUpdateClient`, `AutoUpdateClientConfig`, `AutoUpdateStatus`, `ReleaseStatus`, `RuntimeUpdate`, `CheckForUpdate`, `UpdateHeaders`, `UpdateDownloadEvent`, `UpdateInstallOptions`, `UpdateHostContractVector` |
| `@rightkit/app-update/contract-vectors` | `UPDATE_HOST_CONTRACT_VECTORS`, `UpdateHostContractVector`, `UpdateTier` (the main entry re-exports them) |
| `@rightkit/app-update/react` | `UpdateOverlay`, `UpdateConsentCard` (optional `react` peer; the root entry never imports React) |
| `@rightkit/app-update/native` | `createNativeUpdateClient(transport)`, `NativeUpdateClientError`, `UPDATE_MESSAGES`; types `NativeUpdateClient`, `NativeUpdateTransport`, `UpdateCheckView`, `UpdateStatusView`, `UpdateProgress`, `UpdateDownloadResult`, `UpdateInstallResult`, `UpdateKind`, `UpdateRefusal`, `UpdateState`, `UpdateErrorCode`, `UpdateMessageCode`, `Unlisten`. The renderer side of the native updater; see [Native bridge](#native-bridge-rightkitnative). |
| `@rightkit/app-update/suite-api` | Right Suite API host helpers over the generated contract projection: `assertAppKey`, `releaseStatusUrl`, `updaterEndpoint`, `artifactHosts`, `publicInstallerUrl`, the contract constants (`RIGHT_SUITE_*`, `APP_ARTIFACT_HOSTS`) and the updater key helpers `updaterKeys`, `updaterSigningKeyId`, `updaterKeyIdOfSignature`, `selectUpdaterKey`; types `UpdaterKey`, `UpdaterKeyRefusal`. Dependency-free and browser-safe. |

Peer dependencies: none. No runtime dependencies.

## Example

```ts
import { createAutoUpdateClient, updateTierFromManifest } from "@rightkit/app-update";

const client = createAutoUpdateClient({
  check: (options) => check(options),        // the app's Tauri updater check
  getHeaders: () => ({ Authorization: `Bearer ${activationToken}` }),
  getInstallHeaders: async (tier) =>
    tier === "update" ? { Authorization: `Bearer ${await loadSignedEntitlement()}` } : undefined,
  onStatus: (status) => render(status),
});

const status = await client.checkOnce();
if (status.kind === "available") await client.installAvailable();
```

Status flow: `checking`, then `available`, then `downloading`, then `ready`. `ready` is emitted only after `downloadAndInstall` resolves. A failed install emits `{ kind: "error" }` and never `ready`.

## Opt-in additions

- `verifyMetadata(update, ctx)` config hook: reject update metadata before it is offered. Tauri's updater plugin still verifies the artifact signature against the app's configured pubkey; this package performs no signature verification itself (delegated, by design).
- `relaunch` + `relaunchAfterInstall`: relaunch after `ready`.
- `strictInstallHeaders` (default **on**; `false` is the deprecated escape): without `getInstallHeaders` the artifact download gets no headers, so manifest-check credentials are never reused for it.
- `createBootProbation`: boot-probation state machine; the host supplies storage. A version that does not acknowledge within `maxBootAttempts` boots yields `{ kind: "reinstall_required", failedVersion, fromVersion, guidance: "reinstall" }` (and `onBootFailure`); the host shows reinstall guidance. Nothing is restored: no `.previous` copy is kept or restored, and `previousInstallPath` / `PREVIOUS_SUFFIX` / `onRollback` are deprecated and only help hosts clean a stale copy left by an older RightKit.
- `createPersistedCheckCache(options)`: an update-check cache persisted through a host `CheckCacheStore` (`read`/`write` text) with a last-known-good fallback; a record stamped more than `maxFutureSkewMs` (default 60000) in the future is distrusted, failures are never persisted, corrupt records are treated as absent, and a write failure is reported as `persistError` without failing the check.
- `createCheckGate`: check cache and debounce. `resolveChannel` / `validateChannels`: channel endpoint selection. `compareVersions` / `isNewerVersion`: semver compare. `assertSingleUpdatesVersion`: duplicate-copy guard.
- `@rightkit/app-update/contract-vectors`: test fixtures subpath (the main entry still re-exports them). Four vectors (free and pro, patch and feature update); every app adapter runs them.

## GitHub Releases endpoint and the suite updater keys

A channel endpoint may be a GitHub Releases static manifest: `validateChannels` and `resolveChannel` accept any https URL, so a public GitHub app configures

```ts
const channels = { stable: { endpoints: ["https://github.com/<owner>/<repo>/releases/latest/download/latest.json"] } };
```

`right-release publish-update` with `distribution.provider: "github-releases"` uploads that `latest.json` (Tauri v2 static format) with the signed bundles; see the `@rightkit/release` README, "GitHub Releases updater lane". The URL resolves to the newest non-draft, non-prerelease release, so a prerelease channel needs its own endpoint.

The suite's updater keys are a list in the Right Suite contract (`contracts/right-suite-api.v1.json`): each entry is a key id, the key and an optional end date, and one of them, `updaterSigningKeyId`, is the key releases are signed with. The native shell host accepts every listed key and picks the one that checks a release by the key id inside the release signature; a key past its end date is refused, and so is a key the list does not carry. An app provisions none of this: for a Right Suite app the release tooling (`publish-update` and the seal step) takes the key and its id from the contract, refuses a declared key that is not the contract's, and refuses a signature made by any other key (`distribution.updaterKeyId` may name another listed key, otherwise the signing key is used). Tauri's updater verifies each downloaded artifact; this package does no signature verification itself. Rotate in this order: add the new key to the contract, ship a release signed by the old key (so installed apps learn both keys), and only then change `updaterSigningKeyId`. See `docs/SUITE-TRUST-ROOTS.md` in the RightKit repository for the runbook.

From `@rightkit/app-update/suite-api`, `updaterKeys()` and `updaterSigningKeyId()` read that list, and `selectUpdaterKey(signature, nowUnix?)` names the listed key a `latest.json` signature was made with, or refuses it as `malformed`, `unknown-key` or `key-expired`. It mirrors the native host's choice and only picks a key: it verifies nothing, so it is not a substitute for the updater plugin. Without `nowUnix`, a key that has an end date is refused.

## Update overlay model and consent

`deriveUpdateOverlay(status)` maps an `AutoUpdateStatus` to a render-ready model (phase, bounded `progress`, `reason`, offered `actions`). An ineligible Pro update offers no download or install action. `createUpdateConsent()` is the per-action approval state: `request('download'|'install')`, `allowOnce(run)` (consumes the approval, exposes `busy` and `error`), `deny()`. Download and install are separate approvals. Signature and digest verification stay in the updater plugin and `verifyMetadata`; a failed run never proceeds to install.

### Staged download and install

`client.downloadAvailable()` is the download step. It confirms the already-verified, eligible update and returns `{ ok: true, handle, status }` with a `staged` status. Nothing is installed. `client.installStaged(handle)` is the install step; it accepts only the current handle, so a newer check invalidates it. `client.installAvailable()` runs both steps and keeps its previous status sequence.

The runtime adapter (Tauri updater) exposes only the combined `downloadAndInstall`, so the download step is a **verify-only prefetch**: it does not fetch the artifact bytes. The bytes are fetched during `installStaged`. Use `createUpdateConsentRunner(client)` as the `allowOnce` runner so the download approval runs only `downloadAvailable` and the install approval runs only `installStaged`.

### Native bridge (`@rightkit/app-update/native`)

`createNativeUpdateClient(transport)` is the renderer's only view of a native updater. The transport is `{ invoke, listen }` (Tauri's `invoke` and `listen`). It calls `rightkit_update_check`, `rightkit_update_status`, `rightkit_update_download`, `rightkit_update_install` and `rightkit_update_cancel` and listens to `rightkit://update-progress`, and returns only whitelisted DTO fields (`UpdateCheckView`, `UpdateStatusView`, ...) with closed enums and fixed message codes. `UPDATE_MESSAGES` maps each code to its text; its keys are exactly the `UpdateMessage` codes `rightkit-tauri-shell` sends (a test compares them with `update.rs`) and the text names no licence, key or account. The error codes are `unconfigured`, `busy`, `unresolved`, `network`, `store`, `verify`, `forbidden`, `unsupported` and `unknown`. No URL, header, token or path crosses the IPC boundary. A refusal or failed download or install is a normal result (`state: "refused" | "failed"` with a `reason` and `message`). A rejected command, a malformed reply or a handle id of the wrong shape throws `NativeUpdateClientError` with a closed `code` and fixed text, never the native error text. `dispose()` removes the progress listener, including one whose `listen` call has not resolved yet, and later calls throw `unsupported`. This client needs a host that registers those five commands: the `rightkit-tauri-shell` `update` feature does, and until an app builds with it there is nothing to call. The in-process client above (`createAutoUpdateClient`) is unchanged and does not use it.

### React subpath

`@rightkit/app-update/react` exports `UpdateOverlay` and `UpdateConsentCard`, typed against `UpdateOverlayModel` and `UpdateConsentState`. `react` is an optional peer dependency; the root entry never imports React.

## Known issues

- `getInstallHeaders` is the place to put artifact credentials; set it explicitly (returning `undefined` for public patches). With the default `strictInstallHeaders` the download sends no headers when it is absent. Only `strictInstallHeaders: false` reuses the manifest-check headers, which can send a manifest credential to a different artifact origin; it is a deprecated compatibility escape.
- `UPDATE_HOST_CONTRACT_VECTORS` is test data exported from the runtime entry. Within this repository only the package's own test uses it. Consumer tests import it too (npm audit 2026-10-08, updates finding 1).
- `@rightkit/tauri` 0.2.0 in this tree declares `@rightkit/app-update` as a peer (`^0.2.5`, `packages/tauri/package.json:21-23`), not a pinned dependency; an older published `@rightkit/tauri` that pins `0.2.4` can still give a consumer two copies of this package (npm audit 2026-10-08).
