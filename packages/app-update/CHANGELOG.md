# Changelog

## 0.4.1 — unpublished staging

- Added `@rightkit/app-update/native`: the safe-DTO bridge to the native `rightkit_update_*` commands of the rightkit-tauri-shell `update` feature (M12/G1 v3.1-v3.2). It provides check, status/reconnect, download progress, install, cancel and dispose, projects only whitelisted DTO fields and closed enums, and uses fixed error text. No URL, header, token or path crosses IPC. The existing exports are unchanged.
- Added `createPersistedCheckCache(options)` (`src/check-cache.ts`): an update-check cache persisted through a host `CheckCacheStore` (`read`/`write` text), with a last-known-good fallback when a check fails. Clock-rollback rule: a record stamped more than `maxFutureSkewMs` (default 60000) ahead of `now()` is distrusted, so a real check runs and the record is neither served nor used as fallback. Failures are never persisted; corrupt or invalid records are treated as absent and never deleted; a write failure returns the fresh result with `persistError`. Additive, Breaking: none. `createCheckGate` is unchanged. Not built or run locally.
- `UPDATE_MESSAGES` (`./native`) is de-licenced (PK13): its keys are the fixed message codes the native update host sends (`UpdateMessage` in `crates/rightkit-tauri-shell/src/update.rs`, which lands with PK13-2 and is not yet in the tree) (`none`, `unconfigured`, `busy`, `unresolved`, `networkUnavailable`, `revoked`, `expired`, `forbidden`, `unsupported`, `updateRefused`, `updateFailed`, `verifyFailed`, `storeFailed`, `unknown`) and the text speaks of updates, not licences. Removed from the unpublished `./native` surface: the licence-activation keys (`keyNotAccepted`, `deviceLimit`, `rateLimited`, `rejectedOther`, `clockRollback`, `clockUnavailable`, `invalidToken`, `issuedInTheFuture`, `invalidLifecycle`, `migrationRequired`, `clockRepairRequired`, `trialRequested`, `trialRefused`), the error codes `rejected` and `clock` (now read as `unknown`), the `UpdateRejectReason` type and `NativeUpdateClientError#reason`. No published release carried `./native`, so nothing published changes. The key comparison with the native enum returns without asserting while `update.rs` is absent from the tree.
- `assertSingleUpdatesVersion` (existing, unchanged) gains a test for its default-version path.
- Native error code `unknown` (sent by the rightkit-tauri-shell 0.5.0 update host, `crates/rightkit-tauri-shell/src/update.rs`) is now accepted by the `./native` error decoder (`src/native/index.ts` `errorCodes`), so its message code is kept instead of falling back to the generic error.
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
