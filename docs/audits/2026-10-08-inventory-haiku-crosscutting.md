The report is below. I did not write it to `docs/audits/2026-10-08-inventory-haiku-crosscutting.md`, because this agent's rules forbid writing report files and require findings in the final message. Save it from here if you need the file.

# RightKit inventory: private packages, Swift, ownership-approval, contracts, scripts, and cross-cutting G-K

## 0. Scope, method, caveats

- Read-only. No edits, builds, cargo commands, commits, or pushes. Rust was not built or tested. Rust facts come from manifests and `grep`.
- Snapshot caveat: the tree changed during the audit. `crates/` now has 50 directories, and `rightkit-contracts` plus `contracts/` appeared as untracked (`git status` in `/Volumes/D/claude` shows `?? tools/rightkit/contracts/` and `?? tools/rightkit/crates/rightkit-contracts/`).
- Consumer evidence: every `Cargo.toml` and `package.json` under the 14 app repos and `tools/rightkit` was parsed for `rightkit-*` and `@rightkit/*` dependencies (`scratchpad/inv/consumers.py`). A text-level `grep` pass over app source was started but did not finish, so "zero consumers" below means zero manifest-level consumers.
- Private npm packages are `private: true`: `agent-room-mcp` and six `build-control-*` packages. Public npm packages are covered by other batches except where G-K need them.
- Transitive counts are from `pnpm-lock.yaml` where stated. Rust transitive counts are not measured here (batch 4 owns Cargo.lock).

---

## 1. Private npm packages (A-F)

### 1.1 `@rightkit/build-control-core` (0.1.0-phase1)
- **A.** Path `packages/build-control-core`. Private. Node `>=24` (`package.json` engines). Platforms: macOS and Windows (path roots in `storage.mjs:12`).
- **B.** Protocol constants, profiles, request normalization, repository execution policy, storage governance, Xcode device-support pruning, local-development policy. Main exports: `FRONTENDS` (`index.mjs:7`), `BUILD_CONTROL_PROTOCOL`, `SOURCE_RUNTIME_IDENTITY`, `PROFILES`, `repositoryExecutionPolicy`, `assertRepositoryExecutionAllowed`. `storage.mjs` exports 18 symbols including `withStorageLock` (`:44`) and `pruneManaged`. `xcode-storage.mjs` exports 6.
- **C.** Consumers: `@rightkit/build-control-broker` (11 non-test files, named imports such as `broker.mjs:8`, `runtime.mjs:9`). `@rightkit/build-control-clients` via relative paths (`storage-cli.mjs:4-5`). `@rightkit/build-control-policy` via relative path (`index.mjs:4`). No app-repo consumers.
- **D.** No external dependencies. Node builtins only (`fs`, `fs/promises`, `path`, `os`, `child_process`, `crypto`, `util`, `url`).
- **E.** External npm closure: 0 packages.
- **F.**
  - `storage.mjs:12` hard-codes `/Volumes/D/.rightkit-managed` and `D:\\`, a machine-specific fallback.
  - `index.mjs` is 553 lines and mixes protocol constants, policy, and request normalization.

### 1.2 `@rightkit/build-control-clients` (0.1.0-phase1)
- **A.** Path `packages/build-control-clients`. Private. Node `>=24`. Bins: `rightkit`, `rightkit-managed-agent`, `rightkit-install-agent-shims`.
- **B.** Client request protocol and CLI frontend (`index.mjs`, 490 lines). Notable functions: `createClientRequest`, `createJobCommand`, `compileNativeWindowsLauncher` (`:420`), `writeAtomic` (`:393`). Remote-build transport (`remote-*.mjs`, about 1,000 lines): snapshot via private Git index, tar archive guard, worker, Windows slot mutex (`remote-worker.mjs:65`). Ownership lookup and catalog sync. Storage CLI.
- **C.** Consumers: `build-control-broker` (`cutover-shared.mjs:6`, dependency and devDependency). `build-control-policy` test only (`ownership-write-hook.test.mjs:8`). Runtime consumers are the installed shims and `scripts/install-right-ctl.sh`, which depend on the `rightkit` bin being on PATH.
- **D.**
  - `@rightkit/build-control-core` (declared runtime dependency). Used only in tests by package name (`index.test.mjs`). Runtime code uses relative paths instead (`storage-cli.mjs:4-5`). Verdict: TRIM / fix. Either declare and import by name, or drop the declaration.
  - Undeclared relative imports into the public `@rightkit/release` package: `index.mjs:1` (`managed-roots.mjs`), `ownership-catalog-sync.mjs` (`ownership-catalog.mjs`, `rightkit-sync.mjs`). Verdict: NEEDED, but must be declared.
  - No external npm dependencies.
- **E.** External closure: 0 packages.
- **F.**
  - `writeAtomic` (`index.mjs:393`) has no fsync and no Windows rename retry (compare `thin-state.mjs:43`).
  - Windows native launcher source (`native-windows-launcher/ManagedRustLauncher.cs`) is compiled on demand by `compileNativeWindowsLauncher` (`index.mjs:420`). It is not orphaned.
  - Ownership catalog and versions JSON are generated copies (see G22).

### 1.3 `@rightkit/build-control-broker` (0.1.0-phase1)
- **A.** Path `packages/build-control-broker`. Private. Node `>=24`. Platforms: macOS (launchd, `bootstrap-macos-cli.mjs`, `runtime-cutover-macos*.mjs`, `native/macos-resource`) and Windows (scheduled task, `bootstrap-windows-cli.mjs`, `native/windows-job`). Eight bins in `package.json`.
- **B.** Local build broker. Scheduling, state, cache, cargo freshness, process containment, class1 sandbox install, runtime staging and cutover, storage and control migration, ownership admission. About 85 `src/` modules, about 14.6k lines including tests (estimated split: roughly 40% tests). Largest: `thin-job-service.mjs` (1263), `thin-job-service.test.mjs` (1507), `thin-state.mjs` (710), `runtime.mjs` (610), `process-tree.mjs` (522), `native-openssl.mjs` (346), `config.mjs` (322), `thin-cache.mjs` (390), `bootstrap-macos-cli.mjs` (398), `runtime-cutover-windows.mjs` (340), `windows-build-root.mjs` (277).
- **C.** Consumers: none outside the workspace. Internal: `broker` is depended on by no package. It consumes `build-control-clients` and `build-control-core`.
- **D.** Direct dependencies:
  - `smol-toml@1.7.0`: used in `src/utilities.mjs:3` and `:38` for TOML config parsing. Verdict: NEEDED (or REPLACEABLE by JSON if the config format moves). Zero transitive deps (`pnpm-lock.yaml:2491`).
  - `@rightkit/build-control-core`: used by 11 modules (`broker.mjs:8`, `runtime.mjs:9`, `thin-job-service.mjs:28`, and others). NEEDED.
  - `@rightkit/build-control-clients`: dependency and devDependency both declare it. Runtime use at `cutover-shared.mjs:6`. Verdict: NEEDED, but the duplicate devDependency is TRIM.
  - Relative cross-package imports into the public release package (undeclared): `broker.mjs:2`, `control-migration.mjs:6`, `thin-job-service.mjs:1`, `bootstrap-macos-cli.mjs:2`, `runtime-cutover-macos-cli.mjs:2`, and five more `../../release/managed-roots.mjs` imports; `ownership-admission.test.mjs` imports `../../release/ownership-verdict.mjs`.
  - Node builtins only otherwise.
  - Build scripts `build:windows-job` and `build:macos-resource` run `node-gyp` (not declared in devDependencies). `node-gyp` fetches Node headers unless `--nodedir` is set (general node-gyp behaviour, not measured here).
- **E.** External closure: 1 package (`smol-toml`). Estimated with defaults and minimal features: same.
- **F.**
  - **TOCTOU on native addon load.** `windows-job.mjs:25-26` and `macos-resource.mjs:25-26` read the `.node` file twice (once for size, once for SHA-256 digest) and then call `require(path)` later, which reads it a third time. The bytes verified are not the bytes loaded. Load from the verified buffer or a verified copy.
  - Hard-coded `/Volumes/D` fallback: `cache-proof-cli.mjs:90`.
  - `legacy-v1/` is an empty directory (`packages/build-control-broker/legacy-v1`).
  - `sha256File` duplicated (see G21).
  - Native addon builds are not declared as dependencies (see D).
  - `thin-logger.mjs` (41 lines) duplicates logging owned by `rightkit-logs` in Rust (G28).

### 1.4 `@rightkit/build-control-policy` (no version field in manifest; index says 0.1.0-phase1)
- **A.** Path `packages/build-control-policy`. Private. No `engines`, no `exports`, no bins declared (README instructs invoking `src/install-agent-policy.mjs` directly, `README.md:8`). Platforms: macOS and Windows agent shells.
- **B.** Agent-policy hook decisions for Claude, Cline, Codex and CommandCode (`index.mjs:390` lines; `claudeHookDecision`, `clineHookDecision`, `githubActionsOnlyViolation`, `managedBuildViolation`). Write-time ownership gate (`ownership-write-hook.mjs`, 503 lines, `handleOwnershipWriteHook`). Session hook (`ownership-session-hook.mjs`, 43 lines). Hook installer (`install-agent-policy.mjs`, 78 lines, `appendHook`, `installAgentPolicyHooks`). Asset installer (`install-policy-assets.mjs`, 85 lines). Drift check (`rightkit-drift.mjs`, 113 lines). Four hook shims (4-6 lines each).
- **C.** Consumers: none outside the workspace. Installed into agent configs at runtime.
- **D.**
  - No declared dependencies.
  - Undeclared relative imports: `install-policy-assets.mjs:2` and `install-agent-policy.mjs:2` import `../../release/managed-roots.mjs`; `index.mjs:4` imports `../../build-control-core/src/local-development.mjs`; `ownership-write-hook.test.mjs:8` imports `../../build-control-clients/src/ownership-lookup.mjs`. Verdict: NEEDED but must be declared (or moved into core).
  - Node builtins: `fs`, `path`, `url`, `os`, `child_process`, `crypto`, `util`.
- **E.** External closure: 0.
- **F.**
  - `background-process.mjs` (51 lines) is byte-identical to `packages/release/background-process.mjs` (checked with `diff -q`). See G19.
  - `ownership-availability.mjs` is byte-identical to the clients copy (G22).
  - Two generated catalogs (`ownership-session-catalog.json`, `ownership-fingerprints.json`) are identical to the clients copy (md5 `b1b8407987b7a2b1d17af1ff8df8cd29` for all three). Versions JSON identical to release (G22).

### 1.5 `@rightkit/build-control-staging` (0.1.0-phase1)
- **A.** Path `packages/build-control-staging`. Private. Node `>=24`. Platform: none specific.
- **B.** Two pure functions in `src/index.mjs` (11 lines): `createExternalStage` (`:3-5`) and `prepareSyncBack` (`:7-9`). Test is 12 lines. Its own README (`OWNERSHIP-BROKER-DESIGN.md:18`) notes it "currently only returns descriptors" and does not copy or hash source.
- **C.** Consumers: none. Not referenced by `build-control-broker`, `build-control-clients`, any app repo, or the root scripts (only `test:build-control` at `package.json:28`).
- **D.**
  - `@rightkit/build-control-core` declared (`package.json:8`) but **UNUSED**: the staging `src` and `test` files import only `node:path`, `node:test`, `node:assert/strict`, and `./index.mjs`.
- **E.** External closure: 0.
- **F.** Dead-end module. Declares an unused dependency. Its design goal (copy and sync-back, per `OWNERSHIP-BROKER-DESIGN.md:18`) is not implemented here.

### 1.6 `@rightkit/build-control-phase0` (0.0.0-phase0)
- **A.** Path `packages/build-control-phase0`. Private. Node `>=24`. Bin `rightkit-phase0` (`src/cli.mjs`). Sub-packages `macos/` (`@rightkit/build-control-phase0-macos`) and `windows/` (`@rightkit/build-control-phase0-windows`) each have their own `package.json`, but `pnpm-workspace.yaml` globs only `packages/*`, so they are not workspace members.
- **B.** Feasibility evidence harness for "Build Control Plane V5". Machine inventory, capacity model, clone and write probes, endpoint-security spike, baseline, provenance. Output schemas in `schemas/*.json` (six files) and `macos/schema/evidence.schema.json`. Swift stub `macos/extension/RightKitEndpointExtension.swift` (13 lines).
- **C.** Consumers: none in code. Referenced by root `package.json:26-27` (`test:build-control-phase0`, `run:build-control-phase0`), by `INDEX.md:67`, and by the ownership catalog (`build-control-clients/src/ownership-catalog.json:2264-2285`). Its outputs live under `docs/plans/evidence/rightkit-build-control-plane-v5/`.
- **D.**
  - `ajv@8.17.1` (devDependency), used only by `src/schema.test.mjs:6` (`ajv/dist/2020.js`). Verdict: NEEDED for test only.
  - Node builtins only otherwise.
- **E.** Dev closure: 5 packages (`ajv`, `fast-deep-equal`, `fast-uri`, `json-schema-traverse`, `require-from-string`; `pnpm-lock.yaml:1504`, `3527-3533`).
- **F.**
  - Not in `test` or `test:build-control` aggregates (`package.json:28`). Only runnable through its own script.
  - Evidence harness that has served its purpose; see I.

### 1.7 `@rightkit/agent-room-mcp` (0.1.0)
- **A.** Path `packages/agent-room-mcp`. Private, `license: UNLICENSED`. No `engines`. Node with `fetch` and TypeScript type-stripping (test script `node --test src/index.test.ts`, `package.json:9`). Platform-neutral.
- **B.** Stdio MCP bridge (JSON-RPC over `readline`) to the Rust `agent-room-core` HTTP server. Twelve `room_*` tools (`index.ts:6-11`). Forwards to `/api/join` (`:56`) and `/api/tool/<name>` or `/api/status` (`:60`). Requires env `AGENT_ROOM_URL` (`:216-217`) and optionally `AGENT_ROOM_CREDENTIALS` (`:221`). Exports `createMcpHandler` and `TOOL_NAMES`.
- **C.** Consumers: none in package graph. Its server counterpart is `crates/agent-room-core/src/server.rs` (HTTP). Included in root `test` (`package.json`, `test:agent-room-mcp`).
- **D.** No dependencies. Node builtins: `readline`, `fs`, `path`, `url`, `os`.
- **E.** External closure: 0.
- **F.**
  - Credentials stored as plaintext JSON including bearer and resume tokens (`index.ts:108-110`, mode `0600`). `rightkit-secrets` exists in Rust for this purpose.
  - Re-implements a JSON-RPC MCP server that `rightkit-mcp` (Rust, shared MCP host, 8,618 lines) already provides (G32).
  - `rightkit-ownership.json` declares the capability as declaration-only (no fingerprint).

---

## 2. Swift

### 2.1 `swift/RightKitSwift` (VERSION `0.3.8`, `Package.swift` tools 6.0)
- **A.** Path `swift/RightKitSwift`. Platforms `iOS 17`, `macOS 14` (`Package.swift`). Publication: git URL plus tag via `packages/release/publish-swift.mjs`. Ownership declaration: `rightkit-ownership.json` capability `os-credential-store`, declaration-only.
- **B.** Shared Apple infrastructure. 14 library products, each a target of the same name, and 14 test targets. Foundation/CryptoKit only, no third-party dependency (`Package.swift` header comment).
- **C.** Consumers: `swift/RightKitCloudKitHelper` (`Package.swift`, `.package(path: "../RightKitSwift")`, product `RightKitCloudSync`). `swift/ios-app-template/project.yml:33-50` and `:78` list the products. Apps consume it by git URL (`swift/README.md`). No app-repo `Package.swift` was checked.
- **D.** Direct dependencies: none third-party. System libraries and frameworks per target:

| Target (product) | Sources LOC | Imports / linked libs | Verdict |
|---|---|---|---|
| RightKitLicense | 1534 | Foundation, CryptoKit, Security, Combine, UIKit (guarded, `LicenseClient.swift:7`) | NEEDED |
| RightKitDiagnostics | 150 | Foundation | NEEDED (G14) |
| RightKitArtifacts | 259 | Foundation, CryptoKit, zlib (`linkerSettings` in `Package.swift`) | NEEDED |
| RightKitPurchases | 874 | StoreKit, Security, Combine, UIKit (guarded), depends on RightKitLicense | NEEDED |
| RightKitCloudSync | 2186 | CloudKit, Network, CryptoKit, Foundation | NEEDED (used by helper) |
| RightKitStore | 506 | Foundation, Darwin (guarded) | NEEDED |
| RightKitImportQueue | 432 | Foundation, UniformTypeIdentifiers, depends on RightKitStore | NEEDED |
| RightKitLiveActivity | 177 | Foundation, ActivityKit (guarded) | NEEDED |
| RightKitWatchBridge | 247 | Foundation, WatchConnectivity (guarded, `WatchBridgeSupport.swift:91`) | NEEDED |
| RightKitSpeech | 3915 | Foundation, CoreML (10 files), AVFoundation (8), Speech (2), Accelerate (2); resource `whisper_mel_128.bin` (102,912 bytes) | NEEDED (largest; G10) |
| RightKitIntents | 139 | Foundation, CoreFoundation, AVFAudio | NEEDED |
| RightKitAppleCredentials | 373 | Security, LocalAuthentication, Foundation | NEEDED (G13) |
| RightKitOverlay | 166 | Foundation, AppKit (under `#if os(macOS)`, `Overlay.swift:135`) | NEEDED (G11) |
| RightKitUI | 143 | SwiftUI | NEEDED |

- **E.** Transitive third-party count: 0. Frameworks only.
- **F.**
  - `swift/README.md` "Minimal package surface" table names `RightKitLicensing` and `RightKitQA`. Neither exists in `Package.swift`. The "16/16" test count is stale (14 test targets exist). `RightKitSwift/README.md` says it was published as `0.1.0` while `VERSION` is `0.3.8`.
  - `LicenseController` is `public final class ... : ObservableObject` (`RightKitLicense/LicenseClient.swift:860`). The README rule says "product-facing ObservableObject controllers stay in app repos". Doc and code disagree.
  - The license golden vector is duplicated in `swift/RightKitSwift/Tests/RightKitLicenseTests/Contracts/license-v2.json`. Its md5 matches `swift/contracts/license-v2.json`, but `swift/contracts/contracts.test.mjs` does not check the Tests copy (G12).
  - `RightKitSpeech` and `RightKitOverlay` duplicate Rust algorithms (G10, G11).

### 2.2 `swift/RightKitCloudKitHelper` (macOS 14)
- **A.** Path `swift/RightKitCloudKitHelper`. Executable `rightkit-cloudkit-helper`. Not published separately.
- **B.** Line-JSON `rightkit-sync-helper/1` over stdin/stdout, using `RightKitCloudSync`. Sources: `Session.swift` (298), `Zone.swift` (285), `main.swift` (55).
- **C.** Consumed by `crates/rightkit-sync/src/helper/mod.rs` (`CloudKitHelper`, `:36`) and `helper/process.rs`.
- **D.** One product dependency: `RightKitCloudSync` (local path).
- **E.** Transitive: RightKitSwift closure only (no third-party).
- **F.** Path dependency `../RightKitSwift` (not a version pin). This differs from the git-tag model the README describes.

### 2.3 Other Swift directories (not packages in scope)
- `swift/ios-app-template`: Xcode template (`project.yml`, lines 13-78 consume `RightKitSwift` products; `scripts/check-package-pin.sh`, `scripts/build-with-local-mirror.sh`).
- `swift/scripts/archive-package.mjs` (plus test): deterministic `git archive` for a local registry-style publish. Referenced **only** by its own test. Real publication is `packages/release/publish-swift.mjs` (git mirror plus tag). This is a second distribution contract (G-I).
- `swift/contracts/`: `license-v2.json` plus `contracts.test.mjs`, the cross-language golden-vector test run by root `test:swift-contracts` (`package.json:29`).
- `swift/scraperight-ios-spike/`: spike project, not a RightKit package.

---

## 3. `ownership-approval/` (not a package; no `package.json`)

Read-only observations. Trust, enrollment, and signing are protected by design, and nothing here was executed.

| Component | Size | Role | Notes |
|---|---|---|---|
| `README.md` | 84 lines | Enrollment ceremony (Mac Secure Enclave P-256, Windows Hello RSA-2048) | Commands point to `packages/release/cli/right-release.mjs` |
| `install-trust.mjs` | 195 | Installs protected trust file | Uses `node:child_process` `spawnSync` directly (`:7`); not the release wrapper |
| `prepare-enrollment.mjs` | 53 | Builds enrollment candidate | Imports `DEFAULT_TRUST_PATH` from `../packages/release/ownership-approval.mjs:6` |
| `prepare-trust-floor.mjs` | 38 | Trust floor | Imports `packages/release/ownership-approval.mjs` and `ownership.mjs` |
| `trust-schema.mjs` | 164 | Schema, `sha256` (`:52`), `canonicalJson` (`:56`), `evaluateWindowsAcl` | Duplicates `packages/release` helpers (G20) |
| `adapters/private-local.mjs` | 330 | Local protected-install audit | Imports `packages/release/*` four times |
| `adapters/public-check-producer.mjs` | 294 | Produces signed public checks | Signs with `createPrivateKey` and `sign` (`:4`) |
| `adapters/private-verifier.mjs` | 85 | Verifier | Uses `packages/release/background-process.mjs` `spawnSync` (`:7`) |
| `adapters/package-manifest.mjs` | 86 | Protected-file manifest | Uses `createHash` and `readdirSync` |
| `adapters/receipt.mjs` | 61 | Receipts | Imports `canonicalJson`/`sha256` from `trust-schema.mjs` |
| `adapters/runtime-environment.mjs` | 17 | Child env | Small |
| `conformance/native-source-conformance.test.mjs` | 289 | Reads Swift and C# sources to assert conformance | Uses `conformance/v2-vectors.json` (562 lines) |
| `macos/ownership-approval.swift` | 1046 | AppKit review UI, Secure Enclave, Keychain service `com.orthiclabs.rightkit.ownership` (`:12`) | Compiled by `macos/build.sh` (45 lines, `swiftc` at `:29`) |
| `macos/keychain-storage-impostor.swift` | 70 | Negative ACL probe (reads caller-supplied UUIDs only) | Compiled by `build.sh:29` |
| `windows/OwnershipApproval.cs` | 697 | WinForms review UI, Windows Hello RSA-2048 | `net10.0-windows10.0.22000.0` (`csproj`) |
| `windows/install-class1-service.ps1` | 217 | Service installer | PowerShell |
| `migrations/prepare-migration-drafts.mjs` | 272 | Builds unsigned draft records | Reads `tasks/handoffs/2026-10-07/ownership/` |
| `migrations/validate-migration-drafts.mjs` | 75 | Validates drafts | Imports `packages/release/background-process.mjs` (`:6`) |
| `migrations/drafts/*.json` | 102 files | Unsigned records across 8 repos (coderight 23, cutright 3, genright 5, heardright 13, mailright 11, rightsites 2, scraperight 12, shell variants) | Evidence, not authority |
| `first-install-baseline.json` | 75 | Install baseline | Data |
| `install-trust.test.mjs` (83), `prepare-enrollment.test.mjs` (53), `adapters/*.test.mjs` (4 files) | | Tests | **Not run by any workspace script** (see F) |

**Findings (F):**
- Tests not wired: `packages/release/package.json:21` runs `*.test.mjs` only inside `packages/release`. Root `package.json` has no ownership-approval entry. No CI config was found (`/Volumes/D/claude/.github` does not exist). The `ownership-approval/**/*.test.mjs` files therefore never run in a configured gate.
- Duplicate helpers with `packages/release` (G20).
- Two native signers (Swift and C#) implement the same key-log event model. Cross-language drift is guarded only by `conformance/native-source-conformance.test.mjs`.
- `migrations/README.md` says `prepared-migration-manifest.json` is generated locally, but no such file is tracked.

---

## 4. `contracts/` (repo root)

- Six JSON Schemas: `content-brief.v1`, `daemon-api.v1`, `delivery-envelope.v1`, `demo-plan.v1`, `record-event.v1`, `record-session.v1` (1,386 lines total).
- Fixtures: `contracts/fixtures/<contract>/{invalid,semantic-invalid,version-rejected,...}` for six contracts.
- **Consumers:** only `crates/rightkit-contracts`, which embeds the schemas with `include_str!` (`crates/rightkit-contracts/src/lib.rs:98-108`). No npm or Swift consumer. The crate's Rust tests (`tests/contracts.rs`, `tests/fixtures.rs`) are the only checks.
- **A/B/C/D/E/F:** NEEDED. No duplicates of these schemas found. Note that `contracts/` and `crates/rightkit-contracts/` were untracked at snapshot time.

---

## 5. `scripts/` and bin entries

- `scripts/generate-index.mjs` (74 lines): regenerates `INDEX.md` from manifests (`rightkit-ownership.json`, `Cargo.toml`, `package.json`). No deps. NEEDED.
- `scripts/install-right-ctl.sh` (17 lines): runs `rightkit cargo build -p rightkit-agent-tools --release --bin right-ctl`, Developer-ID signs, and installs to `$root/agent-bin/right-ctl`. Default `root` is hard-coded to `/Volumes/D/.rightkit-managed` (`:8`). Depends on the `rightkit` shim from `@rightkit/build-control-clients` being on PATH. NEEDED, but the path should be parameterised (see K).
- `scripts/run-ui-tests.sh` (21 lines): builds Rust UI tests through `rightkit cargo test --no-run` and then executes the binaries in the user session. NEEDED.
- Bin entries (summary): `rightkit`, `rightkit-managed-agent`, `rightkit-install-agent-shims` (clients); `rightkit-build-broker`, `rightkit-build-cutover-macos`, `rightkit-build-cutover-windows`, `rightkit-build-bootstrap-macos`, `rightkit-build-bootstrap-windows`, `rightkit-cache-proof`, `rightkit-build-control-migrate`, `rightkit-build-storage-migrate` (broker); `rightkit-phase0` (phase0). Policy declares no bins. Rust bins: `right-ctl` (agent-tools, used by the install script), `rightkit-ocr`, `rightkit-tts-worker`, `rightkit-clonefile`, `rightkit-dump-helper`, `rightkit-control`, `rightkit-qa`, `rightkit-secrets`, `room` (agent-room-core), `right-ctl`.

---

## 6. Cross-cutting sections

### G. Duplicated responsibilities

Owner is the proposed single implementation. Lines are as found.

| # | Responsibility | Copies (file:line) | Proposed owner |
|---|---|---|---|
| G1 | Atomic file write (Rust) | `rightkit-fs/src/atomic.rs:51,56,62`; `rightkit-pdf/src/commit.rs:23`; `rightkit-shell/src/recovery.rs:146`; `rightkit-service/src/record.rs:122`; `rightkit-settings/src/atomic.rs:11,17`; `rightkit-logs/src/atomic.rs:14`; `rightkit-models/src/store.rs:981`; `rightkit-models/src/suite/manifest.rs:227` | `rightkit-fs` (8 copies, none depends on it except `rightkit-db`) |
| G2 | Advisory file locks | `rightkit-fs/src/lock.rs:179-194,286` (owner); raw `libc::flock` in `rightkit-service/src/discovery.rs:277-288` (+ Windows variant `:309`); `rightkit-jobs/src/runner.rs:59-82,109-121`; `rightkit-settings/src/store.rs:143`; `rightkit-models/src/lock.rs:93,157`; `rightkit-models/src/suite/lease.rs:239`; `rightkit-models/src/suite/refs.rs:60,72` | `rightkit-fs::lock` |
| G3 | Windows Job Object containment (kill-on-close) | `rightkit-process/src/process_windows.rs:68,81` (+ `owned.rs:800-803`); `rightkit-sandbox/src/windows_job.rs:34,59`; `rightkit-automation/src/win/sys.rs:579,601` | `rightkit-process` |
| G4 | Process-tree enumerate and kill | `rightkit-sandbox/src/process_tree.rs:81,149,185` (`kill_tree`); `rightkit-process` (`killpg`, unix group); Node `build-control-broker/src/process-tree.mjs:28` (`spawnManagedProcess`) and `:79,108`; `pgrep` shell-outs in `rightkit-control/src/mac.rs:522`, `rightkit-qa/src/process.rs:45` | `rightkit-process` (Rust); Node copy stays with broker |
| G5 | Direct `Command` spawns outside `rightkit-process` | No dependency on process: `rightkit-control/src/mac.rs` (6), `rightkit-models/src/hardware.rs` (4), `rightkit-crash/src/native.rs` (3), `rightkit-service/src/discovery.rs` (3), `rightkit-automation/src/macos/appkit.rs` (2), `rightkit-secrets/src/apple_helper.rs` (1), `rightkit-fs/src/private.rs` (1), `rightkit-ocr/src/worker.rs` (1), `rightkit-mcp/src/client_features.rs` (1), `rightkit-speech-client/src/lib.rs` (1), `rightkit-sync/src/helper/process.rs` (1), `rightkit-computer/src/macos.rs` (1). Depends on process but still spawns directly: `rightkit-tts/src/system.rs` (4), `rightkit-tts/src/client.rs` (1), `rightkit-sandbox/src/spawn.rs` (2), `rightkit-shell` (6 across `opener.rs`, `autostart.rs`, `instance.rs`, `deep_link.rs`), `rightkit-qa/src/process.rs` (9) and `control.rs` (6) | `rightkit-process` |
| G6 | HTTP clients | `rightkit-http` (ureq wrapper, owner; used by `llm`, `oauth`); own `ureq::Agent` in `rightkit-logs/src/upload.rs:81,88`; `rightkit-models/src/rightapps.rs:184,197`; `rightkit-models/src/download.rs:183-215`; `rightkit-mcp` (ureq and `reqwest` 0.12 with tokio) plus `advanced_http.rs` | `rightkit-http`; `reqwest` only if async is required |
| G7 | Archive extraction and member-path validation | `rightkit-models/src/archive.rs:17,45,85,128,171` and `rightkit-models/src/suite/archive.rs:17,124,181,224` (same-named helpers `checked_member_path`, `is_ignorable_apple_metadata` in one crate); Swift `RightKitArtifacts/ArtifactInstaller.swift:103,109,116` (`validateTopLevelName`, `extractTarGz`, `inflateGzip`) | Single safe-extract module in `rightkit-fs`; Swift keeps its own |
| G8 | Model download and resumable fetch | `rightkit-models/src/download.rs:183,193,214`; `rightkit-models/src/install.rs:946` (`fetch_file`); `rightkit-models/src/suite/install.rs:70`; `rightkit-models/src/suite/store.rs:292,331` (`download_archive`, `download_files`); Swift `ArtifactInstaller.swift:36,48,60` (SHA-256 verify and install) | `rightkit-models` (one stack, see H4); transport from `rightkit-http` |
| G9 | ONNX Runtime session and environment setup | Owner `rightkit-ort/src/session.rs:138,182`, `environment.rs:296`, `lib.rs:68,99,413-414` (`ORT_DYLIB_PATH`). Direct `Session::builder` elsewhere: `rightkit-vad/src/silero.rs:55-63`; `rightkit-asr/src/model_transducer.rs:90,137`; `rightkit-asr/src/model_multitalker.rs:85`; `rightkit-qts/src/pipeline/vocoder.rs:29,780,788`; `rightkit-ocr/src/support/session.rs:32,48`. `ORT_DYLIB_PATH` re-implemented in `rightkit-tts/src/models.rs:149-153`, `worker.rs:52-54`, `client.rs:151` | `rightkit-ort` |
| G10 | Speech inference algorithms in two languages | Whisper log-mel: Rust `rightkit-asr/src/whisper/coreml/mel.rs:15,27` vs Swift `RightKitSpeech/WhisperMelSpectrogram.swift:36,44,117`. Parakeet beam search: Rust `rightkit-asr/src/parakeet_coreml/decode.rs` vs Swift `ParakeetBeamSearch.swift:19,26,44`. VAD: Rust `rightkit-vad/src/silero.rs:36` vs Swift `VoiceActivityDetector.swift:13` | Keep one per runtime (Swift for Apple apps, Rust for desktop); share golden fixtures from one place, not code |
| G11 | Overlay placement policy | Rust `rightkit-overlay/src/lib.rs:261-265` (`WorkArea::clamp`) and `:302` (`place`); Swift `RightKitOverlay/Overlay.swift:97,121-124`. Header of Rust crate names "React pill, native pill, Cockpit notch", i.e. a third copy in TS not located | One implementation; Rust crate has zero consumers (see I) |
| G12 | License verification | Rust `rightkit-license/src/lib.rs:303` (`verify_token`); Swift `RightKitLicense/LicenseToken.swift:93,118` (`LicenseVerifier.verify`); JS `packages/license/src/index.ts`. Golden vector `license-v2.json` in four identical copies: `swift/contracts/`, `crates/rightkit-license/test-vectors/`, `packages/license/test-vectors/`, `swift/RightKitSwift/Tests/.../Contracts/` (md5 `085249124998683b7739e5b20fb43aba` for all four) | Rust is the security owner; one vector source referenced by path, not copied |
| G13 | Credential storage | `rightkit-secrets/src/apple_helper.rs` (spawns helper); `rightkit-secrets/src/lib.rs` (Keychain / Credential Manager); Swift `RightKitAppleCredentials.swift` (373 lines; Security, LocalAuthentication); `ownership-approval/macos/ownership-approval.swift` (Keychain service `:12`); `rightkit-oauth` `secrets` feature; `agent-room-mcp/src/index.ts:108-110` writes plaintext tokens | `rightkit-secrets` (Rust) and `RightKitAppleCredentials` (Swift); `agent-room-mcp` should use one |
| G14 | Redaction and diagnostics logs | Rust `rightkit-logs/src/redact.rs:35,39`, `scrub.rs:40` (`redact_quoted`); `rightkit-crash` (uses logs); Swift `RightKitDiagnostics/PrivacyRedactor.swift:9`, `JSONLDiagnosticStore.swift:21`; JS `@rightkit/logs` (client); Node `build-control-broker/src/thin-logger.mjs:4` | `rightkit-logs` (Rust) and `@rightkit/logs` (JS client); Swift keeps its own |
| G15 | Chrome for Testing resolution and launch | `rightkit-browser/src/config.rs` (759 lines), `rightkit-browser/src/error.rs`; `rightkit-qa/src/browser.rs` (wraps rightkit-browser); `packages/qa-chrome/src/install.ts:122` (sha256), `packages/qa-chrome/src/index.ts` (464 lines) | `@rightkit/qa-chrome` for node harnesses; `rightkit-browser` for Rust. Drop the copy in `rightkit-qa` |
| G16 | Line-delimited JSON framing | Owner `rightkit-framed-sidecar` (4 crate consumers: `rightkit-jobs`, `rightkit-qa`, `rightkit-service`, `rightkit-speech-client`). Hand-rolled: `rightkit-mcp/src/client.rs`, `server.rs`; `agent-room-core/src/protocol.rs`; `rightkit-tts/src/client.rs`, `worker.rs`; `rightkit-sync/src/helper/process.rs`; `rightkit-media/src/runner.rs`, `probe.rs`; Swift `RightKitCloudKitHelper/main.swift`; Node `build-control-clients/src/remote-protocol.mjs` | `rightkit-framed-sidecar` |
| G17 | Lock, lease and singleton (Node) | `build-control-core/src/storage.mjs:44` (`withStorageLock`); `build-control-broker/src/thin-singleton.mjs:20` (`acquireThinBrokerSingleton`); `build-control-clients/src/remote-worker.mjs:65` (`acquireWindowsSlotGuard`); `packages/release/build-release.mjs:662` (`acquireLock`) | One lock helper in `build-control-core` that release can import |
| G18 | Atomic write (Node) | `build-control-broker/src/thin-state.mjs:32,43` (`replaceAtomicFile`, `atomicJson` with Windows retry); `build-control-clients/src/index.mjs:393` (`writeAtomic`, no retry); `packages/release/cache-policy.mjs:458` (`atomicJson`, mode 600); `ownership-approval/install-trust.mjs` and `adapters/public-check-producer.mjs` (fsync plus rename) | `thin-state.mjs` version (only one with Windows retry), moved to a shared module |
| G19 | Process spawn wrappers (Node) | `packages/release/background-process.mjs` is byte-identical to `build-control-policy/src/background-process.mjs` (`diff -q`). `build-control-phase0/src/process.mjs` (18 lines) | `@rightkit/release` public module, or move to `build-control-core` (which release does not depend on; so put it in core and import from release) |
| G20 | SHA-256 and canonical JSON | `ownership-approval/trust-schema.mjs:52,56`; `packages/release/ownership-approval.mjs:76,81`; `packages/release/ownership-key-log.mjs:10`; `packages/release/ownership-report.mjs:21`; `packages/release/direct-bootstrap.mjs:14,22`. Also 35 non-test `createHash('sha256')` call sites in `build-control-*` and `ownership-approval`, and `crates` (14 Rust crates depend on `sha2`) | One canonical-JSON and hash module (Node: `build-control-core`; ownership-approval should import, not copy) |
| G21 | Tool discovery, hashing, arg parsing (Node broker) | `sha256File` byte-identical at `bootstrap-macos-cli.mjs:68` and `bootstrap-windows-cli.mjs:64`, plus `native-openssl.mjs:77`, `tool-registry.mjs:15`. `discoverTools`, `rustcVersionTriple`, `sccacheVersion` at `bootstrap-macos-cli.mjs:103,177,186` and `bootstrap-windows-cli.mjs:93,134,143`. `parseArgs` at `storage-migration.mjs:13`, `bootstrap-macos-cli.mjs:40`, `bootstrap-windows-cli.mjs:31`. `which` at `bootstrap-macos-cli.mjs:70,83` (Windows uses `where`) | `bootstrap-*` share one module |
| G22 | Generated ownership catalog and versions | Identical copies (md5 `b1b8407987b7a2b1d17af1ff8df8cd29`): `build-control-clients/src/ownership-catalog.json`, `build-control-policy/src/ownership-session-catalog.json`, `build-control-policy/src/ownership-fingerprints.json`. Identical `rightkit-versions.json`: `packages/release/`, `build-control-clients/src/`, `build-control-policy/src/ownership-session-versions.json`. `ownership-availability.mjs` identical in `build-control-clients/src` and `build-control-policy/src` | `packages/release` owns; consumers import or consume only the sync output (`ownership-catalog-sync.mjs` already does this for clients) |
| G23 | Agent hook runtime | `@rightkit/hooks` (`packages/hooks/bin`, `lib`; "neutral lifecycle runtime") vs `build-control-policy/src/index.mjs` (`claudeHookDecision`, `clineHookDecision`), `install-agent-policy.mjs` (`appendHook`), and four hook shims | `@rightkit/hooks` |
| G24 | Tauri host bridges and native host services | Rust `rightkit-shell` (7998 lines; autostart, deep link, opener, instance, recovery) and `rightkit-tauri` (898 lines; depends only on `rightkit-license`); npm `@rightkit/shell` (renderer bridge for rightkit-shell) and `@rightkit/tauri` (typed host adapters) | `rightkit-shell` plus `@rightkit/shell`; fold `rightkit-tauri` and `@rightkit/tauri` in |
| G25 | Accessibility and input synthesis (macOS) | AX: `rightkit-automation/src/macos/ax.rs`, `ffi.rs`; `rightkit-control/src/mac.rs`; `rightkit-computer/src/macos.rs`; `rightkit-hotkeys/src/macos/ffi.rs`, `middle_mouse.rs`. CGEvent: `rightkit-automation/src/macos/events.rs`; `rightkit-control/src/lib.rs`, `embedded/mac_view.rs`; `rightkit-computer/examples/textedit_proof.rs`; `rightkit-hotkeys/src/macos/synth.rs`, `tap.rs` | `rightkit-automation` |
| G26 | PDF engine | `rightkit-pdf` (8163 lines; zero manifest consumers) vs `viewright/src-tauri/crates/pdf` (4586 lines, app-local; ViewRight's `Cargo.toml` lists it as a path member). `rightkit-pdf`'s header says it was extracted from that crate | Decide one: ViewRight's copy is still live, so the shared crate is the duplicate (see I) |
| G27 | OCR | `rightkit-ocr` (2943 lines; zero consumers) vs `viewright/src-tauri/crates/ocr` (app-local); `rightkit-ocr/src/native/vision.rs` (Apple Vision) | Pick one owner |
| G28 | Logging clients | `rightkit-logs` (3500 lines), `@rightkit/logs`, Swift `RightKitDiagnostics`, Node `build-control-broker/src/thin-logger.mjs` | `rightkit-logs` |
| G29 | Settings persistence | `rightkit-settings` (924 lines; consumer genright; dependency of `rightkit-secrets` only via `settings-probe`), `@rightkit/settings-ui` (persists "through rightkit-settings"), Swift `RightKitStore/RevisionedStateStore.swift` | `rightkit-settings` |
| G30 | Model store (intra-crate) | `rightkit-models/src/store.rs` and `src/suite/store.rs`; `install.rs` and `suite/install.rs`; `lock.rs` and `suite/lease.rs`; `archive.rs` and `suite/archive.rs` (see H4) | Keep `suite/` after migration; delete root copies |
| G31 | Cloud sync engine | Rust `rightkit-sync` (4294 lines) vs Swift `RightKitCloudSync` (2186 lines). Intentional split (helper protocol, `API.md`) | Keep both; ensure one shared golden fixture set |
| G32 | MCP JSON-RPC server | Owner `rightkit-mcp` (8618 lines); `packages/agent-room-mcp/src/index.ts` (237 lines); `crates/agent-room-core/src/protocol.rs` | `rightkit-mcp` |
| G33 | Job queue, lease, and runner | `rightkit-jobs` (8352 lines, job store on `rightkit-db`, `JobLocks`) vs `build-control-broker/src/thin-job-service.mjs` (1263) and `thin-scheduler.mjs` (181). Different domains (app jobs vs build jobs) but the same lock and lease concepts | Share lock primitive (G2); do not merge domains |
| G34 | Cache primitives | `rightkit-cache` (643 lines, `DiskCache` at `disk.rs:95`; zero consumers) vs `build-control-broker/src/thin-cache.mjs` (390) and `cache-telemetry.mjs`; `rightkit-embed` vector store | Keep `rightkit-cache` for generic disk cache, or delete if unused |

### H. Responsibilities split sub-optimally

1. **Process and containment:** merge the Windows job code (G3) and process-tree kill (G4) into `rightkit-process`. Move `rightkit-sandbox/src/process_tree.rs` and `windows_job.rs` there. `rightkit-sandbox` keeps Seatbelt, Landlock, AppContainer and PTY. Remove the `pgrep` shell-outs in favour of process-tree APIs.
2. **Atomic write, locks, and safe archive into `rightkit-fs`:** fold the eight atomic writers (G1), the lock variants (G2), and the safe archive helpers (G7) into one `rightkit-fs` surface. `rightkit-settings`, `rightkit-logs`, `rightkit-service`, `rightkit-pdf`, `rightkit-shell` and `rightkit-models` would then depend on it. Note `rightkit-fs` already pulls `nix` and `libc`, so the dependency cost for small crates should be checked.
3. **Model stack in `rightkit-models`:** one implementation. Delete the root `store.rs`, `install.rs`, `lock.rs`, `archive.rs`, `download.rs` once `suite/migration.rs` covers legacy data. Consolidate `download` into `rightkit-http` (G6, G8).
4. **ONNX in `rightkit-ort`:** `rightkit-asr`, `rightkit-vad`, `rightkit-ocr`, `rightkit-embed` and `rightkit-qts` each pin `ort` at `=2.0.0-rc.13` and call `Session::builder` directly. Route all session creation and `ORT_DYLIB_PATH` handling through `rightkit-ort` (G9). Consider one ONNX model family crate instead of five, but only after the consumers are measured.
5. **TTS trio:** `rightkit-qts-ggml` (59 lines, thin wrapper over `rightkit-qts-ggml-sys`) should move into `rightkit-qts` or `-sys`. `rightkit-tts` (1444) depends on `rightkit-qts` only behind an `engine` feature and has zero consumers.
6. **Speech in `rightkit-transcript` plus `rightkit-align`:** `rightkit-align` (1097) has zero consumers and depends only on `rightkit-transcript`. Merge into `rightkit-transcript` behind a feature.
7. **Computer and automation:** `rightkit-computer` (606 lines, prototype with `examples/textedit_proof.rs`, zero consumers) duplicates AX code in `rightkit-automation`. Merge into `rightkit-automation` or delete.
8. **Overlay:** one owner. The Rust crate has zero consumers; Swift `RightKitOverlay` is the only live implementation (G11). Keep Swift; delete or archive the Rust crate after confirming.
9. **Framing:** route all line-JSON users (G16) through `rightkit-framed-sidecar`. Keep `rightkit-speech-client` and `rightkit-service` as separate crates; they are distinct protocols.
10. **Crash into logs:** `rightkit-crash` (1188 lines) depends on `rightkit-logs` and has zero app consumers. Make it a feature of `rightkit-logs`.
11. **Search into db:** `rightkit-search` (1647 lines, optional `rusqlite`) has zero consumers and overlaps `rightkit-db` (which already depends on `rusqlite` with `backup`). Keep one.
12. **Tauri:** fold `rightkit-tauri` (898 lines; only depends on `rightkit-license`) into `rightkit-shell`. Fold `@rightkit/tauri` into `@rightkit/shell`. Consumers (scraperight, viewright, heardright, coderight, mailright, desktop-app-template) need a version bump.
13. **Hooks:** make `build-control-policy` consume `@rightkit/hooks` for lifecycle plumbing and keep only the ownership and Cargo decisions it owns (G23).
14. **Chrome:** keep resolution in `@rightkit/qa-chrome` (node) and `rightkit-browser` (Rust). Remove the copy in `rightkit-qa/src/browser.rs` (G15).
15. **Node private packages:** `build-control-core` should be the single home for shared Node primitives (lock, atomic write, hash, background spawn). `build-control-clients` should not import from `packages/release` at runtime; either move the shared module into `build-control-core` or declare the dependency.
16. **Swift distribution:** a single distribution contract (`publish-swift.mjs`). Delete `swift/scripts/archive-package.mjs` or wire it into that script.

### I. Removal and merge candidates

Evidence is manifest-level (zero Cargo or npm consumers in app repos or the workspace), unless marked otherwise.

| Candidate | Evidence | Replacement or action | Confidence |
|---|---|---|---|
| `rightkit-computer` | Zero consumers; `examples/textedit_proof.rs` only; duplicates AX (G25) | Merge into `rightkit-automation` or delete | High |
| `rightkit-overlay` | Zero consumers; Swift overlay is the live policy (G11) | Delete or archive | High |
| `rightkit-align` | Zero consumers; depends only on `rightkit-transcript` (H6) | Merge into `rightkit-transcript` | High |
| `rightkit-compose` | Zero consumers and zero deps in workspace; "RK9" model header | Confirm owner, then delete or archive | Medium |
| `rightkit-cache` | Zero consumers; broker keeps its own cache (G34) | Delete or adopt in broker | Medium |
| `rightkit-search` | Zero consumers; overlaps `rightkit-db` (H11) | Fold into `rightkit-db` feature | Medium |
| `rightkit-hotkeys` | Zero consumers; macOS hotkey and synth (G25) | Confirm with apps, then fold into automation or delete | Medium |
| `rightkit-playback` | Zero consumers | Confirm owner | Medium |
| `rightkit-tts` (+ `rightkit-qts-ggml`) | Zero consumers; `rightkit-qts` is consumed by genright only (H5) | Confirm; fold `-ggml` wrapper | Medium |
| `rightkit-crash` | Zero consumers; depends on logs (H10) | Fold into `rightkit-logs` | Medium |
| `rightkit-llm` | Zero consumers (manifest and text check on `rightkit-llm` returned none) | Confirm coderight does not use it | Medium |
| `rightkit-audio` | Zero consumers | Confirm cutright or others do not use it | Medium |
| `rightkit-pdf` | Zero consumers; ViewRight keeps its own live copy (G26) | Decide owner; probably delete the shared crate and migrate ViewRight | Medium |
| `rightkit-ocr` | Zero Cargo consumers; ViewRight keeps its own OCR crate (G27) | Decide owner | Medium |
| `rightkit-sync` | Zero Cargo consumers; the Swift helper is its only real peer (G31) | Keep if the helper is used; confirm | Low |
| `rightkit-agent-tools` | Zero Cargo consumers but **installed** as `right-ctl` by `scripts/install-right-ctl.sh` | Keep | n/a |
| `agent-room-core` | Zero Cargo consumers; runs as the HTTP server for `agent-room-mcp` | Keep | n/a |
| `@rightkit/i18n` | Zero consumers in app repos and workspace | Confirm; delete if unused | Medium |
| `@rightkit/qa` | Only consumed as devDependency by workspace packages | Keep | n/a |
| `@rightkit/build-control-staging` | 11-line module; no consumers; declares an unused dependency (1.5) | Delete, or fold into broker | High |
| `@rightkit/build-control-phase0` | Phase-0 evidence harness; referenced only by root scripts, catalog and INDEX | Archive evidence and delete the package | High |
| `build-control-broker/legacy-v1/` | Empty directory | Delete | High |
| `swift/scripts/archive-package.mjs` | Referenced only by its own test; superseded by `publish-swift.mjs` | Delete with its test | High |
| `swift/RightKitSwift/Tests/.../Contracts/license-v2.json` | Duplicate of `swift/contracts/license-v2.json` (same md5) | Reference the single copy | High |
| `packages/build-control-policy/src/background-process.mjs` | Byte-identical to `packages/release/background-process.mjs` | Import instead | High |

### J. npm multi-version packages (`pnpm-lock.yaml`)

Parsed from the `packages:` and `importers:` sections: 471 names, 539 entries, 43 names with more than one version. Direct-importer tracing covers the names below. Transitive-only cases are marked "not traced".

| Package | Versions in lock | Pulled by (traced) |
|---|---|---|
| `@types/node` | 22.20.0, 24.13.3 | 22.20.0 by `packages/legal`, `packages/license`; 24.13.3 by `packages/qa`, `qa-chrome`, `theme` |
| `jsdom` | 25.0.1, 26.1.0 | 25.0.1 by `i18n`, `legal-ui`, `media-ui`, `onboarding`, `settings-ui`; 26.1.0 not traced |
| `vite` | 5.4.21, 6.4.3 | 6.4.3 by `app-shell`, `onboarding`, `settings-ui`; 5.4.21 not traced (transitive) |
| `esbuild` (+ 25 `@esbuild/*` binaries) | 0.21.5, 0.25.12, 0.28.1 | 0.28.1 by `doc-render`, `legal-ui`, `media-ui`, `pdf-ui`; 0.21.5 and 0.25.12 not traced (transitive, likely via vite 5 and mermaid tooling) |
| `commander` | 7.2.0, 8.3.0, 15.0.0 | all three under `doc-render` (transitive) |
| `katex` | 0.16.47, 0.18.11 | under `doc-render` (transitive) |
| `d3-shape`, `d3-array`, `d3-path`, `internmap` | two or three each | under `doc-render` (transitive, mermaid) |
| `undici-types` | 6.21.0, 7.18.2 | 6.21.0 by `legal`, `license`; 7.18.2 by `qa`, `qa-chrome`, `theme` |
| `aria-query` | 5.3.0, 5.3.2 | 5.3.2 by `i18n`, `legal-ui`, `onboarding`, `settings-ui`; 5.3.0 by `media-ui` |
| `dom-accessibility-api` | 0.5.16, 0.6.3 | 0.6.3 by `i18n`, `legal-ui`, `onboarding`, `settings-ui`; 0.5.16 by `media-ui` |
| `tinyexec` | 0.3.2, 1.3.1 | 1.3.1 under `doc-render`; 0.3.2 not traced |
| `cose-base`, `layout-base` | two each | not traced (transitive, mermaid) |
| `rrweb-cssom` | 0.7.1, 0.8.0 | not traced (transitive, jsdom) |

Private packages in the lock: `smol-toml@1.7.0` (single version, `pnpm-lock.yaml:2491`), `ajv@8.17.1` (single version, `:1504`). Neither is duplicated.

### K. Ranked action list

Ordered by value over risk. "Packages affected" names the packages and crates.

1. **Fix the addon TOCTOU (security).** Read the `.node` once, verify its digest from that buffer, and load from a verified copy. Packages: `build-control-broker` (`windows-job.mjs:25-26`, `macos-resource.mjs:25-26`). Risk: medium (native loading path; test on both hosts).
2. **Wire ownership-approval tests into a gate.** Add a root script that runs `ownership-approval/**/*.test.mjs` and the conformance test. Packages: root `package.json`, `ownership-approval`. Risk: low.
3. **Declare or remove undeclared cross-package imports.** `build-control-policy` (`index.mjs:4`, `install-*.mjs:2`), `build-control-clients` (`index.mjs:1`, `storage-cli.mjs:4-5`), `build-control-broker` (`broker.mjs:2` and five more). Either declare workspace dependencies or move shared code into `build-control-core`. Risk: medium.
4. **Delete `build-control-staging`** and its unused core dependency; fold any remaining logic into the broker. Risk: low.
5. **Archive `build-control-phase0`** after copying its evidence to `docs/`. Remove root scripts (`package.json:26-27`) and catalog entries. Risk: low (catalog regeneration needed).
6. **Remove duplicate `background-process.mjs`** from `build-control-policy`; import from `@rightkit/release` or move to core (G19). Risk: low.
7. **Remove duplicate `ownership-availability.mjs`, catalogs and versions** from `build-control-policy` and `build-control-clients`; consume generated outputs (G22). Risk: low to medium (sync must run).
8. **Consolidate Rust atomic writes and locks onto `rightkit-fs`** (G1, G2; H2). Replace raw `libc::flock` in `rightkit-service/src/discovery.rs:288`. Packages: `rightkit-fs`, `rightkit-service`, `rightkit-settings`, `rightkit-logs`, `rightkit-pdf`, `rightkit-shell`, `rightkit-models`. Risk: medium (lock semantics).
9. **Consolidate Windows job containment into `rightkit-process`** (G3). Packages: `rightkit-process`, `rightkit-sandbox`, `rightkit-automation`. Risk: medium (Windows-only; needs Windows build).
10. **Route ONNX session creation and `ORT_DYLIB_PATH` through `rightkit-ort`** (G9). Packages: `rightkit-vad`, `rightkit-asr`, `rightkit-qts`, `rightkit-ocr`, `rightkit-tts`. Risk: medium (ort is pinned at `=2.0.0-rc.13`; change is behavioural).
11. **Collapse `rightkit-models` to one stack** (G30, H3). Packages: `rightkit-models` and its consumers `cutright-model-store`, `genright-*`. Risk: high (live model installs; migration required).
12. **Route all HTTP through `rightkit-http`** (G6). Packages: `rightkit-logs`, `rightkit-models`, `rightkit-mcp`. Risk: medium.
13. **Retire the zero-consumer prototypes:** `rightkit-computer`, `rightkit-overlay`, `rightkit-align`, `rightkit-crash` (into logs), `rightkit-search` (into db). Risk: low once app grep is confirmed.
14. **Decide the PDF and OCR owner** (G26, G27). ViewRight still carries `crates/pdf` and `crates/ocr`. Packages: `rightkit-pdf`, `rightkit-ocr`, `viewright`. Risk: high (product code in ViewRight).
15. **Fold `rightkit-tauri` into `rightkit-shell`** and `@rightkit/tauri` into `@rightkit/shell` (G24). Risk: medium (version bumps across 6 apps).
16. **Move `agent-room-mcp` credentials to `rightkit-secrets`** and decide whether to replace its TS MCP handler with `rightkit-mcp` (G13, G32). Risk: medium.
17. **Hard-coded `/Volumes/D` paths:** parameterise `storage.mjs:12`, `cache-proof-cli.mjs:90`, `install-right-ctl.sh:8`. Risk: low.
18. **Swift doc and code drift:** fix `swift/README.md` table (`RightKitLicensing`, `RightKitQA`), test count, publication version; resolve `LicenseController: ObservableObject` (`LicenseClient.swift:860`) against the README rule; replace the duplicated `license-v2.json` in the Tests folder with a reference. Risk: low.
19. **Consolidate Node hash, canonical JSON and parseArgs helpers** (G20, G21) into `build-control-core`. Risk: low.
20. **Hook runtime:** make `build-control-policy` use `@rightkit/hooks` (G23). Risk: medium.

---

## 7. Not done / limitations

- Rust per-crate A-F sections are owned by other batches; this report covers only the cross-cutting G-I-K view for Rust.
- Transitive Rust counts and Cargo.lock duplicates (J for Cargo) are batch 4's scope.
- The app-repo text search for zero-consumer crate names (`rightkit-llm`, `rightkit-search`, `rightkit-pdf` etc.) did not finish; the manifest-level search is the basis for I.
- Swift was not built or tested (requires macOS toolchain runs outside this read-only audit).
- Node tests were not executed.
- Native addon behaviour, the TOCTOU finding, and the Windows job consolidation need a host run on both platforms before any change.
