# qa-public-exports-pure

PK14 (rows M20, M21) npm R2a consumer recipe for `@rightkit/qa` at the workspace version `0.5.2` (`packages/qa/package.json`). Source fixture only: no qualification receipt exists until the runner executes it.

Runner owns installation, provenance binding and evidence capture. Copy this directory outside the workspace into a fresh scratch consumer directory. Install the exact packed tarball with the repository-pinned pnpm, with no workspace, Git or link overrides; the only other dependency is the reviewed closure pin `@rightkit/qa-chrome` `0.1.10`. Repeat in a fresh directory against the registry-fetched exact version and retain the fetched tarball. Record installation output, resolved version and integrity, and tarball SHA-256 alongside the consumer result.

Closure: `@rightkit/qa-chrome` `0.1.10` is the reviewed closure pin and the `packages/qa` importer specifier in `pnpm-lock.yaml` (the importer remains a workspace link). Before qualification, verify that this exact version is readable from the registry.

Required environment:

| Name | Value |
| --- | --- |
| `RIGHTKIT_CONSUMER_ARTIFACT` | Absolute installed `.tgz` path |
| `RIGHTKIT_CONSUMER_ARTIFACT_SHA256` | Actual lowercase archive SHA-256 (no candidate digest is pinned; no packed archive exists yet) |
| `RIGHTKIT_CONSUMER_EXPECTED_VERSION` | `0.5.2` |
| `RIGHTKIT_CONSUMER_RESOLUTION` | `packed` before upload; `fetched` after registry readback |

No runtime environment is forwarded; the recipe never reads a browser root. Run `node consumer.mjs` from the consumer directory on macOS and Windows (the default unit hosts; Linux is not required for this unit).

Export surface (every `packages/qa/package.json` export): `.`, `./chrome`, `./ts-hooks`, `./ts-register`.

Scenarios, eight in order, all against installed exports with temp directories removed on exit:

1. `root-contrast-and-focus-verdicts`: `contrastReport`, `ContrastFailure`, `parseCssColor`, `focusRingEvidence`, `boxShadowVisible`.
2. `root-ax-query-assertions`: `matchAxNodes`, `assertAx`.
3. `root-evidence-hashed-receipt`: create, hash, check, finish, verify, write and drift refusal.
4. `root-contract-refusal-and-installed-stage`: `validateLegacyWdioQaContract`, `assertNativeQaContract`, `isInstalledQaMode`.
5. `root-detached-record-round-trip`: `readDetachedRecord` on a temp record file. `stopDevServer` is never called.
6. `chrome-subpath-is-root-reexport-with-pinned-engine`: `./chrome` bindings equal the root's; no browser is launched.
7. `ts-hooks-extensionless-and-json-resolution`: `./ts-hooks` resolve and load hooks on temp files.
8. `ts-register-import-last`: imports `./ts-register`, which registers a process-wide loader hook. It runs last on purpose.

Provenance is unchanged from the reviewed onboarding and qa-chrome recipes (`provenance.mjs` is byte-identical). It checks every archive file against its installed copy and binds each selected export to the archive. Exit 0 requires eight passing scenarios; any import, version, archive, or behavior failure exits 1 with the observed error. One stdout JSON records `recipeId`, `schemaVersion`, `ok`, `host`, `artifact`, `scenarios`, `errors`, and `nativeHostQualified`, which is always `false`.
