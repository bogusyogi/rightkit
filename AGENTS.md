<!-- GENERATED FILE. Do not hand-edit. Source: rightkit/docs/agent-rules.md. Regenerate: py -3.11 tools/agent-rules/manage.py sync (Windows) or python3 tools/agent-rules/manage.py sync (Mac). -->
# RightKit Rules

## Purpose
RightKit owns shared packages, crates, QA, licensing, logging, & release tooling; apps consume published versions, never parent paths.

## Canonical sources
- Read `rightkit/README.md` for package contracts.
- Read `docs/RIGHTKIT-SHARED-PACKAGES.md` before package boundaries change.
- Read `docs/RIGHT-SUITE-RELEASE-PIPELINE.md` before release tooling changes.
- Read `docs/RIGHT-SUITE-UPDATE-TIER-ROUTING.md` before tier changes.
- Read signing, distribution, & licensing runbooks before publication.
- Read `docs/RUNTIME-BUDGETS.md` before publishing or adopting any crate that runs continuously; declare, measure & expose its budget.

## Commands
- Run `pnpm --dir rightkit test:release` for release package changes.
- Run `pnpm --dir rightkit doctor:release` for broad diagnostics.
- Run `pnpm --dir rightkit test:qa` for QA package changes.
- Run `node --test rightkit/packages/release/right-suite-contract.test.mjs` for app parity.
- Run the focused package build before publishing any package.

## Locked invariants
- Keep shared source only under `rightkit/`.
- Make apps consume published `@rightkit/*` without Git, link, file, or workspace specs.
- Pin RightKitSwift by repository URL & version tag, never local path.
- Run every release from the application primary checkout.
- Keep `right-release build` upload-free & `right-release upload` build-free.
- Require explicit patch or update tier for upload.
- Require explicit `distribution.provider`: public products use `github-releases`; private products use `r2`.
- Keep add-on adoption bound to version, full manifest SHA-256, immutable URL, and target-qualified aliases.
- Never run browser login, `az login`, Azure enrollment, or signing setup; use native-host provisioned signers.
- Publish Cargo crates only through the RightKit release command.
- Upload no unsigned or test artifact.
- Activate enforcement policy & verifier changes only through Adrian-authenticated trust-sync; see `docs/OWNERSHIP-ENFORCEMENT.md`.
- Apply identical qualification requirements across distribution providers; see `../../docs/rules/release-signing.md`.

## Verification
- Run focused package tests before the release contract suite.
- Verify versions & standalone-install portability across all five desktop apps.
- Compare every GitHub Release asset with sealed hashes after an explicitly requested publication.
