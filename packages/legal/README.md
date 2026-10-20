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
| `@rightkit/legal` | `RIGHT_SUITE_LEGAL_SCHEMA` (2), `assertLegalManifest`, `createAcceptanceReceipt`, `acceptanceDeclaration`, `needsLegalAcceptance`, `materialAcceptanceDigest`; types `LegalManifest`, `LegalDocumentDescriptor`, `LegalAppendixDescriptor`, `AcceptedLegalDocument`, `LegalAcceptanceDeclaration`, `LegalAcceptanceReceipt`, `LegalEligibilityBasis`, `LegalAcceptanceSurface`, `LegalDocumentTreatment`, `LegalDocumentRole` |

| `@rightkit/legal` (defaults) | `RIGHT_SUITE_COMPANY_FACTS`, `SUITE_REFUND_POLICY`, `DEFAULT_DATA_STATEMENT`, `resolveCompanyFacts`, `assertRefundPolicy`, `effectiveRefundPolicy`, `renderDefaultPolicy`, `renderDefaultPolicies`, `defaultPolicyDescriptors`, `DEFAULT_POLICY_ROLE`; types `CompanyFacts`, `RefundPolicy`, `DefaultPolicyInput`, `RenderedPolicyDocument` |

Peer dependencies: none.

## Shared company facts and default policies

Apps never carry their own company facts. Pass `appName`. `refundPolicy` defaults to the suite 14-day window (`SUITE_REFUND_POLICY`); an app overrides it with `{ kind: "window", days }` or `{ kind: "none", trialDays? }`. `supportEmail` defaults to hello@orthiclabs.com and may be overridden. Override a suite fact only where the app truly differs. Adoption steps: `docs/LEGAL-DEFAULTS.md`.

```ts
const input = { appName: "GenRight", refundPolicy: { kind: "none", trialDays: 30 } } as const;
const suiteDefault = { appName: "HeardRight" }; // 14-day refund window
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

## Native receipt creation

A webview must not stamp its own acceptance time. `acceptanceDeclaration(manifest)` (also on `@rightkit/legal/browser`) returns what the renderer declares to a native receipt service: schema, suite, `appKey`, `acceptanceVersion` and the document set (`id`, `role`, `version`, `sha256`, `treatment`, `material`), with no `acceptedAt` and no eligibility. `@rightkit/onboarding` sends it (with `materialAcceptanceDigestAsync(manifest)` as the declared digest) through `LegalReceiptPort.create`; the native service compares both with the manifest it embeds, stamps its own UTC time and stores the receipt. `createAcceptanceReceipt` remains the JS builder for hosts without a native service.

```ts
import { acceptanceDeclaration, materialAcceptanceDigestAsync } from "@rightkit/legal/browser";
const declaration = acceptanceDeclaration(manifest);
const digest = await materialAcceptanceDigestAsync(manifest);
// <Onboarding legalReceipt={nativePort} requiredVersion={declaration.acceptanceVersion}
//   requiredDocumentsHash={digest} receiptDocuments={declaration.documents} ... />
```

## Known issues

- None recorded in this repository.

## OSS profile and browser entry

Set `profile: "oss"` on the manifest for a free or open-source app: only `privacy_notice` and `third_party_notices` are required. Webviews import from `@rightkit/legal/browser`, which has no `node:crypto` and hashes with WebCrypto (`sha256HexAsync`, `materialAcceptanceDigestAsync`).

## Snapshot & npm notice tooling (0.6.0)

Node-only entry `@rightkit/legal/tooling` exports `syncLegalSnapshot(config, appRoot?)`,
`assertFinalLegalSnapshot(config, appRoot?)` & `generateNpmLicenseAppendix(options?)`.
`appRoot` defaults to cwd. Types: `LegalSnapshotConfig`, `SnapshotDocument`,
`SnapshotAppendix`, `NpmLicenseAppendixOptions`, `NpmLicenseGroups`, `NpmLicenseEntry`.

Apps keep document metadata in `legal-snapshot.config.json`. Example for an OSS app:

```json
{
  "appKey": "example",
  "productName": "Example",
  "licensor": "Example Ltd",
  "profile": "oss",
  "effectiveDate": "2026-10-17",
  "acceptanceVersion": "example-v1",
  "draftAcceptanceVersion": "example-draft-v1",
  "sourceRoot": "./approved-legal-masters",
  "documents": [
    {
      "id": "privacy", "role": "privacy_notice", "title": "Privacy", "version": "1",
      "path": "PRIVACY.md", "source": "PRIVACY.md",
      "publicUrl": "https://example.com/legal/privacy/", "treatment": "acknowledge", "material": false
    },
    {
      "id": "notices", "role": "third_party_notices", "title": "Notices", "version": "1",
      "path": "NOTICES.md", "publicUrl": "https://example.com/legal/notices/",
      "treatment": "notice", "material": false
    }
  ],
  "appendices": [{ "id": "npm", "title": "npm licenses", "path": "THIRD_PARTY_NPM_LICENSES.html" }],
  "notices": {
    "template": "NOTICES.template.md", "path": "NOTICES.md",
    "hashes": { "lock": "pnpm-lock.yaml", "npmAppendix": "legal/THIRD_PARTY_NPM_LICENSES.html" }
  }
}
```

Proprietary apps omit `profile` & supply all six required roles. `documents` contain manifest
fields except `sha256`, plus optional `source` relative to `sourceRoot`. Without `source`,
sync reads existing app legal files; configured notices output is rendered from template.
`sourceRoot` is absolute or relative to app root; only explicit sync reads masters.
`appendices` contain manifest fields except `sha256`; generate them before syncing.
`legalDir` defaults to `legal`; `contractPath` defaults to `src/generated/legal-contract.json`.
Document, appendix, template & output paths must be relative without traversal.
`notices.hashes` maps placeholder names to input paths relative to app root (absolute paths &
parent paths allowed for lockfiles outside app root). Templates also receive `version` from
app package.json & configured `effectiveDate`; unknown placeholders fail before writes.

```sh
pnpm exec generate-npm-license-appendix --product-name Example
pnpm exec rightkit-legal npm-licenses --product-name Example
pnpm exec legal-snapshot
pnpm exec legal-snapshot --final
# Explicit roots/configs, independent of cwd:
pnpm exec legal-snapshot --root /path/to/app --config legal-snapshot.config.json --source-root /path/to/masters
```

`rightkit-legal npm-licenses` takes the same options as `generate-npm-license-appendix` and delegates to it.
`rightkit-legal rust-licenses [--root APP] [--config right-release.config.mjs] [--output FILE] [--check]` has one
source: `right-release license-notices cargo` (`cargo-license-notices.mjs` in `@rightkit/release`, which runs
`cargo about generate --locked --fail` through the managed cargo client). This package keeps no licence metadata and no
cargo logic: it finds `@rightkit/release` installed for the app (or the entry file given with `--right-release FILE`),
forwards the options and returns the route's exit status. The app declares `legal.cargoNotices` (`output`, `template`,
optional `config`, `manifestPath`, `features`, `allFeatures`) in `right-release.config.mjs`; `--check` writes nothing and
fails when the file on disk differs. It needs `@rightkit/release` with the `license-notices cargo` route (PK17-5); without
it the command reports what is missing and exits nonzero. Add the output as an appendix in `legal-snapshot.config.json`
and run `legal-snapshot` so its hash binds into the manifest.

All bins accept `--help` & exit nonzero on failure.

Containment: every snapshot and appendix path is checked after resolving symlinks. Reads and writes that resolve
outside their declared root (`legal/` for documents and manifest, the app root for the contract and npm output,
`sourceRoot` for masters) are refused, and so is any symlinked ancestor that points outside. Hash inputs
(`notices.hashes`) and the npm `paths` reported by pnpm are read as declared. `--config` resolves relative to app root.
Sync returns `{ manifest, contract }`, creates output directories, atomically replaces each file &
selects `draftAcceptanceVersion` when any document contains `DRAFT: pending` or `[TBD`.
Commit outputs. `--final` writes nothing, needs no masters & rejects draft markers in documents
or appendices, stale descriptors, hashes or gate contract. Material approvals & version changes
remain explicit app metadata; sync never invents approval or increments acceptance versions.

Npm generation runs `pnpm licenses list --prod --json` in app root. Options: `appRoot`,
`productName` (defaults to package `productName` then `name`), relative `output` (defaults to
`legal/THIRD_PARTY_NPM_LICENSES.html`) & optional `groups` to use captured pnpm JSON.
CLI equivalents: `--root`, `--product-name`, `--output`, `--licenses-json FILE`.
Output sorts packages, excludes `@rightkit/*`, reads LICENSE/LICENCE/COPYING/NOTICE files
from every distinct installed instance, escapes HTML & includes app version / lockfile SHA-256.
Absent packaged texts use explicitly labeled standard MIT/ISC boilerplate; other missing texts fail.
Returns `{ output, packageCount, fallbackCount }`; fallback count counts installed instances.

```ts
import { syncLegalSnapshot, assertFinalLegalSnapshot, generateNpmLicenseAppendix } from "@rightkit/legal/tooling";
generateNpmLicenseAppendix({ appRoot, productName: "Example" });
const { manifest, contract } = syncLegalSnapshot(config, appRoot);
assertFinalLegalSnapshot(config, appRoot);
```

Run `pnpm run test:tooling` to build & run Node unit/CLI integration tests; `pnpm test` also runs
existing Vitest tests. Tooling stays outside browser & default entry points.
