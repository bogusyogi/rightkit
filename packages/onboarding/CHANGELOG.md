# Changelog

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
