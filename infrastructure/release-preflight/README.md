# release-preflight

Executable preflight for the Orthic-Labs `release-candidate.yml` pipeline (products: `legion`,
`membrane`). Read-only — no dispatch, no push, no repo mutation. Node 22 ESM, no dependencies
beyond node builtins and the `gh` CLI (must already be authenticated; see
`docs/rules/github-access.md`).

## Usage

```
node preflight.mjs <legion|membrane> <version> <sha> [--checkout <path>] [--json]
```

Prints one line per check (`PASS|FAIL|WARN <id> <detail>`), then a final
`PREFLIGHT PASS` or `PREFLIGHT FAIL (<n> failures)` line, exit code 0/1.
`--json` emits a single JSON object instead.

## Checks

Each check id is derived from a real 2026-09-02 failure; see the code comments in `preflight.mjs`
for the cited run id.

1. `sha-format` — 40 lowercase hex characters.
2. `sha-is-branch-head` — sha equals `main`'s current HEAD.
3. `local-checkout-matches` — local checkout HEAD equals sha; WARN if dirty.
4. `ci-green-at-sha` — a `*-ci` workflow run at sha is `completed`/`success`.
5. `no-existing-release` — `v<version>` does not already exist.
6. `version-matches-source` — the checkout's source-of-truth version file matches `<version>`.
7. `release-env-allows-main` — the `release` environment's deployment branch policy allows `main`.
8. `release-env-secrets` — every `secrets.X`/non-defaulted `vars.X` referenced in the workflow
   exists in the `release` environment (or repo variables for vars).
9. `protected-jobs-fetch-depth` — every job with `environment: release` and a checkout pinned to
   `needs.admission.outputs.source_revision` also sets `fetch-depth: 0`.
10. `manifest-digests` — `scripts/ci/check-runtime-language-manifest.mjs`, if present, exits 0.
11. `git-hooks-resolve` — every path referenced from a local git hook actually exists.
12. `workflow-scope-token` — WARN when the diff between remote main and sha touches
    `.github/workflows/` and the platform is win32 (Windows `GH_TOKEN` lacks `workflow` scope).
13. `dry-run-receipt` — a completed, successful `release-candidate` run within the last 7 days
    carries a `-dry-run` artifact and was built from the exact `.github/workflows/release-candidate.yml`
    blob at `<sha>` (rule 3: the first real dispatch is never the first execution).
14. `local-ci-command` — every `node`/`pnpm test` line in `scripts/ci/run-ci.sh` exits 0 locally with
    `RIGHT_RELEASE_OFFLINE=1` (rule 1: one CI command, identical everywhere). PASS-skipped when the
    checkout has no `scripts/ci/run-ci.sh`.

## Tests

```
node --test preflight.test.mjs
```

Unit tests cover the pure parsers only (`extractSecretsAndVars`, `splitJobBlocks`,
`analyzeJobForFetchDepth`, `extractHookPathReferences`, `touchesWorkflows`, `parseCiCommandLines`)
against inline fixtures. They never call `gh` or touch a real checkout.
