# RightKit Rules

## Purpose
RightKit owns shared Right Suite packages, crates, QA helpers, licensing, logging, and release tooling.
Application repositories consume published package versions rather than parent-workspace paths.

## Canonical sources
- Read `tools/rightkit/README.md` for package contracts.
- Read `docs/RIGHTKIT-SHARED-PACKAGES.md` before package-boundary changes.
- Read `docs/RIGHT-SUITE-RELEASE-PIPELINE.md` before release tooling changes.
- Read `docs/RIGHT-SUITE-UPDATE-TIER-ROUTING.md` before tier changes.
- Read signing, distribution, and licensing runbooks before publication work.

## Commands
- Run `pnpm --dir tools/rightkit test:release` for release package changes.
- Run `pnpm --dir tools/rightkit doctor:release` for broad release diagnostics.
- Run `pnpm --dir tools/rightkit test:qa` for QA package changes.
- Run `node --test tools/rightkit/packages/release/right-suite-contract.test.mjs` for app contract parity.
- Run the focused package build before publishing any package.

## Locked invariants
- Keep shared source only under `tools/rightkit/`.
- Make apps consume published `@rightkit/*` versions without Git, link, file, or workspace specifications.
- Keep RightKitSwift pinned by repository URL and version tag rather than local path.
- Run every release from the application primary checkout.
- Keep `right-release build` unable to upload and `right-release upload` unable to build.
- Require explicit patch or update tier for upload.
- Keep R2 replace-only and signatures in manifests.
- Publish Cargo crates only through the RightKit release command.
- Upload no unsigned or test artifact.

## Verification
- Run focused package tests before the release contract suite.
- Verify package versions and standalone-install portability across all five desktop apps.
- Compare sealed artifacts with uploaded stable objects after an explicitly requested publication.
