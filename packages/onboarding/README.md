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
| `@rightkit/onboarding` | `Onboarding`, `isLegalAccepted`, `onboardingStateKey`, `licenseStep`, `modelsStep`, `permissionsStep`; types from `types.ts`, `ModelsStepOptions` |
| `@rightkit/onboarding/onboarding.css` | Stylesheet |

## Peer dependencies

| Peer | Range |
|---|---|
| `@rightkit/app-shell` | `>=0.2.0 <1` |
| `@rightkit/legal-ui` | `>=0.1.2 <1` |
| `react` | `>=18 <20` |

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

Acceptance is stored as `legalAcceptance: { version, documentsHash, acceptedAt, eligibilityBasis }`. An app that already keeps its own acceptance receipt can pass `legalReceipt` (`read()` and `write(record)`) and keep that store as the source of truth. Steps can set `canContinue(context)`.

## Known issues

- Peer React is `<20`, while `doc-render`, `i18n`, `media-ui` and `pdf-ui` accept `<21` (npm audit 2026-10-08).
