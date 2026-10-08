# @rightkit/legal

Legal manifest, acceptance receipt and reacceptance primitives for proprietary Right Suite desktop apps. Pure and serializable: no UI, no entitlement logic, no network.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling and server-side app code (Node). No UI. |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

Canonical master documents live in `tools/rightkit/legal/`. Each app ships a hash-bound snapshot. `@rightkit/release` verifies those snapshots before packaging.

This package is infrastructure, not legal advice.

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/legal` | `RIGHT_SUITE_LEGAL_SCHEMA` (2), `assertLegalManifest`, `createAcceptanceReceipt`, `needsLegalAcceptance`, `materialAcceptanceDigest`; types `LegalManifest`, `LegalDocumentDescriptor`, `LegalAppendixDescriptor`, `AcceptedLegalDocument`, `LegalAcceptanceReceipt`, `LegalEligibilityBasis`, `LegalAcceptanceSurface`, `LegalDocumentTreatment`, `LegalDocumentRole` |

Peer dependencies: none.

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

- None recorded in the npm audit (2026-10-08) beyond consumer version drift.
