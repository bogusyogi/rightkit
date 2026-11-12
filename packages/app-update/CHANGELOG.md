# Changelog

## 0.4.3 (unpublished)

- Issue #1019: native deferred install. `UPDATE_MESSAGES` gains `deferred` (the native `UpdateMessage::Deferred`), `UpdateInstallResult.reason` gains `'deferred'`, and `NativeUpdateClient.setBusy(busy)` calls `rightkit_update_set_busy`. In `createNativeAutoUpdateClient`, an install the host refused as `deferred` keeps the staged update current (the host kept the handle and its bytes) and emits `{ kind: 'deferred', version, tier, reason }`, so `installStaged` or `installAvailable` can run again after `setBusy(false)`. New option `messages?: Partial<Record<UpdateMessageCode, string>>` lets the app supply its own words for any message code, used for every error and deferred text the client emits. Additive, Breaking: none. Not compiled.
- Issue #448: boot-stage bindings. `NativeBootProbation.reportStage(stage)` calls `rightkit_boot_stage` (the native `BootProbation::report_stage` in `rightkit-app-update`; 1 to 64 of letters, digits, `_`, `-`, `.`). `@rightkit/app-update/react` gains `BootProbationReporter({ stage, ok, healthy?, mountedStage?, onError? })`: once the UI is up it calls `stage` with `renderer-mounted`, then `ok` once while `healthy` is not false; failures go to `onError` and never break the UI. Pair: `stage={probation.reportStage} ok={probation.acknowledgeHealthy}`. The fetch half of #448 is `createNativeFetch` in `@rightkit/tauri`. Additive, Breaking: none. The command is registered by `rightkit-tauri-shell` (feature `app-update`). Not compiled.
- Issue #394: the persisted check cache with stale-on-failure fallback is `createPersistedCheckCache` in `src/check-cache.ts` (since 0.4.1); no further change.
- Issues #1008, #1097: the `./suite-api` table (`APP_ARTIFACT_HOSTS`, `RIGHT_SUITE_API_CONTRACT`) now lists `cutright` (public host) and `scraperight` (gated and public hosts), regenerated from `contracts/right-suite-api.v1.json`, so `artifactHosts("cutright")` and `artifactHosts("scraperight")` return their hosts. Additive, Breaking: none. Not compiled.
- Issue #1131: `@rightkit/app-update/native` gains `createNativeAutoUpdateClient(native, { proUpdateVersion?, beginBootProbation?, relaunch?, onStatus?, onError? })`, an `AutoUpdateClient` over the native update client. It maps the native check to `available` (eligible, or `pro_required` / `auto_install_off` when not entitled), runs download with percentage progress and a `staged` status, and calls `beginBootProbation(toVersion)` before the native install and `relaunch` after it accepted the install. It never installs from `checkOnce`. Also `createNativeBootProbation(transport)`, the typed contract for the native probation record over the commands `rightkit-tauri-shell` registers (`rightkit_boot_status`, `rightkit_boot_stage`, `rightkit_boot_ok`, `rightkit_boot_begin`; reconciled with the Rust names in 0.4.3: the native `reinstallRequired` kind maps to `reinstall_required`, `unknown` to `none`; `begin(toVersion)` added). Additive, Breaking: none. Not compiled.
- Issue #757: `updaterTargetForPlatform(platform)` maps `macos` and `darwin` to `darwin`, `windows` and `win32` to `windows`, and `linux` to `linux`; any other name throws.
- Issue #731: `buildActivationUpdateHeaders({ activationToken, headerPrefix, app, device })` returns the `Authorization: Bearer` header and the `X-<prefix>-App` and `X-<prefix>-Device` headers for `getHeaders`; it validates each value and never echoes the token. Additive, Breaking: none. Not compiled.

## Unreleased

- `createUpdateSession(createClient, { transport, relaunch, busy?, autoInstall?, background?, initialStatus?, onTransition?, onError? })` lifts the update session of the live app into the package (it was named as this package's own when the dictation UI copy was deleted). It keeps the latest status with `subscribe`, a `check` that suppresses auto-install while it runs, a single-flight `installAndRelaunch` that waits for the host's busy state, loops while the result is `deferred` and relaunches on `ready`, and `startBackground`, which listens to the native background event first and then reads the cached status. Only status transitions reach `onTransition`. The busy predicate with its state command and event, the `auto_install` setting read and the background command and event names are host configuration. It works over `createAutoUpdateClient` and `createNativeAutoUpdateClient`. Differences from the app copy: overlapping manual checks are counted, a `staged` status is not interrupted by a background result, and a failing background `checkOnce` is swallowed. Additive, Breaking: none.
- `proUpdateVersion({ app, platform, currentVersion, url?, fetch? })` (or `{ readStatus, currentVersion }` for a host that reads the status natively) answers the newer `update`-tier version from the release status, or `null`; it never throws. The URL is `releaseStatusUrl(app)` unless `url` names an app whose API is not on the suite origin. Additive, Breaking: none.
- `./native`: `NativeUpdateClient.relaunch()` calls `rightkit_update_relaunch`. Additive for callers; a hand-written `NativeUpdateClient` must add the method.

- `updaterKeyIdOfSignature` trims ASCII whitespace only, as the Rust and JavaScript readers do (`String.trim` also stripped U+FEFF); the shared vectors have both boundary cases.

- `./suite-api` gains `updaterKeys()`, `updaterSigningKeyId()`, `updaterKeyIdOfSignature(signature)` and `selectUpdaterKey(signature, nowUnix?, keys?)`, with the types `UpdaterKey` and `UpdaterKeyRefusal`, over the contract's list of accepted updater keys. They read the key id out of a minisign release signature as `latest.json` carries it and pick the listed key it names, or refuse it as `malformed`, `unknown-key` or `key-expired`; a key with an end date is refused when no clock is given. They mirror `rightkit-license` `suite_api`, so a release the native host accepts is a release they accept, and they verify no signature: the updater plugin does. Additive, Breaking: none.
- `./suite-api` also re-exports the contract's new trust-root constants from the generated projection: `RIGHT_SUITE_LICENSE_PUBLIC_KEY_HEX`, `RIGHT_SUITE_UPDATER_SIGNING_KEY_ID`, `RIGHT_SUITE_UPDATER_PUBLIC_KEYS`, `RIGHT_SUITE_APP_KEYS`, `RIGHT_SUITE_APP_LICENSE_HOSTS` and `RIGHT_SUITE_PENDING_NATIVE_UPDATER`.
- Docs: the README entry table lists `./suite-api`, and the updater key paragraph now describes the contract's key list, the single signing key and the rotation order.

## 0.4.1 — unpublished staging

- Added `@rightkit/app-update/native`: the safe-DTO bridge to the native `rightkit_update_*` commands of the rightkit-tauri-shell `update` feature (M12/G1 v3.1-v3.2). It provides check, status/reconnect, download progress, install, cancel and dispose, projects only whitelisted DTO fields and closed enums, and uses fixed error text. No URL, header, token or path crosses IPC. The existing exports are unchanged.
- Added `createPersistedCheckCache(options)` (`src/check-cache.ts`): an update-check cache persisted through a host `CheckCacheStore` (`read`/`write` text), with a last-known-good fallback when a check fails. Clock-rollback rule: a record stamped more than `maxFutureSkewMs` (default 60000) ahead of `now()` is distrusted, so a real check runs and the record is neither served nor used as fallback. Failures are never persisted; corrupt or invalid records are treated as absent and never deleted; a write failure returns the fresh result with `persistError`. Additive, Breaking: none. `createCheckGate` is unchanged. Not built or run locally.
- `UPDATE_MESSAGES` (`./native`) is de-licenced (PK13): its keys are the fixed message codes the native update host sends (`UpdateMessage` in `crates/rightkit-tauri-shell/src/update.rs`) (`none`, `unconfigured`, `busy`, `unresolved`, `networkUnavailable`, `revoked`, `expired`, `forbidden`, `unsupported`, `updateRefused`, `updateFailed`, `verifyFailed`, `storeFailed`, `unknown`) and the text speaks of updates, not licences. Removed from the unpublished `./native` surface: the licence-activation keys (`keyNotAccepted`, `deviceLimit`, `rateLimited`, `rejectedOther`, `clockRollback`, `clockUnavailable`, `invalidToken`, `issuedInTheFuture`, `invalidLifecycle`, `migrationRequired`, `clockRepairRequired`, `trialRequested`, `trialRefused`), the error codes `rejected` and `clock` (now read as `unknown`), the `UpdateRejectReason` type and `NativeUpdateClientError#reason`. No published release carried `./native`, so nothing published changes. A test compares the keys with the native `UpdateMessage` enum in `update.rs`.
- `assertSingleUpdatesVersion` (existing, unchanged) gains a test for its default-version path.
- Native error code `unknown` (sent by the rightkit-tauri-shell 0.5.0 update host, `crates/rightkit-tauri-shell/src/update.rs`) is now accepted by the `./native` error decoder (`src/native/index.ts` `errorCodes`), so its message code is kept instead of falling back to the generic error. Working-tree `dist/native/index.js` is synced to the same decoder (it previously lacked `unknown`); a test now locks both behaviours. `dist/` is gitignored and `prepack` rebuilds it from source at pack time, so the published bytes always come from `src/`.
- Docs: the README entry table now lists `./contract-vectors`, `./react` and `./native`; documents the native bridge, the persisted check cache and `reinstall_required` (no `.previous` restore); and states that `strictInstallHeaders` is on by default.

## 0.4.0 (2026-10-19)

- Repairs staged-update validation and installation state handling.

## 0.3.0 (2026-10-17)

- Added `downloadAvailable()` and `installStaged(handle)` (verify-only download prefetch), `createUpdateConsentRunner`, a `staged` status and phase, and the `./react` subpath (`UpdateOverlay`, `UpdateConsentCard`).

- Add `deriveUpdateOverlay(status)` and `createUpdateConsent()` (Orthic-Labs/rightkit#625): a framework-free overlay view model over `AutoUpdateStatus` (checking, available incl. non-eligible Pro reason, deferred, downloading with 0..100 bounded progress, ready, error) and a per-action consent controller (download and install are separate approvals; Allow Once / Don't Allow; busy and error state). The React `UpdateOverlay`/`UpdateConsentCard` components are not included (they need a react peer dependency; see issue). Additive, Breaking: none. Not built or run locally.

- Document and test a GitHub Releases `latest.json` URL as a channel endpoint, and how an app reads the suite updater public key (Orthic-Labs/rightkit#237). Documentation and a test only; no code or API change. Not built or run locally; CI is the compiler.

- Define the rollback location (Orthic-Labs/rightkit#261; "no new id, no new folder, for every app"). `previousInstallPath(installPath)` is `<installPath>.previous`, beside the fixed install path; `createBootProbation().begin(from, to, { installPath })` records it, and a `rollback` decision and the `onRollback` hook carry `installPath` and the copy to restore (`restorePath`): the host renames `<install>.previous` back over the install path and relaunches from the install path. The update replaces the app in place at one fixed path, the replaced copy becomes the single `.previous`, and nothing is ever executed from a new, versioned or temporary path or from `.previous`. Optional and backward compatible: without `installPath` behaviour is unchanged. Not built or run locally; CI is the compiler.

- Renamed from @rightkit/updates; @rightkit/updates@0.2.5 remains published under the old name.
- Add optional `verifyMetadata` hook for signed-metadata validation; default off, Breaking: none.
- Add `relaunch` and `relaunchAfterInstall` (default false) for host-driven relaunch after install; default off, Breaking: none.
- Add `src/probation.ts` boot probation state machine with host-injected storage and `onRollback`; default off, Breaking: none.
- Add `createCheckGate` check cache and debounce helper (failures never cached); default off, Breaking: none.
- Add `validateChannels` and `resolveChannel` for update channel name and https URL validation; default off, Breaking: none.
- Add `compareVersions` and `isNewerVersion` semver comparison exports; default off, Breaking: none.
- Add `strictInstallHeaders` opt-in so artifact downloads send no manifest-check headers; default off, Breaking: none.
- Add `registerUpdatesInstance` and `assertSingleUpdatesVersion` duplicate-version guard; opt-in, Breaking: none.
- Add `./contract-vectors` package subpath; main entry still re-exports `UPDATE_HOST_CONTRACT_VECTORS`, Breaking: none.

## 0.2.5 — 2026-10-08

- Emit `ready` once, after `downloadAndInstall` resolves successfully, for manual
  & automatic installs. Download `Finished` events no longer signal readiness.
- Installation failures emit typed `error` status & call `onError` without
  emitting `ready`, preventing premature Restart prompts.
- Preserve public types, method signatures & download progress events.
