# Changelog

## 0.6.3 — unpublished staging

Additive (PK10-1, PK10-2, PK10-3, PK10-5). Every default is unchanged; the change set has no breaking item, so under R5 (pre-1.0) it takes the next patch.

- `LegalReceiptPort.create(request)`: native receipt creation. The renderer declares `{ version, documentsHash, documents, eligibilityBasis, surface: "first_run" }` and no time; the native service stamps its own UTC time, stores the receipt and returns the record. A throw, a malformed record, or a record for another version, digest or eligibility keeps the gate closed; a retry after only the settings save failed reuses the created receipt (no second stamp). New props `receiptDocuments` (the declared set; default the gate's titles and URLs, never bodies) and the port methods `create` and `reset`.
- `LegalReceiptPort.write` is DEPRECATED and now optional: the renderer-built record (webview clock) is the legacy path, used only when the port has no `create`. A port with neither cannot accept (gate stays closed). Settings-only apps (no `legalReceipt`) are unchanged.
- `createNativeLegalReceiptPort({ invoke, commands? })` and `LEGAL_RECEIPT_COMMANDS` (`rightkit_legal_receipt_read|create|reset`, provisional until the `rightkit-tauri-shell` host service is published); host rejections become `Error`s.
- `reopenLegalAcceptance` (re-acceptance on demand; persisted `legalReopened`, honoured by `isLegalAccepted` even over a custom `isAccepted`) and `resetLegalAcceptance` (clears the receipt, then the settings copy).
- `licenseBackendFromView(client, options)` and `licenseStateFromView`: `LicenseBackend` over a native `LicenseView` client (structural; no import of `@rightkit/license`).
- `signIn` prop: built-in sign-in step after the licence step, and `ctx.signIn` is now actually provided to steps (it was typed but never set). `signInBackendFromOAuth(port, providers)` builds the backend over the app's `rightkit-oauth` commands. `signInStep` as an app step is unchanged.
- Permissions and models steps offer "Try again" when the list itself cannot load (offline); before, only an error message was shown with no way to retry.
- Exports `LicenseStepCopy` and `LicenseStepOptions`; README documents `layout`, the receipt paths, the adapters, and the `@rightkit/legal-ui` peer `>=0.4.1`.
- Tests: first run and relaunch at an incomplete step, re-acceptance on digest change, failed assent save (receipt and settings), permission cancel/deny/retry, offline model list and install failure, skip/back/finish, both layouts, native receipt, adapters.

Not built or run; verification is the root integration batch.

## 0.6.2 (2026-10-19)

- Adds `layout="contained"` to let a host size onboarding content; default `window` preserves viewport sizing.

## 0.6.1 (2026-10-19)

- Repairs onboarding continuation and persistence behavior.
- Changed: with no `requiredVersion` or `requiredDocumentsHash`, `isLegalAccepted` requires a recorded `legalAcceptance`; a bare legacy `legalAccepted: true` no longer passes the gate and is shown the re-acceptance screen once.

## 0.6.0 (2026-10-17)

Added (#619, CodeRight port; additive, defaults unchanged):
- `OnboardingStep.onContinue(ctx)`: runs before Continue/Finish advances. `false` or a throw keeps the step and shows the error; `void`/`true` advances.
- `onComplete(result)` now receives `{ completed, skipped, steps }`. `ctx.setResult(value)` stores a per-step value for `result.steps[id]`. Values live for this mount only.
- Per-step Skip: shown for `skippable` steps that are not the last and whose `canContinue` holds. It advances without marking the step complete; ids are kept in `PersistedOnboardingState.skippedSteps`.
- `OnboardingStep.continueLabel` for the primary button text. `labels` prop (`OnboardingLabels`: `skip`, `skipStep`, `back`, `continue`, `finish`, `loading`).
- `licenseStep(backend, { copy })` (`LicenseStepCopy`): `keyLabel`, `activateLabel`, `trialLabel`, `plansLabel`, `noLicenceDetail`. Defaults unchanged.
- An empty `title` hides the step heading.

Not built or run; verification is the root integration batch.

## 0.5.0

Additive (Orthic-Labs/rightkit#285). The default (`legalMode="gate"`) is unchanged.

- `legalMode`: `"gate"` (default), `"notice"` or `"none"`. `none` has no legal step at all; `notice` has no gate and no acceptance record, and a first "Notice" step lists the `legal` documents as links (free or open-source apps that still hold their notice). In both, the first app step is reachable and Back works from the second step on.
- `legal` is optional unless `legalMode` is `gate` (a gate without `legal` throws). New `appName` prop for the step rail when there is no `legal`. New type `LegalMode`.
- With no steps at all in `none` mode the component shows "Nothing to set up" and a Finish button.

## 0.4.0

- Add `signInStep(backend)` and the `SignInBackend` / `SignInProvider` types (Orthic-Labs/rightkit#279). Buttons call the app's shared `rightkit-oauth` command, which opens the user's DEFAULT browser (never an in-app webview); the copy says "Opens your browser". Additive. Not built or run locally.

## 0.3.3

Republish. 0.3.2 is on npm without the change below; a consumer on 0.3.2 cannot reach it.

- Fixed: `isLegalAccepted` compared only the version, so a legal text that changed without a version bump never re-prompted. New optional `requiredDocumentsHash` (prop on `Onboarding`, fourth argument of `isLegalAccepted`): when given, the stored acceptance must carry the same hash. `documentsHash` stays record-only. Not built or run locally.

## 0.3.2

- Republish with `dist/` present. 0.3.1 was published from an unbuilt tree and cannot be imported (exports point at ./dist/index.js); do not pin 0.3.1. `prepack` now builds.

## 0.3.1 — 2026-10-10

- Test-only: the real-browser e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade (`@rightkit/qa/chrome` re-exports it). No runtime change.

## 0.2.3 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.2.2

### Added
- `shouldAutoStart?(entry, models)`: the app picks which missing model auto-starts when the models step first opens (first `missing` entry returning true; no "something is installed" guard, the app decides). `autoStartDownload` behaves as before.
- `onModelsStepEnter?(ctx)`: full control, called once when the step opens and the list has loaded; `ctx` has `models`, `start(id)` and `refresh()`. Replaces `shouldAutoStart` and `autoStartDownload`; a throw is shown in the step. New types `ModelsStepEnterContext`, `ModelsAutoStart`; `ModelsStepOptions` gains the same options and `startGuard`.

### Fixed
- The auto-start guard now lives in `Onboarding`, not in the models step, so Back/Next (which remounts the step) and re-renders cannot start a download a second time.

## 0.2.1

### Added
- `legalReceipt?: LegalReceiptPort` (`read()` / `write(record)`, sync or async): the app supplies and reads its own acceptance receipt (e.g. ScrapeRight's four-field v1), mapped to and from `LegalAcceptanceRecord`. The receipt overrides the settings backend's acceptance on load (`null`, a throw or an incomplete record mean not accepted), `requiredVersion` / `isAccepted` evaluate against it, and `write` is awaited on agree before settings are patched (a failed write shows the gate's save-failure alert and records nothing). README has the 4-field mapping.
- `legalDragRegion` (default `false`).

### Fixed
- `dragRegion` no longer renders on the legal stage (loading and gate); the gate has no drag strip, as before 0.2.0. Set `legalDragRegion: true` to restore it there. `titlebar` content is unchanged. Setup steps are unaffected.

## 0.2.0

### Fixed
- Back on the first step after the agreement no longer navigates to the (empty) Agreement step; it is disabled, and the Agreement rail entry is not clickable once accepted.
- Continue/Finish now honour a per-step readiness contract and stay disabled until satisfied.
- The models step shows Pause only when the backend supplies `cancel` (`ModelsBackend.cancel` is now optional).
- Legal acceptance is stored as a full record (`legalAcceptance`: `version`, `documentsHash`, `acceptedAt` UTC ISO, `eligibilityBasis`). The legacy `legalAccepted`/`eligibilityBasis` fields are still written and read.

### Added
- `OnboardingStep.canContinue(context)`, `blockedHint`; `OnboardingStepContext.ready` / `setReady`.
- `requireModel` (models step needs an installed model), `autoStartDownload`, `modelsIntro`, `modelsStep(backend, { required, autoStartDownload, intro })`.
- `requiredVersion`, `isAccepted(state)`, `documentsHash`, `beforeLegalGate()`; the gate re-prompts with `variant: "re-acceptance"` when the stored version differs. A legacy boolean acceptance is only re-prompted when `requiredVersion` is set.
- `allowSkip` (default true), `titlebar` slot, opt-in `dragRegion` (`data-tauri-drag-region` strip; height via `--rk-onboarding-titlebar-h`).
- `isLegalAccepted()` export and types `LegalAcceptanceRecord`, `EligibilityBasis`, `ModelsStepOptions`.
- Unit tests (jsdom + Testing Library) in `src/Onboarding.test.tsx`.

### Notes
- The package never sized the window; there is no 960x560 behaviour to remove. Use `beforeLegalGate` for any window sizing.
- CSS tokens use the `--rk-*` names.
