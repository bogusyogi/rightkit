# @rightkit/onboarding

First-run onboarding framework for Right Suite apps. The legal agreement gate comes first, then ordered steps (permissions, model download, license or trial, and app-defined steps), with progress, skip and resume.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/onboarding` | `Onboarding`, `isLegalAccepted`, `onboardingStateKey`, `licenseStep`, `modelsStep`, `permissionsStep`, `signInStep` (Google/Apple/OAuth buttons that call the shared `rightkit-oauth` owner; copy says "Opens your browser"; never an in-app webview, bogusyogi/rightkit#279); types from `types.ts`, `ModelsStepOptions` |
| `@rightkit/onboarding/onboarding.css` | Stylesheet |

## Peer dependencies

| Peer | Range |
|---|---|
| `@rightkit/react-shell` | `>=0.2.0 <1` |
| `@rightkit/legal-ui` | `>=0.1.2 <1` |
| `react` | `>=18 <21` |

## Example

Illustrative. The `settings`, `legal`, `permissions`, `models` and `license` values are app-provided adapters.

```tsx
import { Onboarding } from "@rightkit/onboarding";
import "@rightkit/onboarding/onboarding.css";

<Onboarding
  settings={settings}      // SettingsBackend; progress saved under "onboarding.state"
  legal={legal}
  permissions={permissions}
  models={models}
  license={license}
  onComplete={finish}      // runs only after the state was saved
  onSkip={later}
/>
```

Free or open-source apps without a legal gate pass `legalMode="none"` (no legal step, `legal` not needed, `appName` for the rail) or `legalMode="notice"` (no gate or acceptance; a first Notice step lists `legal.documents` as links). The default is `"gate"`.

Acceptance is stored as `legalAcceptance: { version, documentsHash, acceptedAt, eligibilityBasis }`. An app that already keeps its own acceptance receipt can pass `legalReceipt` (`read()` and `write(record)`) and keep that store as the source of truth. Steps can set `canContinue(context)`, `onContinue(context)` (a veto: `false` or a throw keeps the step), `continueLabel`, and `skippable` (a per-step Skip button). `ctx.setResult(value)` stores a value that `onComplete({ completed, skipped, steps })` receives. `labels` overrides the button and status text, and `licenseStep(backend, { copy })` overrides the licence copy.

## When the legal gate re-prompts

`isLegalAccepted(state, requiredVersion?, isAccepted?, requiredDocumentsHash?)` decides it, in this order: a custom `isAccepted` wins outright; a `requiredDocumentsHash` must equal the stored acceptance's `documentsHash`; a `requiredVersion` must equal its `version`; with neither, a recorded acceptance (`legalAcceptance`) is still required, so a bare legacy `legalAccepted` flag re-prompts once as a re-acceptance. Pass `requiredDocumentsHash` (the hash of the material documents, from `@rightkit/legal`) to catch a text change that did not bump the version: without it only the version is compared. `documentsHash` is recorded with the acceptance and never compared; by default it covers document titles and URLs, not their text.

## Known issues

- Peer React is `<21`, the same range as `doc-render`, `i18n`, `media-ui` and `pdf-ui` (`package.json:29`).
