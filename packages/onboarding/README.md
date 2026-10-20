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
| `@rightkit/onboarding` | `Onboarding`, `isLegalAccepted`, `onboardingStateKey`, `licenseStep`, `modelsStep`, `permissionsStep`, `signInStep` (Google/Apple/OAuth buttons that call the shared `rightkit-oauth` owner; copy says "Opens your browser"; never an in-app webview, bogusyogi/rightkit#279); native receipt: `createNativeLegalReceiptPort`, `LEGAL_RECEIPT_COMMANDS`, `reopenLegalAcceptance`, `resetLegalAcceptance`; adapters: `licenseBackendFromView`, `licenseStateFromView`, `signInBackendFromOAuth`; types from `types.ts` (including `LegalReceiptPort`, `LegalReceiptCreateRequest`, `DeclaredLegalDocument`), `LicenseStepCopy`, `LicenseStepOptions`, `ModelsStepOptions`, `LicenseViewClient`, `LicenseViewLike`, `LicenseViewBackendOptions`, `OAuthSignInPort`, `LegalReceiptInvoke`, `NativeLegalReceiptPortOptions` |
| `@rightkit/onboarding/onboarding.css` | Stylesheet |

## Peer dependencies

| Peer | Range |
|---|---|
| `@rightkit/react-shell` | `>=0.2.0 <1` |
| `@rightkit/legal-ui` | `>=0.4.1 <1` |
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

`layout` is `"window"` by default (the surface fills the viewport, as in a dedicated onboarding window). `layout="contained"` removes the viewport minimum height so the host sizes the surface; it is passed to the legal gate as well (`@rightkit/legal-ui` `>=0.4.1`).

Acceptance is stored as `legalAcceptance: { version, documentsHash, acceptedAt, eligibilityBasis }`. An app can pass `legalReceipt` and keep its own receipt as the source of truth (`read()` is consulted on load and overrides the settings copy). The receipt is recorded one of two ways:

- **Native (preferred): `create(request)`.** The renderer declares `{ version, documentsHash, documents, eligibilityBasis, surface: "first_run" }` and no time; the native service stamps its own UTC `acceptedAt`, compares the declaration with the manifest it embeds, stores the receipt and returns the record. `receiptDocuments` sets the declared document set (default: the gate's titles and URLs; document bodies are never sent). Build the declaration with `acceptanceDeclaration(manifest)` and the digest with `materialAcceptanceDigestAsync(manifest)` from `@rightkit/legal/browser`. A throw, an invalid record, or a record for another version, digest or eligibility keeps the gate closed with an alert; if only the settings save failed afterwards, a retry reuses the receipt already created instead of stamping a second one.
- **Legacy: `write(record)` (deprecated).** The renderer builds the record, including `acceptedAt` from the webview clock, and the app stores it. It is used only when the port has no `create`. A port with neither cannot accept.

`createNativeLegalReceiptPort({ invoke })` is the port over the host's legal-receipt service (`rightkit_legal_receipt_read`, `rightkit_legal_receipt_create`, `rightkit_legal_receipt_reset`; `LEGAL_RECEIPT_COMMANDS`, override with `commands`). The native side registers those commands once under the host caller policy (`rightkit-tauri-shell` host services, trusted for the window that shows the gate) and stores the receipt under the app root. Command names are provisional until that service is published. A persistence failure, in any path, keeps the gate closed, and a bare legacy `legalAccepted` flag with no record never passes.

`reopenLegalAcceptance({ settings })` makes the gate show again as a re-acceptance on the next mount (the old record stays until the user agrees; progress is kept). `resetLegalAcceptance({ settings, legalReceipt? })` clears the receipt first (`legalReceipt.reset`, required when a port is given) and then the settings copy, so the next mount shows the first-run gate.

```tsx
import { createNativeLegalReceiptPort } from "@rightkit/onboarding";
import { acceptanceDeclaration, materialAcceptanceDigestAsync } from "@rightkit/legal/browser";
import { invoke } from "@tauri-apps/api/core";

const declaration = acceptanceDeclaration(manifest);
<Onboarding
  legalReceipt={createNativeLegalReceiptPort({ invoke })}
  requiredVersion={declaration.acceptanceVersion}
  requiredDocumentsHash={await materialAcceptanceDigestAsync(manifest)}
  receiptDocuments={declaration.documents}
  /* ... */
/>
```

### Licence and sign-in adapters

`licenseBackendFromView(client, options)` turns the native licence client (`createLicenseClient` from `@rightkit/license` 0.3.0, structurally: this package does not import it) into the `LicenseBackend` the licence step takes. The lifecycle decides the status (`grace`, `lifetime` and `verification_required` show as active; `migration_required`, `clock_repair_required`, `revoked` and `invalid` as invalid; anything unknown is invalid, never active). Pass `messageText: licenseMessage` for the detail text, `trialEmail` (where the trial email comes from; a trial is offered only with it and `client.requestTrial`), and optionally `purchaseUrl`, `openExternal`, `labels`. The step displays what the native service reports and decides nothing.

`signIn` adds the built-in sign-in step after the licence step and is handed to every step as `ctx.signIn`. `signInBackendFromOAuth(port, providers)` builds it over the app's `rightkit-oauth` commands (`port.status()` and `port.signIn(provider)`, which call `OAuthClient::sign_in_with_browser` and open the default browser); only `signedIn` and `account` are read from the answers, and a sign-in that resolves without `signedIn: true` rejects so the step shows its retry message. `signInStep(backend)` still works as an app step. Steps can set `canContinue(context)`, `onContinue(context)` (a veto: `false` or a throw keeps the step), `continueLabel`, and `skippable` (a per-step Skip button). `ctx.setResult(value)` stores a value that `onComplete({ completed, skipped, steps })` receives. `labels` overrides the button and status text, and `licenseStep(backend, { copy })` overrides the licence copy.

## When the legal gate re-prompts

`isLegalAccepted(state, requiredVersion?, isAccepted?, requiredDocumentsHash?)` decides it, in this order: a custom `isAccepted` wins outright; a `requiredDocumentsHash` must equal the stored acceptance's `documentsHash`; a `requiredVersion` must equal its `version`; with neither, a recorded acceptance (`legalAcceptance`) is still required, so a bare legacy `legalAccepted` flag re-prompts once as a re-acceptance. Pass `requiredDocumentsHash` (the hash of the material documents, from `@rightkit/legal`) to catch a text change that did not bump the version: without it only the version is compared. `documentsHash` is recorded with the acceptance and never compared; by default it covers document titles and URLs, not their text.

## Known issues

- Peer React is `<21`, the same range as `doc-render`, `i18n`, `media-ui` and `pdf-ui` (`package.json:29`).
