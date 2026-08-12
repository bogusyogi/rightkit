# RightKit Cargo Control Plane V4

Status: superseded by `2026-08-12-rightkit-build-control-plane-v5.md`; do not execute
Platforms: macOS 13+; Windows 11 Pro+
Scope: every Cargo/rustup/rustc/toolchain invocation, every workspace, every user context on each enrolled machine

## 1. Decision

RightKit becomes sole Rust toolchain execution authority. `cargo`, `rustup`, `rustc`, `rustdoc` & registered `cargo-*` tools visible to users are signed clients, never toolchain executables. Clients submit requests to a privileged local broker. Broker validates storage, policy, workspace, toolchain, capacity, configuration, leases, & provenance before executing real Cargo/toolchain programs.

Real Cargo/rustup/toolchain binaries are installed in service-owned storage. Interactive users, shells, IDEs, agents, schedulers, package scripts, & absolute paths cannot execute them directly. Native OS execution policy blocks copied, renamed, downloaded, or alternate Rust toolchain binaries. If broker, policy driver/extension, external storage, registry, or attestation is unhealthy, toolchain execution fails closed.

This is one cross-platform contract with native enforcement adapters. OS-agnostic does not mean pretending macOS & Windows expose identical security primitives.

## 2. Guarantees

1. No real Cargo/rustup/rustc/toolchain command runs outside broker-authorized pipeline.
2. No Rust build/cache payload writes to internal OS storage.
3. Missing, replaced, disconnected, full, or unhealthy external storage fails closed.
4. Cache growth is physically bounded, concurrency-safe, measurable, recoverable, & cost-aware.
5. New workspaces require no repository-specific cache configuration.
6. Toolchain/config/CLI overrides cannot redirect storage or bypass policy.
7. Migration never deletes last validated copy.
8. Rollout cannot break current release flows before each app passes cutover gates.
9. Recovery remains inside pipeline; no environment-variable bypass exists.
10. Every guarantee has machine-verifiable evidence on macOS & Windows.

## 3. Threat model

### In scope

- Accidental direct Cargo/rustup/rustc/toolchain execution.
- Absolute paths, copied binaries, renamed binaries, hard links, shell aliases, executable Cargo aliases, `env -i`, scrubbed environments, nested shells, `rustup run`, `cargo +toolchain`, IDE servers, SSH, tmux, launchd, Scheduled Tasks, services, npm/pnpm/Tauri scripts, & delegated agents.
- Project Cargo configuration, environment, CLI overrides, build scripts, proc macros, native build tools, package-manager caches, & temporary paths attempting internal writes.
- Concurrent builds, PID reuse, broker crash, stale leases, corrupt registry, low space, full cache, storage disconnect, mount races, drive-letter changes, volume replacement, interrupted migration, & interrupted GC.
- User downloading another official Cargo/rustup distribution.

### Security boundary

- Enrolled machine runs enforced OS application-control policy.
- Local administrator can deliberately uninstall security controls. Such change marks machine non-compliant; no Cargo request is accepted until policy attestation is restored.
- Arbitrary malicious kernel code or physical compromise is outside scope.
- Arbitrary project source is treated as untrusted for filesystem writes; Cargo process trees receive bounded write authority.

## 4. Components

```text
shell / IDE / agent / scheduler / release
                 |
        signed cargo/rustup client
                 |
       authenticated local IPC request
                 |
     RightKit Cargo Broker (privileged)
       |       |        |         |
   policy   storage   registry   admission
   adapter  adapter   + leases   + quota
       |
  constrained build runner
       |
 service-owned Cargo -> rustc -> children
```

### Shared core

- Versioned policy schema.
- Request normalization & digest.
- Cargo/rustup argument parser.
- Workspace resolution.
- Cargo-config precedence scanner.
- Toolchain resolver.
- Cache key/class policy.
- Registry, leases, reservations, receipts, reconciliation, GC, migration, doctor, audit & explain logic.
- Cross-platform conformance fixtures.

### Native adapters

| Concern | macOS | Windows |
|---|---|---|
| Broker | signed LaunchDaemon + XPC | signed Windows Service + named pipe |
| Exec enforcement | Endpoint Security system extension using authorization events | App Control for Business enforced policy + NTFS ACL |
| Broker identity | code-signing requirement + XPC audit token | service SID + client PID/token/signature validation |
| Process containment | dedicated service identity + Endpoint Security write policy + process group | restricted service token + ACL + Job Object |
| External identity | APFS volume UUID + device/mount verification | volume GUID + filesystem serial + device identity |
| Hard cache bound | dedicated quota-limited APFS volume | maximum-size VHDX containing NTFS cache volume |
| Scheduler | LaunchDaemon timer | Task Scheduler/service timer |
| Durable log | Unified Log + external receipts | Event Log + external receipts |

### Control-plane state machine

| State | Real Cargo allowed? | Claim |
|---|---|---|
| `BOOTSTRAP` | legacy path only while installer inventories | no enforcement claim |
| `OBSERVE` | broker + audited legacy routes | evidence collection only |
| `CUTOVER` | broker for promoted workspaces; audited legacy for remaining workspaces | scoped per-workspace claim |
| `ENFORCE` | broker only under active native policy | full contract |
| `RECOVERY` | broker recovery request only | external-only recovery contract |

State transitions require signed migration receipt & doctor proof. Downgrade from `ENFORCE` requires authenticated admin recovery, enters `RECOVERY`, never `OBSERVE`. Audit-only OS policy is acceptable only in `OBSERVE`/`CUTOVER`; it is fatal in `ENFORCE`.

## 5. Bootstrap trust

Bootstrap must not depend on repository checkout or local Cargo.

1. Signed installer ships prebuilt broker, clients, native policy adapter, migration tool & embedded release public key.
2. macOS package is universal; Windows package includes x64 & arm64 payloads selected by installer.
3. Installer verifies platform, admin authority, external-storage capability & existing application-control compatibility.
4. Installer creates service identity, service-owned runtime root, local machine key & A/B runtime slots.
5. Local machine key lives in System Keychain on macOS or TPM/DPAPI-protected machine storage on Windows.
6. Runtime manifests are signed by RightKit release key. Local request receipts are authenticated by machine key.
7. Updates install inactive slot, verify signature/hash/schema, run doctor, atomically switch pointer, retain one rollback slot.
8. Existing `~/bin/cargo`, repository scripts, Homebrew Cargo/rustup/sccache & PATH shims remain shadow-only until native broker passes.

No installed client may execute code from a mutable Git working tree.

## 6. System-wide execution enforcement

### Public command contract

- `cargo ...` invokes signed RightKit client & preserves normal Cargo CLI.
- `rustup ...` invokes signed RightKit client; mutation commands become broker operations.
- Direct `rustc`, `rustdoc`, clippy/formatter drivers & registered `cargo-*` tools route through broker scratch/tool classes or are denied.
- `right cargo -- ...` is explicit equivalent.
- Client cannot launch Cargo. It only submits request & streams broker output.
- Request contains caller identity, client signature/hash, cwd, canonical argv, sanitized environment allowlist, workspace identity, requested toolchain, terminal mode, policy version, nonce & timestamp.
- Broker rejects replay, unknown client, unsigned client, stale nonce, client/binary mismatch, unregistered caller context or mutable client path.
- Nested Cargo calls receive broker-created child capability over inherited private IPC handle. They join parent request, target lease & reservation; recursive public-client admission is rejected.

### macOS enforcement

1. Real toolchains live under root/service-owned external runtime with no ordinary-user read/write authority.
2. Endpoint Security extension subscribes to execution authorization events.
3. Real Cargo/rustup/rustc proxies are allowed only for broker-created runner identity matching live request digest, PID/start identity & expiry.
4. Copies, renamed files, hard links, alternate rustup homes, downloaded Cargo distributions & execution from user-writable paths are denied by signing/hash/path policy.
5. Strict policy uses executable allowlisting for unapproved developer tools; pipeline-created binaries receive request-scoped execution grants, not global trust.
6. Endpoint Security authorization covers broker process-tree writes: build payload may write only registered external source-stage, cache, artifact & scratch roots.
7. Extension missing, disabled, unapproved, late, overloaded or unhealthy causes broker refusal.
8. Installation is incomplete until Endpoint Security entitlement, system-extension approval & enforced-policy self-test pass.

### Windows enforcement

1. App Control for Business runs user-mode code integrity in enforced mode.
2. Policy allows RightKit signed client/broker plus service-owned toolchain paths. Rust publisher trust is never granted globally.
3. Toolchain directory permits operational execute/read only to RightKit service SID. SYSTEM retains installer/update authority; administrator & ordinary-user tokens receive no operational access.
4. Protected-path App Control rules apply only to service-owned, non-user-writable locations. Exact Cargo copied elsewhere is denied.
5. Downloaded, renamed, hard-linked or alternate Cargo/rustup outside managed location is denied by App Control baseline.
6. Broker service runs with restricted token; only named-pipe clients matching allowed user/logon SID & signed client image may connect.
7. Build descendants remain in Job Object; service token has no internal payload-write authority.
8. App Control audit/disabled state, writable toolchain ACL, policy drift or service-policy mismatch causes broker refusal.

### Direct-execution acceptance

Tests must prove all routes below fail with OS-policy denial or enter broker with matching receipt:

- PATH Cargo/rustup.
- Absolute managed Cargo/rustup.
- Copy, rename, hard link & symlink.
- Fresh official rustup/Cargo download.
- `env -i`, altered HOME, altered PATH & scrubbed process.
- `cargo +toolchain`, `rustup run`, toolchain files & directory overrides.
- Shell function, alias, executable Cargo alias, nested shell & script interpreter.
- Homebrew/winget/scoop/choco-installed Rust.
- IDE, Rust Analyzer, Tauri, npm, pnpm, SSH, stale tmux, launchd, Scheduled Task, Windows Service, Codex/Claude agent & CI runner.
- Broker stopped, policy stopped, policy audit-only, storage absent & registry corrupt.

## 7. Broker pipeline

For every command, including light commands:

1. Authenticate client & caller.
2. Verify control-plane state. In `ENFORCE`, native execution policy must be active with exact expected enforced digest; rollout states bind request to scoped audit receipt.
3. Resolve external storage identity to current mount, never configured name/drive letter.
4. Open stable volume/root handle; verify filesystem, device, ownership, writability, quota volume & free space before path creation.
5. Normalize Cargo/rustup command & toolchain selection.
6. Resolve workspace with parser plus broker-controlled `cargo locate-project --workspace` validation.
7. Scan effective Cargo configuration & reject forbidden storage/compiler/alias keys.
8. Snapshot/synchronize workspace into external service-owned build stage, including uncommitted files while excluding cache/artifact namespaces.
9. Choose class, target, build, install, source, sccache, package-manager, temp & artifact paths.
10. Acquire shared-cache lease, per-target lease & heavy-work reservation where applicable.
11. Create constrained runner with sanitized environment & write boundary.
12. Execute selected real Cargo; stream output with original-path diagnostic remapping.
13. Monitor volume identity, quota, free floor, runner tree & policy health continuously.
14. On disconnect/I/O/policy loss, terminate process tree, record poisoned entry, forbid retry/fallback & quarantine after volume returns.
15. Record command, result, actual growth, high-water usage, duration, toolchain & output manifest.
16. Release reservations/leases in `finally`.
17. Reconcile & run bounded cost-aware GC.

`new` & `init` are broker-authorized source mutations executed in explicitly selected destination. Lockfile changes are returned as reviewed source patches; untrusted build descendants never receive arbitrary workspace-write access.

### User-session execution

Compilation always runs under constrained broker executor. Commands requiring logged-in user/session resources use split execution:

1. Broker compiles with Cargo JSON messages & captures exact executable/artifact manifest.
2. Broker authenticates manifest & grants read/execute, never write, to requesting user-session runner.
3. macOS LaunchAgent or Windows per-user signed runner launches artifact in caller session under process-tree policy.
4. Exit status, signals, stdio, debugger/terminal mode & cancellation return through original client.
5. GUI/Tauri runs, UI tests & credential-dependent tests have explicit recipes; unsupported semantic translation fails before compilation.

Ordinary build/check/test semantics remain direct Cargo under service context where user-session access is unnecessary. Contract fixtures compare brokered argv, environment, exit status & produced artifacts with native Cargo for every supported command.

## 8. Workspace identity

Nearest `Cargo.toml` is insufficient.

1. Start from `--manifest-path` or cwd.
2. Parse Cargo TOML while walking ancestors.
3. Resolve explicit `package.workspace` links.
4. Select authoritative workspace manifest containing `[workspace]`; standalone package is its own root.
5. Validate using broker-owned real Cargo `locate-project --workspace` under light-command policy.
6. Canonicalize case, symlinks, UNC paths, Windows long paths & macOS volume case behavior.
7. Registry identity is machine ID + canonical workspace ID, not display path alone.

Worktrees receive distinct IDs. Rust Analyzer uses dedicated `analysis` class so it cannot serialize all interactive CLI builds or corrupt their incremental state.

## 9. Toolchain semantics

Normalize one precedence model for:

- `cargo +toolchain`.
- `rustup run <toolchain>`.
- `RUSTUP_TOOLCHAIN`.
- `rust-toolchain` & `rust-toolchain.toml`.
- rustup directory overrides.
- host triple & explicit target triple.
- pinned Cargo/rustc path requested by registered release recipes.

Resolved rustc commit, Cargo version, host/target triples & component set bind target key, registry entry & request receipt. Feature probes determine support for `build.build-dir`; unsupported toolchains receive version-specific generated config rather than global incompatible config.

Toolchain update requires measured transition reservation for current + candidate toolchain targets. Candidate runs contract suite before activation. Current becomes previous rollback toolchain; older targets become GC candidates only after soak & receipt.

## 10. Storage model

Each enrolled machine selects an external backend. macOS & Windows caches are never shared directly.

```text
<verified-external-root>/rightsuite/<machine-id>/
  runtime/{active,previous}/
  toolchains/
  cargo-home/{bin,registry,git}/
  cache-volume/
    targets/{dev,test,analysis,release,worktree}/
    sccache/
    scratch/
    recovery/
    quarantine/
  artifacts/{current,rollback}/
  state/{registry.sqlite,leases,reservations,receipts,policy}/
  logs/
```

Internal storage may hold only signed client, broker/service metadata, OS policy, external-volume locator & bounded native logs. Cargo payload is forbidden.

### Identity

- macOS resolves APFS UUID to current mountpoint dynamically, then verifies mounted filesystem UUID/device through system APIs. `/Volumes/D`, `/Volumes/D-1` & stale squat directories are irrelevant.
- Windows uses volume GUID path, filesystem serial & device identity. Drive letter is display-only.
- All path mutations are relative to verified open root handle; no `mkdir` occurs through unverified textual mount path.
- macOS mutations use directory-relative no-follow operations plus device/inode containment checks. Windows mutations use handle-relative paths, file IDs & reparse-point rejection. Symlink/junction/reparse traversal cannot escape managed root.
- Identity is rechecked before admission, target creation, artifact commit, GC mutation & periodically during build.

### Hard physical bound

- macOS cache namespaces live on quota-limited APFS volume.
- Windows cache namespaces live inside maximum-size VHDX on external storage.
- Broker refuses backend lacking enforceable hard maximum.
- Runtime, retained artifacts & receipts remain outside cache quota but inside separate measured budgets.

### Child environment

Broker owns `CARGO_HOME`, `RUSTUP_HOME`, `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`, `CARGO_INSTALL_ROOT`, `RUSTC_WRAPPER`, `SCCACHE_DIR`, `TMPDIR`/`TEMP`/`TMP`, minimal HOME/USERPROFILE, XDG roots, npm, pnpm, node-gyp, CMake, Tauri/native caches & known compiler temp variables.

Caller overrides are rejected unless exact registered paths. Cargo `--target-dir`, `--root`, `--config`, `[env]`, `build.*`, `install.root`, runner, rustc/wrapper & executable alias redirects are parsed before real Cargo runs.

## 11. Capacity proof

Fixed aspirational numbers are forbidden. Setup produces full-disk budget before migration.

Definitions:

```text
T = external filesystem usable bytes
P = measured protected non-cache data
A = retained artifact budget
R = recovery + migration rollback budget
F = required free-space reserve
X = toolchain-transition headroom
Cmax = T - P - A - R - F - X
```

Migration cannot proceed unless:

```text
Cmax >= measured steady-state cache working set + maximum active reservations
```

`P` comes from complete classified inventory, not subtraction assumptions. Unknown/unreadable paths make proof incomplete. Every preserved model, media asset, backup, repository, installer & user-data class appears in signed capacity receipt.

Current Mac baseline is 927 GiB total, 727 GiB used, 200 GiB free. No 300-GiB completion claim is accepted until inventory proves it. If required cache + reserve does not fit, broker requires larger external storage or lower evidenced retention; it does not manufacture space targets.

### Measured budgets

For each active workspace/class/toolchain:

- Clean-build peak.
- Warm-build working set.
- P50/P95/P99 growth.
- Build duration/rebuild cost.
- Required release rollback target.
- Toolchain transition duplication.

Per-workspace soft quota derives from measurements. Aggregate hard quota derives from capacity proof. First build uses conservative platform/stack default; admission may reserve entire remaining class allowance. Prediction never replaces hard quota.

## 12. Registry, leases & admission

SQLite uses WAL, `busy_timeout`, schema migrations & one mutation coordinator. Append-only authenticated receipts provide recovery source independent of database.

Entry records include machine/workspace/class/toolchain IDs, canonical paths, markers, sizes, high-water growth, timestamps, rebuild cost, lease identity, reservation, pin, artifact references & poison state.

Lease rules:

- Target writers take exclusive per-entry lease.
- Read-only metadata commands need no target lease but hold shared Cargo-home lease.
- Rust Analyzer uses exclusive analysis-target lease.
- Cargo-home/registry/Git/sccache consumers hold shared maintenance lease.
- GC/migration/update takes exclusive maintenance lease.
- Wait policy is bounded FIFO; interactive default 10 minutes, analysis default 2 minutes, release configurable by signed recipe.
- PID alone is invalid; identity includes PID + OS process-start token + executable identity + request digest.
- Crash recovery reclaims only after identity mismatch & receipt reconciliation.

Heavy admission is serialized. Reservation covers target, Cargo-home, sccache, scratch, staging, install & artifact growth. Available capacity subtracts active reservations. Live hard-quota threshold blocks new writers; emergency floor triggers graceful process-tree termination before filesystem exhaustion.

## 13. GC

GC is cost-aware, not fixed LRU alone.

Eviction score combines expiry, bytes, last use, measured rebuild time, download cost, toolchain status, release rollback value & active reservations. Typical order:

1. Crashed scratch & poisoned incomplete entries.
2. Expired test/worktree/analysis entries.
3. Expired recovery after validated rollback window.
4. Superseded releases beyond current + rollback.
5. Unreferenced registry/Git data.
6. sccache through native bounded eviction.
7. Idle dev targets last.

GC never touches active/shared lease, reservation, pin, current/rollback release artifact, current/previous toolchain transition target or durable evidence.

Deletion requires global GC lock, exclusive maintenance lease, registry transaction, double validation, same-volume rename to quarantine, durable receipt, asynchronous deletion, filesystem remeasurement & committed reclaimed-byte result. Error, unknown directory, path escape, unreadable entry, marker mismatch, size failure or identity uncertainty is hard failure.

Cache roots receive indexing exclusions: `.metadata_never_index` + Time Machine exclusion on macOS; Windows Search indexing exclusion on Windows. Security scanning exclusions are not automatic.

## 14. Inventory & migration

Inventory is generated, never hand-maintained.

Scanner covers:

- Every registered workspace plus bounded filesystem discovery for `Cargo.toml`.
- Every `target`, custom target/build/install directory & symlink bridge.
- Cargo/rustup/sccache homes from environment, configs, services, IDEs & schedulers.
- `.cache/*target*`, `.right-release`, every `.rightsuite-*`, dated backups, volume-root caches, heavy-work, native probes, installer backups, worktrees, dispatch roots & unknown cache-like namespaces.
- Internal & external volumes, source volume identity, size, owner, age, live process, rebuildability & artifact/evidence classification.

Migration phases:

1. Freeze Cargo through broker deny mode.
2. Produce immutable inventory + capacity proof.
3. Classify every path; unknown blocks deletion.
4. Install external runtime, broker-owned toolchains & sccache first.
5. Import shared Cargo-home/toolchains; validate hashes & probes.
6. Stage caches inactive. Same-volume paths rename; cross-volume paths use resumable checksummed copy + fsync.
7. Build registry from markers; never activate moved state directly.
8. Cut over one workspace/app; run contract suite; retain source quarantine.
9. Repeat until all registered surfaces pass.
10. Enforce OS policy.
11. Retain rollback window, then expire receipts safely.

No source is deleted before destination manifest, activation test & rollback receipt pass. Reformat/replacement flow registers new volume identity through signed admin workflow, re-provisions empty backend & imports only validated artifacts; old UUID never silently updates.

## 15. Recovery

- Registry corruption: stop admissions, preserve corrupt DB, replay receipts, scan entry markers, rebuild fresh DB, reconcile sizes, then resume.
- Broker update failure: atomic rollback to previous signed slot.
- Cache-volume corruption: quarantine volume, create new bounded volume, rebuild caches.
- External disconnect: terminate jobs, poison entries, require identity revalidation on return.
- Lost/replaced external drive: signed re-enrollment workflow; no internal fallback.
- Policy drift: deny all Cargo until reinstall/attestation passes.
- Stuck build: authenticated client cancellation terminates full process group/Job Object.

Break-glass is `right cargo recovery`, not environment flag. It requires local admin authentication, healthy OS execution policy, verified external storage, short-lived broker receipt & external recovery target. It may bypass normal cache reuse/GC scheduling; it cannot permit direct Cargo or internal writes.

## 16. Rollout

### Phase 0 — evidence

- Generate Mac + Windows inventories, full-disk budgets, current execution-route map & baseline tests.
- Freeze policy schema, threat model, ADRs & migration schema.

### Phase 1 — trusted runtime

- Build/sign universal macOS package & Windows x64/arm64 MSI.
- Install broker, clients, policy adapter, external toolchains, external sccache & bounded cache backend.
- Atomically replace repository-dependent shim; enforcement remains audit-only.

### Phase 2 — shadow

- Route public client through broker.
- Log every would-deny execution/write/config route.
- Keep current release roots temporarily registered as external legacy entries.
- Soak until zero unexplained routes.

### Phase 3 — per-app cutover

- Migrate one app/workspace behind contract suite.
- Promote only after clean/warm/release/IDE/interruption tests pass.
- Repeat across every suite app, worktree, agent & scheduler.

### Phase 4 — native enforcement canary

- macOS Endpoint Security & Windows App Control remain audit mode first.
- Compare audit events to broker receipts; unexplained Cargo identity blocks promotion.
- Enable enforcement on canary context, then full machine.

### Phase 5 — enforce & prune

- Freeze legacy Cargo execution.
- Verify direct-execution negative matrix.
- Quarantine legacy caches/toolchains.
- Prune only after rollback window.
- Run 72-hour concurrent-build/storage-disconnect soak.

Hub UI is phase 2 product work & never gates safety completion.

## 17. Verification matrix

Every row runs on macOS & Windows where applicable:

| Area | Required cases |
|---|---|
| Execution | PATH, absolute, copy, rename, hard link, download, package manager, env scrub, nested shell |
| Context | terminal, SSH, tmux, launchd/Task Scheduler, service, IDE, Rust Analyzer, Tauri, npm/pnpm, agents |
| Toolchain | `+toolchain`, rustup run, env override, toolchain file, directory override, update/rollback |
| Config | both Cargo config filenames, ancestor precedence, CLI config, aliases, env, target/build/install redirects |
| Workspace | root package, virtual workspace, member cwd, worktree, symlink, UNC/long path, case variants |
| Storage | absent, wrong identity, changed mount/letter, squat path, read-only, full, disconnect, reconnect, replacement |
| Concurrency | same target, distinct targets, analysis + CLI, GC + fetch, GC + sccache, PID reuse, crash recovery |
| Capacity | first build, underestimated build, simultaneous admission, transition 2x, quota hit, reserve-floor hit |
| Migration | same/cross volume, interruption each phase, checksum error, unknown path, rollback, stale inventory |
| Recovery | corrupt DB/WAL, missing receipt, poisoned target, broker crash/update rollback, policy drift |
| Writes | build script, proc macro, linker, npm/pnpm, node-gyp, CMake, installer & temp internal-write attempts |

## 18. Required evidence

Adversarial approval packet must contain current artifacts, not prose claims:

- `machine-inventory-macos.json` & `machine-inventory-windows.json`.
- `capacity-model-<machine>.json` with every protected non-cache class.
- Clean/warm/P99 build measurements per workspace/class/toolchain.
- OS policy source, compiled policy digest & live attestation.
- Direct-execution negative-test results.
- Internal-write trace proving zero Cargo payload.
- Broker request/receipt correlation report.
- Migration manifest + rollback simulation.
- Concurrent reservation/quota/disconnect test results.
- GC recovery & registry-rebuild results.
- 72-hour soak report.

## 19. Completion gates

- Every visible `cargo`/`rustup`/`rustc`/registered tool is signed RightKit client.
- OS denies every tested real/alternate toolchain route outside broker.
- Only broker service identity can access managed toolchains.
- Disabled/audit policy causes Cargo denial, not degraded operation.
- Fresh workspace works without local configuration & creates no local target.
- Cargo process tree produces zero internal payload writes.
- External storage identity changes never redirect through stale path/drive letter.
- Mid-build disconnect terminates job & poisons target without fallback.
- Full-disk capacity equation closes using classified inventory.
- Measured working set + reservations fits hard cache bound.
- Concurrent writers cannot exceed hard bound or reserve floor.
- Workspace root is stable from root/member/worktree invocation.
- Toolchain selection remains semantically identical through broker.
- Shared-cache maintenance cannot race consumers.
- All legacy namespaces are inventory-classified & migrated/quarantined.
- Current + rollback release artifacts survive GC.
- Registry rebuilds from markers + receipts.
- Recovery path remains brokered & external-only.
- Mac + Windows matrices pass from clean install through enforced mode.

## 20. RightKit implementation map

Current code is migration input, not final boundary:

- `packages/release/cargo-guard.mjs`: compatibility parser/client behavior; must stop spawning real Cargo.
- `packages/release/heavy-command.mjs`: replace internal default with broker reservations & external scratch.
- `packages/release/cache-policy.mjs`: migrate release entries into shared registry.
- `packages/release/cargo-contract.mjs`: replace environment-only ownership with authenticated broker receipt.
- New shared package: policy schema, request normalization, cache model & conformance fixtures.
- New macOS package: signed client, LaunchDaemon/XPC broker, Endpoint Security system extension & installer.
- New Windows package: signed client, Windows Service/named-pipe broker, App Control policy/installer & VHDX manager.

## 21. Primary references

- Cargo build/cache routing: https://doc.rust-lang.org/stable/cargo/reference/build-cache.html
- Cargo reference: https://doc.rust-lang.org/cargo/reference/
- Apple Endpoint Security authorization: https://developer.apple.com/documentation/endpointsecurity/monitoring-system-events-with-endpoint-security
- Apple Service Management: https://developer.apple.com/documentation/servicemanagement/
- Apple XPC: https://developer.apple.com/documentation/XPC
- Windows App Control rules: https://learn.microsoft.com/windows/security/application-security/application-control/app-control-for-business/design/select-types-of-rules-to-create
- Windows App Control enforcement rollout: https://learn.microsoft.com/windows/security/application-security/application-control/app-control-for-business/deployment/enforce-appcontrol-policies
- Windows named-pipe security: https://learn.microsoft.com/windows/win32/ipc/named-pipe-security-and-access-rights
