# @rightkit/onboarding

First-run onboarding framework for Right Suite apps: a legal agreement gate first, then ordered steps (permissions, model download, licence/trial, plus app-defined steps) with progress, skip and resume.

```tsx
import { Onboarding } from "@rightkit/onboarding";
import "@rightkit/onboarding/onboarding.css";

<Onboarding settings={settings} legal={legal} permissions={permissions} models={models} license={license}
  onComplete={finish} onSkip={later} />
```

- `settings` is a `SettingsBackend`; progress is stored under the `onboarding.state` key.
- The agreement is enforced until accepted, regardless of the saved step index. Acceptance is stored as `legalAcceptance: { version, documentsHash, acceptedAt (UTC ISO), eligibilityBasis }`; legacy `legalAccepted: true` is still read.
- `onComplete` and `onSkip` run only after the state was saved. A rejected save is shown in an alert and neither callback runs.

## Props

| Prop | Purpose |
| --- | --- |
| `requiredVersion` | Version the user must have accepted. A different or missing stored version re-prompts with the legal gate's `re-acceptance` variant. Legacy boolean acceptance is only re-prompted when this is set. |
| `isAccepted(state)` | Custom acceptance check; overrides `requiredVersion`. |
| `documentsHash` | Hash recorded with the acceptance (default: SHA-256 of document titles and URLs). |
| `beforeLegalGate()` | Awaited before the gate shows (e.g. size or reveal the window). The package does no window sizing itself. |
| `requireModel` | Continue/Finish on the models step wait for an installed model. |
| `autoStartDownload` | Start the first missing model download when the models step opens. |
| `modelsIntro` | App copy shown at the top of the models step. |
| `allowSkip` | Show "Set up later" (default `true`). |
| `titlebar` | Node rendered in a strip along the top. |
| `dragRegion` | Add `data-tauri-drag-region` to that strip (default `false`; height `--rk-onboarding-titlebar-h`, default 32px). |

## Custom steps and readiness

A step may set `canContinue(context)`; Continue/Finish stay disabled while it returns `false` (optional `blockedHint` is shown). Steps with async readiness call `context.setReady(boolean)` from `render` and read `context.ready[stepId]`. Back is disabled on the first step after the agreement.

## Models backend

`ModelsBackend.cancel` is optional; the Pause control appears only when it is supplied.

## Tests

`pnpm exec vitest run` runs the jsdom unit tests (`src/*.test.tsx`) and the headless-Chrome e2e.
