# Ownership exception schema v2

> **Retired (bogusyogi/rightkit#262).** Signed schema v2 exception records are no longer read or verified. Exceptions are plain entries `{capability, package, reason, owner, date}` in `.rightkit/ownership-exceptions.json`; see `OWNERSHIP-ENFORCEMENT.md`. This document is kept as history.

Status: contract for P1 of `tasks/reviews/2026-10-08-rightkit-converged-plan.md`. Node, the Windows helper (`OwnershipApproval.cs`) and the macOS helper (`ownership-approval.swift`) must all implement exactly this. Records with schema 1 are rejected once v2 lands. No v1 record has ever been signed, so nothing needs migrating; drafts are regenerated from a fresh scan.

## Envelope

```json
{ "schema": 2, "payload": { ... }, "signature": null | { "host": "windows"|"macos", "keyId": "win-<64hex>"|"mac-<64hex>", "value": "<base64>" } }
```

The signature is computed over `canonicalJson(payload)`: keys sorted, no whitespace, and UTF-8 encoded.

## Payload (exactly these keys)

| Key | Type | Rule |
|---|---|---|
| `repositoryIdentity` | string ≤ 512 | Canonical identity (`canonicalRepositoryIdentity`). |
| `fingerprint` | string | Catalog fingerprint id, for example `browser.npm.puppeteer`. It must match `^[a-z0-9][a-z0-9._-]{1,127}$`. It is **not** hex. |
| `policyDigest` | 64 hex | `sha256(canonicalJson(policyEntry))`. `policyEntry` is the catalog fingerprint object, plus the capability's `capability`, `package` and `tier`. When the catalog changes, only exceptions whose own rule changed are invalidated. |
| `minimumScannerVersion` | dotted numeric | The scanner version the request was drafted with. Verification fails below this floor. |
| `minimumCatalogVersion` | dotted numeric | The catalog version the request was drafted with. Same rule. |
| `occurrences` | array of 1–256 | Each item has exactly `{ "path", "kind", "hash" }`. `path` is an exact repository-relative POSIX path with the same rules as v1 `paths`. `kind` is `"source"` or `"dependency"`. `hash` is 64 hex. The (`path`, `hash`) pairs must be distinct. |
| `reason` | string 1–4000 | |
| `owner` | string 1–256 | |
| `issued` | `YYYY-MM-DD` | Must not be after the verification date. |
| `expires` | `YYYY-MM-DD` | Must satisfy `issued < expires ≤ issued + 90 days`. |
| `requestId` | `^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$` | |
| `trackingRef` | `{ id, url, verified: true }` | HTTPS URL, with no userinfo. |

`catalogDigest`, `paths` and `occurrenceHashes` are removed.

## Occurrence hash v2

`hash = sha256(canonicalJson(identity))`. Display text such as `finding.text`, which is truncated to 160 characters, is **never** an input.

Source finding:
```json
{ "v": 2, "kind": "source", "fingerprint": "<id>", "path": "<rel>", "lineText": "<the full matched line, untrimmed, without its line terminator>", "ordinal": <0-based index of this exact lineText among the matches of this fingerprint in this file> }
```
Line numbers are not part of the identity, so unrelated edits do not invalidate the exception. Editing the matched line does, and so does adding another identical match, which gets a new ordinal.

Dependency finding:
```json
{ "v": 2, "kind": "dependency", "fingerprint": "<id>", "path": "<manifest rel path>", "ecosystem": "cargo"|"npm"|"swift", "name": "<declared name>", "alias": "<alias or null>", "section": "<manifest table/section, e.g. dependencies, dev-dependencies, target.'cfg(windows)'.dependencies, devDependencies>", "requirement": "<version requirement exactly as declared, or null>", "features": ["<sorted, deduplicated declared features>"], "defaultFeatures": true|false|null, "source": "<registry | git+<url>#<rev|branch|tag> | path:<rel>>", "resolvedVersion": "<lockfile-resolved version or null>" }
```

## Verification order (Node is authoritative; the native helpers validate shape only)

Verification checks, in order:
1. The envelope and payload shape.
2. The expiry window against the verification date.
3. The scanner and catalog floors.
4. The `policyDigest` against the installed, authenticated catalog entry.
5. Each `occurrences[i].hash` against a current finding.
6. The signature, against the monotonic key-log trust.

Any failure means the exception does not apply. A finding is covered only when its (`path`, `hash`) pair is listed.

## Conformance vectors

`ownership-approval/conformance/v2-vectors.json` holds production-shaped valid and invalid drafts. The valid ones have dotted fingerprint ids and real-length key ids. The invalid ones cover: a non-hex `fingerprint` accepted, a hex-only `fingerprint`, a 91-day expiry, `issued` in the future, duplicate occurrences, an extra key, and schema 1. Node tests and both native `source-conformance` scripts consume the same file.
