# @rightkit/legal

Legal manifest, acceptance receipt and reacceptance primitives for proprietary Right Suite desktop apps. Pure and serializable: no UI, no entitlement logic, no network.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling and server-side app code (Node). No UI. |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

Canonical master documents live in `rightkit/legal/`. Each app ships a hash-bound snapshot. `@rightkit/release` verifies those snapshots before packaging.

This package is infrastructure, not legal advice.

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/legal` | `RIGHT_SUITE_LEGAL_SCHEMA` (2), `assertLegalManifest`, `createAcceptanceReceipt`, `needsLegalAcceptance`, `materialAcceptanceDigest`; types `LegalManifest`, `LegalDocumentDescriptor`, `LegalAppendixDescriptor`, `AcceptedLegalDocument`, `LegalAcceptanceReceipt`, `LegalEligibilityBasis`, `LegalAcceptanceSurface`, `LegalDocumentTreatment`, `LegalDocumentRole` |

| `@rightkit/legal` (defaults) | `RIGHT_SUITE_COMPANY_FACTS`, `DEFAULT_DATA_STATEMENT`, `resolveCompanyFacts`, `assertRefundPolicy`, `renderDefaultPolicy`, `renderDefaultPolicies`, `defaultPolicyDescriptors`, `DEFAULT_POLICY_ROLE`; types `CompanyFacts`, `RefundPolicy`, `DefaultPolicyInput`, `RenderedPolicyDocument` |

Peer dependencies: none.

## Shared company facts and default policies

Apps never carry their own company facts. Pass `appName` and `refundPolicy` (required; refunds have no suite default); `supportEmail` defaults to hello@orthiclabs.com and may be overridden. Override a suite fact only where the app truly differs. Adoption steps: `docs/LEGAL-DEFAULTS.md`.

```ts
const input = { appName: "GenRight", refundPolicy: { kind: "none", trialDays: 30 } } as const;
const documents = renderDefaultPolicies(input);
const descriptors = defaultPolicyDescriptors(input, { version: "1", publicUrlBase: "https://example.app/legal/" });
```

## Example

```ts
import { assertLegalManifest, createAcceptanceReceipt, needsLegalAcceptance } from "@rightkit/legal";

const manifest = assertLegalManifest(loadedManifest);
if (needsLegalAcceptance(manifest, storedReceipt)) {
  const receipt = createAcceptanceReceipt(manifest, {
    acceptedAt: new Date().toISOString(),
    surface: "first_run",
    eligibilityBasis: "individual",
  });
  save(receipt);
}
```

## Known issues

- None recorded in this repository.
