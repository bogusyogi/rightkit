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
| `shouldAutoStart(entry, models)` | Choose which missing model auto-starts when the models step first opens (first `missing` entry returning true). Replaces `autoStartDownload`'s rule, including its "a model is already installed" guard. Once per mount, across re-renders and Back/Next. |
| `onModelsStepEnter(ctx)` | Full control, called once when the step opens and the list has loaded: `ctx.models`, `ctx.start(id)`, `ctx.refresh()`. Replaces `shouldAutoStart` and `autoStartDownload`; a throw is shown in the step. |
| `modelsIntro` | App copy shown at the top of the models step. |
| `allowSkip` | Show "Set up later" (default `true`). |
| `titlebar` | Node rendered in a strip along the top. |
| `dragRegion` | Add `data-tauri-drag-region` to that strip on the setup steps (default `false`; height `--rk-onboarding-titlebar-h`, default 32px). Never rendered on the legal stage. |
| `legalDragRegion` | Also render the drag region on the legal stage (default `false`; the legal gate has no strip, and `titlebar` content alone never adds the attribute). |
| `legalReceipt` | `{ read(), write(record) }`: the app's own acceptance receipt. See below. |

## App-owned legal receipt

An app that already persists its own acceptance receipt passes `legalReceipt` and keeps that store as the source of truth. `read()` runs on load (sync or async) and returns a `LegalAcceptanceRecord` (`version`, `documentsHash`, `acceptedAt`, `eligibilityBasis`) or `null`. Its result replaces any acceptance in the settings backend: `null`, a throw, or an incomplete record all mean "not accepted", so the gate shows. `requiredVersion` / `isAccepted` then evaluate against the app's receipt, and a receipt for another version re-prompts with `variant: "re-acceptance"`. On agree, `write(record)` is awaited first; if it throws, the gate shows its save-failure alert and nothing else is persisted.

Mapping example for ScrapeRight's four-field `scraperight.legal-acceptance.v1` receipt (`legalAcceptanceVersion`, `legalAcceptedAt`, `legalEligibilityBasis`, `legalMaterialSha256`; pass `documentsHash={LEGAL_MATERIAL_SHA256}` so the hash round-trips):

```ts
const legalReceipt: LegalReceiptPort = {
  read() {
    const r = loadLegalAcceptance(); // reads localStorage "scraperight.legal-acceptance.v1"
    if (!r?.legalAcceptanceVersion || !r.legalAcceptedAt || !r.legalEligibilityBasis) return null;
    return {
      version: r.legalAcceptanceVersion,
      acceptedAt: r.legalAcceptedAt,
      eligibilityBasis: r.legalEligibilityBasis, // "individual" | "enterprise"
      documentsHash: r.legalMaterialSha256 ?? "",
    };
  },
  write(rec) {
    saveLegalAcceptance({
      legalAcceptanceVersion: rec.version,
      legalAcceptedAt: rec.acceptedAt, // UTC ISO, ends in Z
      legalEligibilityBasis: rec.eligibilityBasis,
      legalMaterialSha256: rec.documentsHash,
    });
  },
};
<Onboarding legalReceipt={legalReceipt} requiredVersion={REQUIRED_LEGAL_ACCEPTANCE_VERSION} documentsHash={LEGAL_MATERIAL_SHA256} ... />
```

## Custom steps and readiness

A step may set `canContinue(context)`; Continue/Finish stay disabled while it returns `false` (optional `blockedHint` is shown). Steps with async readiness call `context.setReady(boolean)` from `render` and read `context.ready[stepId]`. Back is disabled on the first step after the agreement.

## Models backend

`ModelsBackend.cancel` is optional; the Pause control appears only when it is supplied.

## Tests

`pnpm exec vitest run` runs the jsdom unit tests (`src/*.test.tsx`) and the headless-Chrome e2e.
