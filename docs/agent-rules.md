# RightKit Rules

## Purpose
RightKit owns shared packages, crates, QA, licensing, logging, & release tooling; apps consume published versions, never parent paths.

## Canonical sources
- Read `docs/PIPELINE-ARCHITECTURE.md` before changing any check, build, qualification or release path; keep its section 5 green.
- Read `rightkit/README.md` for package contracts.
- Read `docs/RIGHTKIT-SHARED-PACKAGES.md` before package boundaries change.
- Read `docs/RIGHT-SUITE-RELEASE-PIPELINE.md` before release tooling changes.
- Read `docs/RIGHT-SUITE-UPDATE-TIER-ROUTING.md` before tier changes.
- Read signing, distribution, & licensing runbooks before publication.

## Commands
- Compile & test locally through the gate (Mac & Dell via the broker, Linux direct); CI qualifies every push; see `docs/CI.md`.
- Run `node scripts/preflight.mjs` before every push, hook or no hook: seconds, files only. Fix every `FAIL` it prints, commit, & rerun until it passes; CI's first step runs the same check.
- Run `node scripts/ci/gate.mjs --plan --base <sha>` to preview what a push will verify.
- Push to `main` in batches & read the `ci` run; apply its `fmt.patch` or `Cargo.lock.patch`, never hand-format.
- Run `rustfmt --edition <edition> --check <file>` on each changed Rust file before pushing.
- Record a red unit in `ci/known-red.json` with its reason; never publish a unit listed there.
- RightKit publishes automatically from git through CI after CI qualification; never from the Mac or Dell; see `docs/CI.md` "Publishing".

## Locked invariants
- Keep shared source only under `rightkit/`.
- Make apps consume published `@rightkit/*` without Git, link, file, or workspace specs.
- Pin RightKitSwift by repository URL & version tag, never local path.
- Run every release from a selected commit through the gate.
- Keep `right-release build` upload-free & `right-release upload` build-free.
- Require explicit patch or update tier for upload (win and mac); the Android Play internal-track upload takes no tier.
- Android: `right-release build --platform android` makes the signed AAB (upload key from the credential store, passwords only by transient file path); `right-release upload --platform android --release <id>` is dry-run unless `--apply`, build-free, and refuses a debug-signed or unpinned AAB; provider `google-play`, internal track only; never put the keystore, its passwords or the Play service-account JSON in the repo, environment, argv or logs.
- Require explicit `distribution.provider`: public products use `github-releases`; private products use `r2`.
- Keep add-on adoption bound to version, full manifest SHA-256, immutable URL, and target-qualified aliases.
- Never run browser login, `az login`, Azure enrollment, or signing setup; use native-host provisioned signers.
- Upload no unsigned or test artifact.
- Activate enforcement policy & verifier changes only through Adrian-authenticated trust-sync; see `docs/OWNERSHIP-ENFORCEMENT.md`.
- Apply identical qualification requirements across distribution providers; see `../../docs/rules/release-signing.md`.

## Verification
- Run focused package tests before the release contract suite.
- Verify versions & standalone-install portability across all five desktop apps.
- Compare every GitHub Release asset with sealed hashes after an explicitly requested publication.
