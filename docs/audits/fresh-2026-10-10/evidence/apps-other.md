# Fresh RightKit adoption inventory: remaining apps

Scope: current source only, bounded local scan on 2026-10-10. No build, test, install, network, commit, or source edits run. Paths below are absolute. “Keep local” means product semantics stay in app; shared capability should consume published RightKit.

## Version evidence used for comparison

`/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:4-13` is adopted npm baseline, not proof of registry publication. `stagedNpm` at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:15-37` is staged work; `registry.npm` is recorded registry inventory at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:377-400`; this report makes no npm publication claim. Relevant recorded values: baseline `@rightkit/release 0.2.47`; staged `@rightkit/release 0.2.147`, `@rightkit/ax 0.2.4`, `@rightkit/hooks 0.1.2`, `@rightkit/legal 0.3.1`, `@rightkit/app-shell 0.3.0`, `@rightkit/platform-ui 0.2.0`, `@rightkit/qa 0.4.0`.

Swift sections record URL plus adopted exact version `0.1.1` & staged version `0.3.1` at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:321-331`. README’s `0.3.8` example is explicitly illustrative at `/Volumes/D/claude/rightkit/swift/RightKitSwift/README.md:181-190`; it is not verified as published here. Handoffs below use recorded adopted `0.1.1` only after publication readiness confirms tag availability.

## Named products

### Legion — desktop CLI/backend/release tooling

Evidence:

- Exact npm pins: `/Volumes/D/claude/legion/package.json:104-110` has runtime `@rightkit/hooks 0.1.2`; dev `@rightkit/ax 0.2.4`, `@rightkit/git 0.2.33`, `@rightkit/release 0.2.147`.
- External release bridge invokes RightKit at `/Volumes/D/claude/legion/engine/xtask/src/rightkit_release_bridge.rs:66-97`.
- Local duplicate AX behavior is explicit: `/Volumes/D/claude/legion/engine/xtask/src/portable_core.rs:1-7` calls itself Rust port of `@rightkit/ax/plugin/portable-core.mjs`.
- Runtime metadata lags package pin: `/Volumes/D/claude/legion/engine/bins/legion/src/cli.rs:173-177` hard-codes AX `0.2.1` plus source commit, versus package `0.2.4`.

Keep local Legion policy, Rust orchestration, & release composition. Handoff: reconcile hard-coded AX metadata with manifest `0.2.4`; confirm `@rightkit/hooks 0.1.2` is accepted by `/Volumes/D/claude/legion/pnpm-workspace.yaml:2-5` (allowlist contains older hooks); retain bridge calls; do not grow another AX implementation. Desktop-only package additions are not required.

### RustRight — backend/native process broker

Evidence:

- Cargo declarations: `/Volumes/D/claude/rustright/Cargo.toml:22,37-39` has exact `rightkit-ownership = "=0.2.0"`, but caret ranges `rightkit-fs = "0.3.2"` & `rightkit-process = "0.3.1"`, plus explicit Windows feature list. Lock resolves `rightkit-fs 0.3.2` & `rightkit-process 0.3.1` at `/Volumes/D/claude/rustright/Cargo.lock:837-873`.
- Actual ownership imports are used by broker admission at `/Volumes/D/claude/rustright/crates/rustright-broker/src/job_service/assemble.rs:26-30`.
- Product-owned broker/scheduler/cache crates remain local in `/Volumes/D/claude/rustright/Cargo.toml:18-26`.

Keep local broker, scheduler, state, cache, service, install, & parity behavior. Handoff: exact-pin `rightkit-fs` & `rightkit-process` to satisfy RightKit contract, or document approved exception; keep ownership exact; verify package updates against current Windows feature surface. No web, mobile, or desktop UI dependency should be forced.

### SellRight — website/backend + iOS mobile

Evidence:

- Backend exact pin `@rightkit/legal 0.3.1` at `/Volumes/D/claude/sellright/packages/api/package.json:27-31`.
- Actual backend imports are in `/Volumes/D/claude/sellright/packages/api/src/legal/manifests.ts:19-24` & `/Volumes/D/claude/sellright/packages/api/src/legal/acceptance.ts:1-8`.
- iOS uses RightKitSwift by URL but range: `/Volumes/D/claude/sellright/ios/project.yml:16-20` says URL plus `from: 0.1.0`; target consumes `RightKitDiagnostics` at `:27-30`.

Keep storefront, commerce, & legal acceptance adapter local. Handoff: replace mobile range with exact recorded adopted Swift tag `0.1.1` only after publication readiness confirms tag availability; retain only Legal + Swift diagnostics adoption. Classify as web/backend/mobile; do not add desktop UI packages.

### RightSites — website/backend + iOS mobile

Evidence:

- Backend exact `@rightkit/legal 0.3.1` at `/Volumes/D/claude/rightsites/packages/api/package.json:27-32`; source imports mirror SellRight at `/Volumes/D/claude/rightsites/packages/api/src/legal/manifests.ts:19-24`.
- iOS repeats URL range `from: 0.1.0` and `RightKitDiagnostics` at `/Volumes/D/claude/rightsites/ios/project.yml:16-30`.
- Workspace exception list is stale: `/Volumes/D/claude/rightsites/pnpm-workspace.yaml:54-56` lists old release/legal versions, while package consumes legal `0.3.1`.

Keep site catalog, storefront, theming, & acceptance adapter local. Handoff: replace Swift range with exact recorded adopted tag `0.1.1` only after publication readiness confirms tag availability; reconcile/remove stale workspace exceptions; keep Legal + diagnostics only. No desktop dependency.

### PostRight — docs-only checkout, no current app source

Bounded source scan found no `package.json`, `Cargo.toml`, Swift project, or app imports under `/Volumes/D/claude/postright`; only docs plus ignore metadata are present (`/Volumes/D/claude/postright/.gitignore:1-5`). No exact pin or actual import can be established from current source. Keep verdict: adoption unmaterialized; reassess when implementation checkout exists. No desktop, web, backend, or mobile RightKit dependency should be inferred from docs.

### Xtool — native desktop + backend/headless MCP bridge

`/Volumes/D/claude/xtool/package.json:31-54` has no `@rightkit/*` dependency. Xtool is Node-hosted, with local generic infrastructure:

- Logging: `/Volumes/D/claude/xtool/src/main/logger.ts:1-24,63-90` owns an in-memory ring, redacted file mirror, & agent attribution. `@rightkit/logs` is a webview client that batches through injected Tauri `invoke` commands at `/Volumes/D/claude/rightkit/packages/logs/README.md:1-20` & `/Volumes/D/claude/rightkit/packages/logs/src/index.ts:17-45,152-176`. No compatible Xtool Tauri/native command seam is present; keep Xtool logger local.
- Durable state: `/Volumes/D/claude/xtool/src/main/durable.ts:1-18,94-124` atomically renames debounced JSON snapshots. `rightkit-fs` provides Rust descriptor-pinned `StateDir`, `fsync` writes, locks, & owner-only paths at `/Volumes/D/claude/rightkit/crates/rightkit-fs/README.md:3-9,22-31`. API semantics overlap, but Xtool has no Rust host boundary; keep Node durable store local unless native binding work is intentionally opened.
- Secrets: Xtool Keychain calls `/usr/bin/security` & AES-GCM at `/Volumes/D/claude/xtool/src/main/secrets-keychain.ts:1-15,31-80`; Windows DPAPI uses a PowerShell child at `/Volumes/D/claude/xtool/src/main/secrets-dpapi.ts:1-22,40-54`. `rightkit-secrets` exposes Rust `Store`/`Namespace`, macOS Keychain, Windows Credential Manager, an Apple helper, & Linux fail-closed behavior at `/Volumes/D/claude/rightkit/crates/rightkit-secrets/README.md:1-21` & `/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/lib.rs:1-34`. Xtool’s Node codecs, legacy service identity, & cross-platform fallback do not share this API; keep local, with migration requiring native bridge plus data migration.
- Process execution: Xtool shell-free `runCommand`/`launchCommand` uses Node `spawn`, explicit env filtering, timeouts, & detached POSIX groups at `/Volumes/D/claude/xtool/src/main/exec.ts:1-10,67-74,545-591`. `rightkit-process` owns Rust child-tree lifetime, restart tracking, health thresholds, Unix groups, & Windows Job Objects at `/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:3-20,24-31`. This is partial lifecycle overlap; Xtool’s Node process policy & command protocol remain product-local, with no direct package substitution.
- Filesystem sandbox: `/Volumes/D/claude/xtool/src/main/sandbox.ts:1-17,171-197,291-312` canonicalizes approved roots, rejects links escaping roots, & maps virtual paths. `rightkit-fs` covers descriptor-relative state/locks rather than Xtool’s model-facing virtual-root protocol at `/Volumes/D/claude/rightkit/crates/rightkit-fs/README.md:3-9,33-38`; keep Xtool sandbox local.
- Desktop helper: Windows UIA/SendInput is embedded PowerShell in `/Volumes/D/claude/xtool/src/main/computer/helper.ts:1-31,63-83`; Node selects Windows PowerShell or macOS Swift backend over one JSON protocol at `/Volumes/D/claude/xtool/src/main/computer/index.ts:173-179,321-341`, with Swift helper sources importing AppKit/ApplicationServices/ScreenCaptureKit at `/Volumes/D/claude/xtool/native/macos-desktop-helper/main.swift:1-10`. `rightkit-automation` is an in-process Rust AX/UIA ladder with policy, leases, receipts, & no Node runtime at `/Volumes/D/claude/rightkit/crates/rightkit-automation/README.md:1-25,27-36`. This is substantial native-helper overlap; possible binding/migration is larger project work, not required for current adoption.
- RightKitSwift: package offers Apple targets such as Diagnostics, AppleCredentials, UI, Store, & Intents at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:4-24`; Xtool has no Swift package dependency or `RightKitSwift` import. Its macOS helper is standalone AppKit/ScreenCaptureKit source, so no direct Swift target compatibility is evidenced. Keep helper local; reassess only if Xtool intentionally adopts an Apple package boundary.

Keep Xtool MCP, PTY, session, connector, permission, & native-helper protocol semantics local. Handoff checklist: no forced `@rightkit/logs`, Rust fs/process/secrets/automation, or RightKitSwift dependency; consider only targeted native-bridge projects after API/packaging decisions. Classify Xtool as native desktop + backend/headless infrastructure, not website or mobile.

## Additional first-party checkouts

### Membrane — Tauri desktop, three duplicate worktrees

Canonical `/Volumes/D/claude/membrane` is absent. These are linked worktree copies, not three products: `/Volumes/D/worktrees/membrane-main-f602-baseline`, `/Volumes/D/worktrees/membrane-blueprint-findings`, `/Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair`; root, hub manifest, & lock hashes match across all three (`96218bc…`, `126b6e…`, `fea592…`).

Evidence from baseline (same content in copies):

- Root pins `@rightkit/hooks 0.1.0`, `@rightkit/ax 0.1.1`, `@rightkit/git 0.2.0` at `/Volumes/D/worktrees/membrane-main-f602-baseline/package.json:29-35`.
- Hub pins `@rightkit/platform-ui 0.1.0` & `@rightkit/release 0.2.67` at `/Volumes/D/worktrees/membrane-main-f602-baseline/apps/membrane-hub/package.json:16-23`.
- Actual platform UI import is `/Volumes/D/worktrees/membrane-main-f602-baseline/apps/membrane-hub/src/popover.mjs:87`; release bridge imports `@rightkit/release/notary-auth.mjs` at `/Volumes/D/worktrees/membrane-main-f602-baseline/apps/membrane-hub/scripts/build-mac-release.mjs:17`.

Keep Membrane engine, Blueprint/Cortex, hub semantics, & Tauri wiring local. Handoff: restore/select one canonical checkout before change; move hooks/AX/git/platform-ui/release to verified package pins only after publication proof & compatibility review. No RightKit Rust UI/process dependency is evidenced; do not force one.

### ScreenRight — archived desktop checkout only

Only `/Volumes/D/claude/archive/screenright-2026-10-08` exists; no current canonical checkout. Evidence:

- Archived manifest pins `@rightkit/release 0.2.66` at `/Volumes/D/claude/archive/screenright-2026-10-08/package.json:32-34`.
- Current archive test expects release `0.2.61`, conflicting with manifest, at `/Volumes/D/claude/archive/screenright-2026-10-08/test/release-pipeline.test.mjs:9-15`.
- Local diagnostics duplicate RightKit-like behavior at `/Volumes/D/claude/archive/screenright-2026-10-08/menu/RightKitDiagnostics.swift:3-7`; controller calls it at `:382,532,539,555`.
- Archive test explicitly rejects RightKit QA Cargo dependency at `/Volumes/D/claude/archive/screenright-2026-10-08/test/boundary.test.mjs:13`; `src-tauri/Cargo.toml:1-5` has no RightKit crate.

Keep archive untouched. If resurrected, handoff: reconcile release version/test drift; migrate local diagnostics to RightKit logs/Swift diagnostics; use exact published release policy. No current app adoption action.

### LightCraft — native/browser Rust workflow app

`/Volumes/D/claude/workflows/lightcraft/README.md:1-8` describes pure-Rust photo/raw tooling for macOS, Windows, Linux, & browser WASM. Bounded source search found no RightKit manifest or import. Keep image/raw pipeline local; evaluate release/QA only when shipping integration exists. Browser target stays separate from desktop packages.

### FreeCAD — third-party fork/reference checkout

`/Volumes/D/claude/workflows/freecad/README.md:1-8` identifies FreeCAD fork/upstream 3D modeler. No RightKit manifest/import found. Classify as third-party fork/reference, exclude from Right Suite adoption universe, keep local.

## Discovery, duplicates, exclusions

- First-party roots inspected: named `/Volumes/D/claude/{legion,rustright,sellright,rightsites,postright,xtool}`, plus `/Volumes/D/worktrees/membrane-*`, `/Volumes/D/claude/archive/screenright-2026-10-08`, `/Volumes/D/claude/workflows/{lightcraft,freecad}`. Pulse is covered by separate apps-new evidence.
- Membrane worktrees are duplicate content; absence of canonical `/Volumes/D/claude/membrane` blocks canonical handoff.
- Citadel source checkout was not found. Only asset `/Volumes/D/claude/orthic-labs-hero-art/citadel/hero-illustrate-v1.png`, stale submodule metadata `/Volumes/D/claude/.git/modules/citadel`, & docs-only plan directory exist; no adoption evidence.
- ScreenRight is archive-only. Duplicate SwiftRight/build artifacts under `/Volumes/D/sr0-build` are not independent products; use source checkout identity from machine inventory, not artifact copies.
- Excluded bounded matches: `/Volumes/D/claude/repos/membrane/*` research/reference clones; `/Volumes/D/claude/apple-skill-sources/*` third-party skill sources; `/Volumes/D/claude/vendor/*`; caches, `node_modules`, `target`, `dist`, `build`, generated reports, & docs-only plans. These are not app adoption evidence.
