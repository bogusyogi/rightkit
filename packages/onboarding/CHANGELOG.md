# Changelog

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
