# RightKit ownership approvals

> **Retired for exceptions (Orthic-Labs/rightkit#262).** Ownership exceptions are plain entries in `.rightkit/ownership-exceptions.json`, reviewed through git; signing, drafts and `verifyException` no longer exist. See `OWNERSHIP-ENFORCEMENT.md`. The text below is historical, except the protected trust and key-log sections.

Ownership exception signatures are verified by `packages/release/ownership-approval.mjs`. A signed record binds repository identity, ownership catalog digest, fingerprint, exact repository-relative paths, hashes of each baseline occurrence, reason, owner, expiry, request ID, and a verified HTTPS tracking reference. The verifier rejects unsigned records, wildcards, changed inputs, expired records, revoked keys, and host keys outside protected enrollment.

## Trust and rollout

Trust is read only from `C:\ProgramData\Orthic Labs\RightKit\ownership\trust.json` on Windows or `/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json` on macOS. Windows audits each trust-path ACL, owner, broad write permissions, reparse points, and refuses elevated verifier processes. macOS checks the trust file & each parent for root ownership, write permissions, ACL grants, and symlinks. A failed protection check blocks trust loading. Repository files and environment overrides cannot provide trust.

Both distinct host keys must be enrolled before any exception verifies. Missing enrollment is reported as pending and grants no authority. Trust also carries revoked key IDs, a protected repository registry (`repositories` entries with canonical identity, exact roots, and `public`/`private` visibility), externally verified tracking evidence, and monotonic `minimumScannerVersion` and `minimumCatalogVersion` floors. Never seed example keys or infer enrollment from a machine name.

Protected provenance uses `provenance` entries with canonical repository identity, exact paths, a content digest, `third-party` or `generated` kind, and request ID. `verifyProtectedProvenance` requires every path's bytes and an exact protected entry match; names such as `vendor` or `generated` alone confer no exclusion.

## Draft and sign

Agents may create unsigned drafts with `ownership-approval-command.mjs --draft request.json`. The command has no signing operation. Importing a signature copies the signed record; verifiers still validate it. The intended signing path uses a separately installed native host helper, a full-record prompt, and OS user verification. A CLI argument, TTY, process name, or helper presence does not prove user initiation. Neither host has enrolled approval keys or exercised approval prompts, so records remain pending until that path is established.

The Mac helper built, and its isolated crypto self-test matched 147 canonical UTF-8 bytes & passed Node ECDSA verification using a temporary software key. That self-test did not exercise Secure Enclave, Keychain, enrollment, or approval UX. Windows source conformance passed, but native compilation and Hello/CNG prompt behavior remain unverified because no suitable .NET SDK/targeting packs are installed. No approval keys were enrolled and no record was signed. Install both public keys and protected trust data only through administrator-managed host setup before approvals can verify.

## Scanner integration

Call `verifyException(record, { repositoryIdentity, catalogDigest, findings, trust, today, scannerVersion, catalogVersion })`. Both versions are required for protected floor checks. Repository identities must use canonical HTTPS host/path form; `canonicalRepositoryIdentity` normalizes Git HTTPS, SSH, and scp remotes for draft creation and comparison. The verifier returns `{ ok, problems }`. Load trust using `loadTrustedExternal()`; never accept caller-selected trust paths. Stable record hashing helpers are exported as `canonicalJson`, `sha256`, and `occurrenceHash`. `createDraft` produces unsigned migration requests only. The current scanner integration and the transition to signed-only enforcement are owned by the release CLI/scanner maintainer; do not enable unsigned exceptions as a compatibility fallback.
