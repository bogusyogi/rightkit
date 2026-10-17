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
| `@rightkit/app-update` | `createAutoUpdateClient(config)`, `fetchReleaseStatus(url, fetch?)`, `updateTierFromManifest(rawJson)`, `UPDATE_HOST_CONTRACT_VECTORS`; types `UpdateTier`, `AutoUpdateClient`, `AutoUpdateClientConfig`, `AutoUpdateStatus`, `ReleaseStatus`, `RuntimeUpdate`, `CheckForUpdate`, `UpdateHeaders`, `UpdateDownloadEvent`, `UpdateInstallOptions`, `UpdateHostContractVector` |

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

## Opt-in additions (all default off)

- `verifyMetadata(update, ctx)` config hook: reject update metadata before it is offered. Tauri's updater plugin still verifies the artifact signature against the app's configured pubkey; this package performs no signature verification itself (delegated, by design).
- `relaunch` + `relaunchAfterInstall`: relaunch after `ready`.
- `strictInstallHeaders`: never reuse manifest-check headers for the artifact download.
- `createBootProbation`: boot-probation / rollback-decision state machine; the host supplies storage and performs the restore.
- `createCheckGate`: check cache and debounce. `resolveChannel` / `validateChannels`: channel endpoint selection. `compareVersions` / `isNewerVersion`: semver compare. `assertSingleUpdatesVersion`: duplicate-copy guard.
- `@rightkit/app-update/contract-vectors`: test fixtures subpath (the main entry still re-exports them).

## GitHub Releases endpoint and the suite updater public key

A channel endpoint may be a GitHub Releases static manifest: `validateChannels` and `resolveChannel` accept any https URL, so a public GitHub app configures

```ts
const channels = { stable: { endpoints: ["https://github.com/<owner>/<repo>/releases/latest/download/latest.json"] } };
```

`right-release publish-update` with `distribution.provider: "github-releases"` uploads that `latest.json` (Tauri v2 static format) with the signed bundles; see the `@rightkit/release` README, "GitHub Releases updater lane". The URL resolves to the newest non-draft, non-prerelease release, so a prerelease channel needs its own endpoint.

There is one updater trust root for every RightKit app: the Right Suite updater key (key id `29395F9FF466261D`), the same key `sign-updater.mjs` signs with for the R2 lane. An app does not provision its own. The app's `tauri.conf.json` carries the key's public half in `plugins.updater.pubkey` (the base64 text of the key's `.pub` file); copy the value from any shipped suite app, or from the `.pub` file beside the updater private key on the release host. Tauri's updater verifies each downloaded artifact against that value; this package does no signature verification itself. Set `distribution.updaterKeyId: "29395F9FF466261D"` in the release config and `publish-update` also refuses to publish a signature made by any other key.

## Update overlay model and consent

`deriveUpdateOverlay(status)` maps an `AutoUpdateStatus` to a render-ready model (phase, bounded `progress`, `reason`, offered `actions`). An ineligible Pro update offers no download or install action. `createUpdateConsent()` is the per-action approval state: `request('download'|'install')`, `allowOnce(run)` (consumes the approval, exposes `busy` and `error`), `deny()`. Download and install are separate approvals. Signature and digest verification stay in the updater plugin and `verifyMetadata`; a failed run never proceeds to install.

### Staged download and install

`client.downloadAvailable()` is the download step. It confirms the already-verified, eligible update and returns `{ ok: true, handle, status }` with a `staged` status. Nothing is installed. `client.installStaged(handle)` is the install step; it accepts only the current handle, so a newer check invalidates it. `client.installAvailable()` runs both steps and keeps its previous status sequence.

The runtime adapter (Tauri updater) exposes only the combined `downloadAndInstall`, so the download step is a **verify-only prefetch**: it does not fetch the artifact bytes. The bytes are fetched during `installStaged`. Use `createUpdateConsentRunner(client)` as the `allowOnce` runner so the download approval runs only `downloadAvailable` and the install approval runs only `installStaged`.

### React subpath

`@rightkit/app-update/react` exports `UpdateOverlay` and `UpdateConsentCard`, typed against `UpdateOverlayModel` and `UpdateConsentState`. `react` is an optional peer dependency; the root entry never imports React.

## Known issues

- Omitting `getInstallHeaders` reuses the `getHeaders` result for the download. That can send a manifest credential to a different artifact origin. New apps should set `getInstallHeaders` explicitly, returning `undefined` for public patches.
- `UPDATE_HOST_CONTRACT_VECTORS` is test data exported from the runtime entry. Within this repository only the package's own test uses it. Consumer tests import it too (npm audit 2026-10-08, updates finding 1).
- `@rightkit/tauri` 0.2.0 in this tree declares `@rightkit/app-update` as a peer (`^0.2.5`, `packages/tauri/package.json:21-23`), not a pinned dependency; an older published `@rightkit/tauri` that pins `0.2.4` can still give a consumer two copies of this package (npm audit 2026-10-08).
