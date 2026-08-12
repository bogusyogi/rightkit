# RightKit Build Control Plane V5

Status: approved & frozen for Phase 0, revision 3; Phase-0 evidence work authorized now, product implementation gated on applicable `PASS`
Platforms: macOS 13+; Windows 11 Pro+
Scope: every build, test, generated executable & Rust toolchain invocation for one designated developer on each enrolled machine

## 1. Decision

RightKit becomes sole build execution authority. `cargo test`, `cargo build`, `cargo run`, `rustup`, `rustc`, `rustdoc` & registered `cargo-*` tools keep familiar CLI, but visible executables are signed RightKit clients, never real toolchains. Clients submit requests to privileged local broker. Broker validates storage, native policy, workspace, toolchain, capacity, configuration, leases & provenance before executing real build programs.

Strict mode is default-deny executable control, not Cargo-name detection. OS blocks every unapproved executable from user-writable locations. Broker grants request-scoped execution to exact signed clients, service-owned tools & build outputs recorded in live request. Therefore copied, renamed, downloaded, source-built or disguised Cargo is denied exactly like any other unapproved binary. C/C++, Go, Swift, Node native modules, CMake probes & generated test binaries must also enter RightKit or remain denied.

Real toolchains live in service-owned storage. Interactive users, shells, IDEs, agents, schedulers, package scripts & absolute paths cannot execute them directly. If broker, native policy, external storage, registry or attestation is unhealthy, RightKit admits no new work. Platform enforcement failures remain OS trust-boundary failures; V5 does not claim resistance to malicious administrator, kernel compromise or Apple/Microsoft enforcement defects.

Shared policy/core is cross-platform; enforcement adapters & completion gates are platform-native. Mac completion never waits for Windows, nor Windows for Mac.

## 2. Guarantees

1. While platform policy is attested `ENFORCE`, no unapproved executable starts; every real compiler, build, test & generated program requires broker authorization.
2. No build/cache/stage payload writes to internal OS storage.
3. Missing, replaced, disconnected, full, or unhealthy external storage fails closed.
4. Cache growth is physically bounded, concurrency-safe, measurable, recoverable, & cost-aware.
5. New workspaces require no repository-specific cache configuration.
6. Toolchain/config/CLI overrides cannot redirect storage or bypass policy.
7. Migration never deletes last validated copy.
8. Rollout cannot break current release flows before each app passes cutover gates.
9. Recovery remains inside pipeline; no environment-variable bypass exists.
10. Each guarantee has machine-verifiable evidence on each platform before that platform ships.

## 3. Threat model

### In scope

- Accidental direct Cargo/rustup/rustc/toolchain execution.
- Absolute paths, copied binaries, renamed binaries, hard links, shell aliases, executable Cargo aliases, `env -i`, scrubbed environments, nested shells, `rustup run`, `cargo +toolchain`, IDE servers, SSH, tmux, launchd, Scheduled Tasks, services, npm/pnpm/Tauri scripts, & delegated agents.
- Project Cargo configuration, environment, CLI overrides, build scripts, proc macros, native build tools, package-manager caches, & temporary paths attempting internal writes.
- Concurrent builds, PID reuse, broker crash, stale leases, corrupt registry, low space, full cache, storage disconnect, mount races, drive-letter changes, volume replacement, interrupted migration, & interrupted GC.
- User downloading another official Cargo/rustup distribution.
- User compiling Cargo from source, renaming it, packing it into another binary or launching arbitrary generated executables.
- Non-Rust builds producing executable code outside RightKit.

### Security boundary

- Enrolled machine runs default-deny OS application-control policy. Unknown executables are denied; identification as “Cargo” is unnecessary.
- Local administrator can deliberately uninstall security controls. Such change marks machine non-compliant; no build request is accepted until policy attestation is restored.
- Arbitrary malicious kernel code or physical compromise is outside scope.
- Arbitrary project source is treated as untrusted for filesystem writes; build process trees receive bounded write authority.

### Non-goals

- Linux support.
- Multiple mutually distrusting developers on one machine; V5 supports one designated developer plus system contexts.
- Protecting receipts from local administrator; machine key proves pipeline continuity, not admin-proof history.
- Build-script read confidentiality beyond staged dependency closure & explicitly injected credentials.
- Admission control for native modules loaded by approved interpreters outside RightKit build requests on macOS; ES execution authorization does not govern `dlopen`. Such activity cannot write managed roots or satisfy build receipts.
- Network sandboxing; network access is recipe-controlled later, not V5 completion gate.
- Preserving ability to execute arbitrary freshly generated binaries outside RightKit.
- Detect-only or PATH-only mode as acceptable final product.

## 4. Components

```text
shell / IDE / agent / scheduler / release
                 |
         signed RightKit client
                 |
       authenticated local IPC request
                 |
     RightKit Build Broker (privileged)
       |       |        |         |
   policy   storage   registry   admission
   adapter  adapter   + leases   + quota
       |
  constrained build runner
       |
 service-owned build tool -> compiler -> children
```

### Shared core

- Versioned policy schema.
- Request normalization & digest.
- Cargo/rustup argument parser.
- Registered build-frontend adapters; Cargo is mandatory first adapter, followed by observed CMake, npm/pnpm/Tauri & inventory-required tools.
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
| Exec enforcement | Endpoint Security `AUTH_EXEC`, deny-by-default in-memory decisions | App Control for Business UMCI default-deny policy |
| Broker identity | code-signing requirement + XPC audit token | service SID + client PID/token/signature validation |
| Process containment | root broker + isolated non-login runner UID per live request + external stage ACL + process group; ES adds authorization/audit | restricted service token + external stage ACL + Job Object |
| External identity | APFS volume UUID + device/mount verification | volume GUID + filesystem serial + device identity |
| Hard cache bound | dedicated quota-limited APFS volume | maximum-size VHDX containing NTFS cache volume |
| Scheduler | LaunchDaemon timer | Task Scheduler/service timer |
| Durable log | Unified Log + external receipts | Event Log + external receipts |

Unified Log/Event Log are diagnostic rings. Authenticated external receipts are durable record.

### Control-plane state machine

| State | Real build tools allowed? | Claim |
|---|---|---|
| `BOOTSTRAP` | legacy path only while installer inventories | no enforcement claim |
| `OBSERVE` | broker + audited legacy routes | evidence collection only |
| `CUTOVER` | broker for promoted workspaces; audited legacy for remaining workspaces | scoped per-workspace claim |
| `ENFORCE` | broker only under active native policy | full contract |
| `RECOVERY` | broker recovery request only | external-only recovery contract |

State transitions require signed migration receipt & doctor proof. Downgrade from `ENFORCE` requires authenticated admin recovery, enters `RECOVERY`, never `OBSERVE`. Audit-only OS policy is acceptable only in `OBSERVE`/`CUTOVER`; it is fatal in `ENFORCE`.

## 5. Phase-0 feasibility gates

Phase 0 is first executable work in any “implement RightKit control plane” task, not prerequisite evidence expected before task begins. Missing Phase-0 artifacts at kickoff authorizes producing them; it does not block Phase-0 execution. No Phase-1 product implementation starts until applicable platform report is `PASS`. Spike budget is 10 working days per platform; `FAIL` stops that platform, while external dependency yields typed `BLOCKED_EXTERNAL` report & preserves independent-platform progress.

### macOS stop/go

1. Submit Apple request for `com.apple.developer.endpoint-security.client` immediately.
2. Prove system-extension installation from signed universal app, `/Applications` activation, interactive approval, Full Disk Access & headless provisioning handoff. `PENDING_USER_APPROVAL` is explicit non-enforced state.
3. Build minimal `AUTH_EXEC` deny-by-default client. Handler performs bounded in-memory lookup only; no disk, IPC, network or logging lies on authorization path.
4. Stress with full suite + generated-process storm. Capture deadlines, response latency, queue depth, sequence gaps, restart behavior & implicit-allow events.
5. Kill/hang/upgrade extension during hostile exec attempts. Apple documents missed AUTH deadline as implicit allow; packet must quantify window & prove companion watchdog locks designated developer session before policy attestation can be restored.
6. Gate requires entitlement grant, zero missed deadlines during load & explicit user acceptance that macOS kernel’s documented implicit-allow behavior is platform trust boundary. Without all three, macOS cannot enter `ENFORCE` & V5 is not shippable there.
7. Prototype staged full Tauri release with Git/worktree provenance, external path dependencies, lockfile/fmt sync-back, path remapping & internal-write trace.
8. Measure APFS clone/reflink behavior across D source volume & quota volume; choose snapshot backend from evidence.
9. Prototype pre-provisioned pool of locked, non-login runner UIDs. Root broker allocates one UID per live request, drops supplementary groups/GID/UID before untrusted code, & grants write access only to that request’s stage/target/temp roots. Toolchains, shared package inputs, sibling roots, registry, receipts & policy state remain read-only or inaccessible; shared mutations occur through broker/sccache capability IPC.
10. Prove hostile `build.rs`, proc macro, compiler plugin & native child cannot write sibling/current/previous request roots, toolchains, shared caches, registry, receipts or policy state; cannot retain processes across teardown; & cannot exploit UID reuse after wipe/quarantine + generation change. Any failure blocks macOS architecture.
11. Apple entitlement response has no bounded wall-clock SLA. While pending, macOS Phase 0 is `BLOCKED_EXTERNAL` but Windows/shared work continues. Denial triggers recorded owner decision, using Phase-0 data, to write new permanent-`CUTOVER` scope ADR or abandon macOS strict product; degraded scope never activates implicitly.

### Windows stop/go

1. Deploy App Control default-deny UMCI policy in audit VM, then enforced sacrificial machine/ring with recovery policy.
2. Prove normal approved apps plus C/C++, Go, Swift where installed, Python native wheels, Node `.node` modules, Rust proc-macro DLLs & generated tests work only through request-scoped RightKit grants.
3. Prove copied/renamed/downloaded/source-built Cargo, arbitrary user-writable executable & script-host bypass are denied.
4. Prototype service identity, named-pipe authentication, staged Tauri release, external path dependencies, sync-back & VHDX hard limit.
5. Prototype broker-generated supplemental hash policy for immutable native modules, activate/attest it without reboot or global publisher trust, then prove copy-to-user-writable-path denial. ISG & Managed Installer remain disabled unless separate ADR proves equivalent scope.
6. Verify signing, policy update/rollback, boot recovery & non-Rust developer usability before Windows implementation begins.
7. Admit representative signed CLI, `~/.nvm` Node version & hash-only native module through self-service flow within five minutes each, without reboot or disabling policy.
8. Inventory full current Python/Node/Rust native-module ecosystem, then stress base + supplemental policy at 2x observed module count & update churn. Record rule count, policy bytes, compile/sign/deploy/refresh P50/P95/P99, memory/boot impact & measured failure/degradation ceiling. Gate requires policy size/rules at or below 50% of measured safe ceiling, activation P99 at or below 60 seconds & end-to-end admission at or below five minutes.

### Shared stop/go evidence

- Inventory every current build entrypoint & installed `cargo-*` tool, including observed `cargo-about`, `audit`, `deny`, `geiger`, `machete`, `outdated` & `sweep`.
- Exercise Cargo, SwiftPM, Node/pnpm/node-gyp, CMake, Make & raw-compiler adapters against one clean, incremental, test & generated-executable fixture each.
- Measure baseline CLI/IDE latency, clean/warm cache working sets, stage size & full-disk capacity.
- Freeze strict-default-deny ADR, staging ADR, user/session model, native policy sources & platform-specific acceptance.
- Produce spike report with `PASS`, `FAIL` or `BLOCKED`; no ambiguous completion.

## 6. Bootstrap trust

Bootstrap must not depend on repository checkout or local Cargo.

1. Signed installer ships prebuilt broker, clients, native policy adapter, migration tool & embedded release public key.
2. macOS package is universal; Windows package includes x64 & arm64 payloads selected by installer.
3. Installer verifies platform, admin authority, external-storage capability & existing application-control compatibility.
4. Installer creates service identity, service-owned runtime root, separate local grant/policy/receipt keys as platform requires & A/B runtime slots.
5. Grant/policy-signing keys are non-exportable & hardware-backed: Secure Enclave-backed System Keychain identity on supported macOS hardware, TPM-backed CNG identity on Windows. Missing hardware-backed signing blocks `ENFORCE`. Receipt key may use service-ACL-bound System Keychain or TPM/DPAPI machine storage.
6. Grant/policy private keys are usable only by signed broker/policy-adapter identity. Rotation uses active/next generations, bounded drain overlap, explicit revocation & recovery receipt; compromise enters `RECOVERY` & invalidates every live grant/policy generation. Signing is rate-limited & audited.
7. Runtime manifests are signed by RightKit release key. Local request receipts are authenticated by receipt key. Release signing keys never exist on enrolled machines.
8. Updates install inactive slot, verify signature/hash/schema, run doctor, atomically switch pointer, retain one rollback slot.
9. Existing `~/bin/cargo`, repository scripts, Homebrew Cargo/rustup/sccache & PATH shims remain shadow-only until native broker passes.

No installed client may execute code from a mutable Git working tree.
macOS package cannot advertise/install `ENFORCE` until Apple grants Endpoint Security entitlement. Entitlement denial has no degraded strict product: Mac remains `OBSERVE`, current external-cache relief may operate, & completion is `BLOCKED`.

## 7. System-wide execution enforcement

### Public command contract

- `cargo ...` invokes signed RightKit client & preserves normal Cargo CLI.
- `rustup ...` invokes signed RightKit client; mutation commands become broker operations.
- Direct `rustc`, `rustdoc`, clippy/formatter drivers & registered `cargo-*` tools route through broker scratch/tool classes or are denied.
- `right cargo -- ...` is explicit equivalent.
- `right build -- <frontend> ...` is generic explicit entrypoint; installed compatibility clients (`swift`, `cmake`, approved `make`, Node package runners & registered compilers) submit same typed request.
- Client cannot launch Cargo. It only submits request & streams broker output.
- Request contains caller identity, client signature/hash, cwd, canonical argv, sanitized environment allowlist, workspace identity, requested toolchain, terminal mode, policy version, nonce & timestamp.
- Broker rejects replay, unknown client, unsigned client, stale nonce, client/binary mismatch, unregistered caller context or mutable client path.
- Nested Cargo calls receive broker-created child capability over inherited private IPC handle. They join parent request, target lease & reservation; recursive public-client admission is rejected.
- Third-party `cargo-*` tools install only through broker. Registry pins package/source digest, executable hash, toolchain, command capabilities, filesystem needs & Cargo-spawn behavior. Unregistered tools are denied; registered tools execute as broker children.

Observed Cargo extensions receive explicit classes after Phase-0 behavior trace: `about`, `audit`, `deny`, `geiger`, `machete` & `outdated` begin as analysis with shared Cargo-home access; any observed target write upgrades to exclusive target writer. `sweep` is maintenance-only, requires exclusive maintenance leases & cannot race builds. Unknown behavior fails registration.

| User action | System result |
|---|---|
| `cargo test` | signed RightKit client is allowed; broker runs real Cargo + tests inside request |
| absolute real `cargo test` | OS denies executable before Cargo starts |
| copied/downloaded/rebuilt Cargo | OS default-deny rejects unknown executable |
| Cargo spawned by build tool | child capability joins parent request or denial |
| generated test/app outside request | OS denies |
| generated test/app inside live request | exact request grant/protected chamber permits execution |

### macOS enforcement

1. Real toolchains live under root/service-owned external runtime with no ordinary-user read/write/execute authority.
2. Endpoint Security extension subscribes to `AUTH_EXEC`; default result is deny.
3. Immutable signed OS/application allowlist permits normal non-build apps. User-writable/generated executables require live broker grant keyed by protected stage path, volume UUID, vnode identity, cdhash, request digest, parent/process identity, policy generation & expiry.
4. Real Cargo/rustup/rustc, registered tools & build outputs are allowed only inside matching live request. Copy, rename, hard link, alternate home, download or source rebuild has no grant & is denied.
5. Extension authorization path uses preloaded immutable snapshot; updates arrive out of band with atomic generation switch.
6. Root broker never executes untrusted build code as broker/service identity. It allocates locked non-login runner UID from fixed pool, creates request roots owned only by that UID, drops supplementary groups/GID/UID, then execs build tree. Runner sees toolchains/package inputs read-only; registry, receipts, policy, sibling stages/targets & other runner roots are inaccessible.
7. Extension missing, disabled, unapproved, unhealthy, sequence-gapped or deadline-missing marks attestation invalid, locks designated developer session & causes broker refusal.
8. Apple’s documented implicit-allow-on-missed-deadline remains explicit OS trust-boundary behavior, never described as RightKit fail-closed behavior.
9. Installation is incomplete until entitlement, system-extension approval, Full Disk Access, enforced self-test & stress spike pass.

Per-request UID is generation-bound. Teardown kills full process group, closes capabilities, removes ACLs, quarantines writable roots, proves no surviving processes/open handles, wipes root, then advances generation before UID reuse. Shared cache mutation is broker RPC or sccache capability, never direct runner write. Pool exhaustion queues request; it never reuses live UID.

Code signature, cdhash or grant signature is identity evidence, never sufficient authorization. Copied signed output loses protected path/vnode/request binding. V5 uses supported service identities, ACLs & Endpoint Security only; introduction of private/deprecated Seatbelt or `sandbox_init` APIs fails Phase 0 & requires new ADR.

### Windows enforcement

1. App Control for Business runs UMCI default-deny in enforced mode; script enforcement stays enabled.
2. Policy allows immutable signed OS/apps, RightKit client/broker & protected service-owned execution chambers. Rust publisher trust is never global.
3. Toolchain/runtime/chamber directories permit operational access only to broker service SID/SYSTEM. Administrator & ordinary-user tokens receive no operational access.
4. Broker writes exact generated executable into protected per-request chamber, then launches it under requesting user token inside Job Object. Chamber is destroyed after request. Copy elsewhere is denied.
5. Downloaded, renamed, hard-linked, source-built or alternate toolchain executable outside chamber is denied by default policy, regardless of name.
6. Broker runs restricted service; named pipe DACL + PID/token/image-signature checks bind designated user client.
7. Build descendants run from external stage, remain in Job Object & inherit service/stage ACL boundary. Job Object manages lifecycle, not file authorization.
8. App Control audit/disabled state, writable protected path, policy drift or service-policy mismatch causes broker refusal & designated-session lock.
9. Cargo build scripts, proc macros, linkers & test binaries must execute from protected request root without disabling App Control runtime filepath protection or granting global publisher/managed-installer trust. Phase-0 proof is mandatory; failure blocks Windows architecture rather than weakening default deny.
10. `.dll`, `.pyd`, `.node`, proc-macro DLLs & other native load images are executable policy objects. Broker verifies origin/digest, copies each into immutable service-owned package or request chamber, generates narrowly scoped supplemental hash rules, activates policy generation, attests it, then permits interpreter launch.
11. Locally built native modules remain bound to request chamber, protected path, exact digest & policy generation. Third-party modules installed for reuse live in immutable versioned package root. Changing any byte requires re-admission; copying module elsewhere does not inherit authorization.
12. ISG reputation & Managed Installer trust are off by default because trust can outlive exact broker request. Either mechanism needs separate adversarial ADR, measured revocation & proof it cannot authorize copied or unrelated payloads.

RightKit does not dynamically sign arbitrary generated code into machine-wide trust. Windows uses App Control policy plus protected path/chamber; macOS uses live ES grants. Any separately requested durable release signature uses offline release pipeline & release key, never machine grant key.

### Direct-execution acceptance

Tests must prove every route below fails with OS-policy denial or enters broker with matching receipt:

- PATH Cargo/rustup.
- Absolute managed Cargo/rustup.
- Copy, rename, hard link & symlink.
- Fresh official rustup/Cargo download.
- Cargo rebuilt from source, renamed, packed or launched from memory-backed/temp/user-writable location.
- `env -i`, altered HOME, altered PATH & scrubbed process.
- `cargo +toolchain`, `rustup run`, toolchain files & directory overrides.
- Shell function, alias, executable Cargo alias, nested shell & script interpreter.
- Homebrew/winget/scoop/choco-installed Rust.
- IDE, Rust Analyzer, Tauri, npm, pnpm, SSH, stale tmux, launchd, Scheduled Task, Windows Service, Codex/Claude agent & CI runner.
- Broker stopped, policy stopped, policy audit-only, storage absent & registry corrupt.
- Arbitrary C/C++/Go/Swift/Node/CMake generated executable outside request; same executable succeeds only inside request grant/chamber.

### Steady-state tool admission

Denial is actionable, not silent. Client, Hub & `right build denials` show event ID, denied identity/path, reason, required class & exact admission command without exposing secrets.

`right build admit-tool <path|package-spec>` performs authenticated self-service admission for designated developer:

1. Capture denial event or package specification; reject path races, symlinks/reparse points, running/mutating files & ambiguous bundles.
2. Identify publisher/notarization, package source, version, hash, dependencies, interpreters/native modules, update mechanism & requested capability class.
3. Copy verified payload into immutable service-owned versioned tool/package root. Original Homebrew, nvm, scoop, winget, download or user-writable path never becomes trusted execution location.
4. Run static policy checks plus isolated smoke fixture. Classify as UI app, runtime/interpreter, build frontend, build child, analysis tool, native module, maintenance tool or rejected unknown.
5. Generate minimal Mac allowlist generation or Windows signed supplemental policy; activate atomically, attest live digest & retain prior generation for rollback.
6. Emit admission receipt with provenance, exact hashes/publisher, capabilities, policy generation, expiry/update rule & uninstall transaction.
7. On failure, roll back payload + policy generation atomically; executable remains denied.

Build-capable executable cannot be admitted as generic CLI. It requires registered frontend adapter or child capability whose outputs, writes & descendants remain inside broker pipeline. Tool admission never creates direct Cargo/compiler/build-system bypass.

`~/.nvm` is discovery/import input only. Each Node version is installed via `right build tool install node@<version>` into service-owned runtime, pinned in frontend manifest & admitted with its native modules. nvm path churn never receives publisher/path trust.

Phase 0 requires signed CLI, nvm Node runtime & hash-only native module admission in five minutes each, without reboot, policy disable or global publisher/Managed Installer trust. Denials are reviewed at each login & at least daily during `OBSERVE`/`CUTOVER`; any unexplained event older than 24 hours blocks `ENFORCE`. In `ENFORCE`, denied command returns event immediately & remains denied until admission completes.

## 8. Broker pipeline

For every command, including light commands:

1. Authenticate client & caller.
2. Verify control-plane state. In `ENFORCE`, native execution policy must be active with exact expected enforced digest; rollout states bind request to scoped audit receipt.
3. Resolve external storage identity to current mount, never configured name/drive letter.
4. Open stable volume/root handle; verify filesystem, device, ownership, writability, quota volume & free space before path creation.
5. Select registered frontend adapter; normalize command, toolchain & declared capabilities.
6. Resolve workspace/source root using adapter rules plus frontend-native validation.
7. Scan effective frontend configuration & reject forbidden storage/compiler/alias keys.
8. Resolve complete local source closure, create immutable external stage & provenance manifest.
9. Choose class, target, build, install, source, sccache, package-manager, temp & artifact paths.
10. Acquire shared-cache lease, per-target lease & heavy-work reservation where applicable.
11. Create constrained runner with sanitized environment & write boundary.
12. Execute selected real frontend; stream output with original-path diagnostic remapping.
13. Monitor volume identity, quota, free floor, runner tree & policy health continuously.
14. On disconnect/I/O/policy loss, terminate process tree, record poisoned entry, forbid retry/fallback & quarantine after volume returns.
15. Record command, result, actual growth, high-water usage, duration, toolchain & output manifest.
16. Release reservations/leases in `finally`.
17. Reconcile & run bounded cost-aware GC.

`new` & `init` are broker-authorized source mutations executed in explicitly selected destination. Build descendants never receive arbitrary workspace-write access.

### Build-frontend adapter contract

Every build system uses same staging, storage, admission, child-exec, mutation & artifact-delivery contract. An adapter is signed code plus versioned declarative policy; repository scripts cannot define or widen one. Adapter emits normalized `BuildRequest` with:

- adapter ID/version, canonical argv, tool/runtime identity & source-root discovery proof;
- complete source/input closure plus config/lockfile provenance;
- declared mutable source files, external cache classes, stage-local output roots, temp roots & durable artifacts;
- child executable/native-module capabilities, session needs, network/secret recipe & denial rules;
- expected output manifest, sync-back policy, path remapping & conformance fixture ID.

Broker rejects unknown frontend, ambiguous root, in-source payload write, undeclared output, unbounded discovery, undeclared child load or mutable tool/runtime path. All compiler objects, `.build`, `build`, `target`, module caches, package caches, generated code, native modules & temp payloads remain in verified external stage/cache namespaces.

| Adapter | Root/closure | External output/cache routing | Required parity proof |
|---|---|---|---|
| Cargo/rustup | §9 workspace + TOML/path/Git closure | target/build/install, Cargo home, rustup, sccache, temp | build/check/test/run/release/tooling |
| SwiftPM | explicit `--package-path` or nearest `Package.swift`; resolved package graph, local dependencies, `Package.resolved`, plugins | broker-selected scratch/build path, module cache, artifact/plugin cache, temp | build/test/run/plugin, diagnostics, generated executable |
| Node/pnpm/Tauri/node-gyp | workspace/lockfile root; package graph, local workspaces, lifecycle/native inputs | external pnpm/npm stores, node-gyp cache, stage `node_modules`, native/Tauri targets, temp | install/build/test/Tauri release/native `.node` load |
| CMake | explicit source or nearest `CMakeLists.txt`; configure trace plus declared external inputs | mandatory out-of-source build tree, compiler/module/download caches, temp | configure/build/test/install/probe |
| Make | explicit signed recipe root + bounded input/write declaration; no inferred arbitrary tree | stage mirror; broker overrides permitted output/temp variables; in-source or undeclared writes fail | build/test/install plus recursive make |
| Raw compiler | explicit declared input closure & output set | every `-o`, depfile, module cache, object, library & temp path redirects into stage | compile/link/run, response files, child tools |

SwiftPM, Node/node-gyp, CMake, Make & raw-compiler adapters are Phase-0 hard-gate scope, not later aspirations. Newly observed build systems remain denied until adapter + parity fixture ships.

### Artifact delivery & execution

- Durable build artifacts stay under external broker artifact root by default & are addressed by receipt.
- Explicit user output such as `rustc -o ./foo` resolves logical destination before execution, writes stage-local candidate, validates manifest, then atomically promotes content to requested external source path only when captured destination content hash still matches. Internal destination, race or policy escape fails.
- Promotion preserves requested filename/mode only; receipt binds source digest, toolchain, adapter, output digest & destination identity. It never grants general execution.
- Running promoted/generated binary later creates new broker request. Broker revalidates digest/provenance & copies it into protected live request chamber; direct execution outside request remains denied.
- Install/package/release operations use explicit adapter delivery recipe. Signing or placement into durable application allowlist is separate release admission, never side effect of compile.

### Staging contract

Staging is write boundary on both platforms & cannot remain implementation detail.

1. Closure resolver TOML-parses workspace members, explicit `package.workspace`, target paths, recursively local `path` dependencies, patches/replacements & registered build inputs before Cargo executes. Existing lockfile validation uses broker-controlled `cargo metadata --locked`; offline is added when required index/package inputs are already present. Missing lockfile uses explicit resolution recipe in stage & syncs only resulting `Cargo.lock` through content-hash transaction. Escape, cycle, unreadable dependency or dependency outside approved external source roots fails closed.
2. Snapshot includes tracked, untracked & uncommitted source state except generated/cache/artifact exclusions. Manifest records source file ID, size, mode, cryptographic content hash & preimage generation. Sync-back concurrency decisions use content hash, never mtime.
3. Git provenance stage includes worktree-aware HEAD, index, refs, packed refs, shallow state, submodule metadata & required object closure. It never leaves `.git` file pointing outside stage. Fixture requires Git-aware build scripts to emit same commit/dirty/submodule provenance as source, including worktrees.
4. macOS backend uses `clonefile`/APFS clone only after Phase-0 cross-volume proof; otherwise content-addressed incremental copy. Windows uses content-addressed incremental mirror into external stage; VHDX stores cache, while stage may live on verified external source volume under service ACL.
5. Stage, Git metadata & source mirror are measured capacity classes outside target budgets but inside total external capacity proof. Active request lease prevents GC; abandoned stages use crash TTL.
6. Source mutation uses optimistic transaction against captured preimages:

| Command | Sync-back |
|---|---|
| metadata/resolution | only newly resolved `Cargo.lock`; existing lock validation is `--locked` & mutation-free |
| build/check/test/run/doc/bench | only `Cargo.lock` when Cargo legitimately creates/updates it |
| fmt/fix/clippy --fix | atomically apply exact patch when every touched preimage still matches |
| add/remove/update | atomically apply manifest + lockfile patch when preimages match |
| build script/proc macro/native child | never sync arbitrary source mutations |
| new/init | broker writes requested new source tree directly after destination validation |

Conflict leaves source unchanged, stores patch externally & exits with dedicated conflict code plus one-command retry. No silent overwrite & no mandatory human confirmation on conflict-free normal operation.

7. Stage paths are canonical pipeline paths. Rust receives deterministic `--remap-path-prefix` from stage to logical source ID. Diagnostics remap to caller paths. `file!`, debug info, provenance & release receipts are tested against declared pipeline semantics; byte identity with unmanaged native Cargo is not promised. RightKit pipeline artifact is canonical release artifact.
8. Credential/network material is explicit recipe input through broker secret handles. Stage service identity cannot read designated user home, SSH keys or unrelated repositories.
9. Initial/full & incremental snapshot latency, bytes & CPU are Phase-0 measurements. Promotion requires user-approved P99 budgets for metadata, Rust Analyzer, one-file check, clean build & release; regression tests freeze those budgets.
10. Full Tauri release fixture must prove correct Git stamp, external path dependency closure, dirty-tree identity, generated installer, diagnostics, cancellation & zero internal payload writes.

### User-session execution

Compilation always runs under constrained broker executor. Commands requiring logged-in user/session resources use split execution:

1. Broker compiles with Cargo JSON messages & captures exact executable/artifact manifest.
2. Broker authenticates manifest & grants read/execute, never write, to requesting user-session runner.
3. macOS LaunchAgent or Windows per-user signed runner launches artifact in caller session under process-tree policy.
4. Exit status, signals, stdio, debugger/terminal mode & cancellation return through original client.
5. GUI/Tauri runs, UI tests & credential-dependent tests have explicit recipes; unsupported semantic translation fails before compilation.

Ordinary build/check/test semantics remain real Cargo under service context where user-session access is unnecessary. Cargo-launched tests receive child execution grants. Contract fixtures compare argv, exit status, provenance, diagnostics & declared outputs against frozen pipeline semantics.

## 9. Workspace identity

Nearest `Cargo.toml` is insufficient.

1. Start from `--manifest-path` or cwd.
2. Parse Cargo TOML while walking ancestors.
3. Resolve explicit `package.workspace` links.
4. Select authoritative workspace manifest containing `[workspace]`; standalone package is its own root.
5. Validate using broker-owned real Cargo `locate-project --workspace` under light-command policy.
6. Canonicalize case, symlinks, UNC paths, Windows long paths & macOS volume case behavior.
7. Registry identity is machine ID + canonical workspace ID, not display path alone.

Worktrees receive distinct IDs. Rust Analyzer uses dedicated `analysis` class so it cannot serialize all interactive CLI builds or corrupt their incremental state.

## 10. Toolchain semantics

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

Toolchain/component install/update is signed broker recipe. Network profile explicitly supplies proxy, CA bundle, mirror, timeout & offline policy. Credentials arrive through platform secret handle; service never assumes designated user environment or home. Download hash/signature, unpack inventory & toolchain probes bind activation receipt.

## 11. Storage model

Each enrolled machine selects an external backend. macOS & Windows caches are never shared directly.

```text
<verified-external-root>/rightsuite/<machine-id>/
  runtime/{active,previous}/
  toolchains/
  cargo-home/{bin,registry,git}/
  cache-volume/
    targets/{dev,test,analysis,release,worktree}/
    sccache/
    stages/{sources,git,execution}/
    scratch/
    recovery/
    quarantine/
  artifacts/{current,rollback}/
  state/{registry.sqlite,leases,reservations,receipts,policy}/
  logs/
```

Internal storage may hold only signed client, broker/service metadata, OS policy, external-volume locator & bounded native logs. Build/cache/stage payload is forbidden.

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

## 12. Capacity proof

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

## 13. Registry, leases & admission

SQLite uses WAL, `busy_timeout`, schema migrations & one mutation coordinator. Append-only authenticated receipts provide recovery source independent of database.

Entry records include machine/workspace/class/toolchain IDs, canonical paths, markers, sizes, high-water growth, timestamps, rebuild cost, lease identity, reservation, pin, artifact references & poison state.

Lease rules:

- Target writers take exclusive per-entry lease.
- Read-only metadata commands need no target lease but hold shared Cargo-home lease.
- Rust Analyzer uses exclusive analysis-target lease.
- Cargo-home/registry/Git/sccache consumers hold shared maintenance lease.
- GC/migration/update takes exclusive maintenance lease.
- Wait policy is bounded priority-fair FIFO: cancellation/recovery, interactive, analysis, agent, release/background. Phase 0 freezes P99 & maximum waits per class from measured workloads.
- Timeout cancels only queued request, returns holder/request summary + retry token, & never kills unrelated holder.
- PID alone is invalid; identity includes PID + OS process-start token + executable identity + request digest.
- Crash recovery reclaims only after identity mismatch & receipt reconciliation.

Admission transaction is serialized; admitted builds run in parallel up to measured CPU/memory/I/O heavy-slot limit. Head-of-line bypass permits smaller independent request when reservations & fairness allow. `right build queue` exposes position, holder, age, reservation, cancellation & predicted start.

Reservation covers target, Cargo-home, sccache, scratch, staging, install & artifact growth. Available capacity subtracts active reservations. Live hard-quota threshold blocks new writers; emergency floor terminates selected process trees before filesystem exhaustion.

Broker generation binds every request, runner, lease & reservation. Broker upgrade/restart drains or terminates its generation; supervisor owns process groups/Job Objects, kills survivors, poisons partial targets & reconciles receipts before new admissions. Orphan continuation is forbidden.

One broker-owned sccache daemon runs per machine as service identity with external directory, native hard size limit, idle lifecycle & health probe. All consumers hold shared lease. Upgrade/GC takes exclusive lease, stops daemon, reconciles data & restarts before admission.

Light path performs no filesystem scan or synchronous durable-log write after registry warmup. Phase 0 sets P99 targets for client auth, `cargo metadata`, Rust Analyzer refresh & no-change snapshot; promotion fails on regression.

## 14. GC

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

## 15. Inventory & migration

Inventory is generated, never hand-maintained.

Scanner covers:

- Every registered workspace plus bounded filesystem discovery for `Cargo.toml`.
- Every `target`, custom target/build/install directory & symlink bridge.
- Cargo/rustup/sccache homes from environment, configs, services, IDEs & schedulers.
- `.cache/*target*`, `.right-release`, every `.rightsuite-*`, dated backups, volume-root caches, heavy-work, native probes, installer backups, worktrees, dispatch roots & unknown cache-like namespaces.
- Internal & external volumes, source volume identity, size, owner, age, live process, rebuildability & artifact/evidence classification.
- Homebrew, MacPorts, `~/.nvm`, npm/pnpm global bins, Python environments/native wheels, winget, scoop, Chocolatey, Visual Studio toolsets, SDKs, CMake/Make/Swift runtimes, `.dll`/`.pyd`/`.node` loads & every user-writable executable route.

Migration phases:

1. Freeze unmanaged build execution through broker deny mode.
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

## 16. Recovery

- Registry corruption: stop admissions, preserve corrupt DB, replay receipts, scan entry markers, rebuild fresh DB, reconcile sizes, then resume.
- Broker update failure: atomic rollback to previous signed slot.
- Cache-volume corruption: quarantine volume, create new bounded volume, rebuild caches.
- External disconnect: terminate jobs, poison entries, require identity revalidation on return.
- Lost/replaced external drive: signed re-enrollment workflow; no internal fallback.
- Policy drift: enter `RECOVERY`, lock designated developer session & deny all build admission until reinstall/attestation passes.
- Stuck build: authenticated client cancellation terminates full process group/Job Object.

Break-glass is `right build recovery`, not environment flag. It requires local admin authentication, healthy OS execution policy, verified external storage, short-lived broker receipt & external recovery target. It may bypass normal cache reuse/GC scheduling; it cannot permit direct toolchain/generated execution or internal writes.

## 17. Rollout

Each platform advances independently through these phases. Shared schema stays compatible; Mac may reach completion while Windows remains Phase 0.

### Phase 0 — feasibility & evidence

- Start here when implementation is requested. Absence of evidence is work queue, not refusal condition.
- Complete applicable §5 stop/go spikes.
- Generate platform inventory, full-disk budget, current execution-route map & baseline tests.
- Freeze strict policy, staging, user-session, threat-model & migration ADRs.
- Emit sealed implementation contract with owned paths, commands/checks, evidence producers, closed acceptance slice & typed per-platform report: `PASS`, `FAIL` or `BLOCKED_EXTERNAL`.
- `PASS` automatically authorizes Phase 1 for that platform within same implementation task. `FAIL` stops it. `BLOCKED_EXTERNAL` pauses only blocked platform & preserves shared/other-platform progress.

### Phase 1 — trusted runtime

- Build/sign universal macOS package or Windows x64/arm64 MSI for current platform.
- Install broker, clients, policy adapter, external toolchains, external sccache & bounded cache backend.
- Ship generic frontend adapter registry, immutable tool roots, self-service admission transaction & key rotation/recovery before any strict canary.
- Atomically replace repository-dependent shim; enforcement remains audit-only.

### Phase 2 — shadow

- Route public client through broker.
- Log every would-deny execution/write/config route.
- Keep current release roots temporarily registered as external legacy entries.
- Soak until zero unexplained routes.

### Phase 3 — per-app cutover

- Migrate one app/workspace behind contract suite.
- SwiftPM adapter + parity fixture must pass before first Swift-producing or Swift-consuming app cutover; current Swift release flow cannot remain legacy under `ENFORCE`.
- Promote only after clean/warm/release/IDE/interruption tests pass.
- Repeat across every suite app, worktree, agent & scheduler.

### Phase 4 — native enforcement canary

- Endpoint Security or App Control remains audit mode first.
- Compare every generated/unapproved exec event to broker receipts; unexplained identity blocks promotion.
- Enable enforcement on canary context, then full machine.

### Phase 5 — enforce & prune

- Freeze legacy/unapproved executable execution.
- Verify direct-execution negative matrix.
- Quarantine legacy caches/toolchains.
- Prune only after rollback window.
- Run 72-hour concurrent-build/storage-disconnect soak.

Hub UI is phase 2 product work & never gates safety completion.

### Effort bound

Phase 0 is capped at 10 working days per platform. After both feasibility reports, owner approves/rejects new estimate. Planning envelope: shared broker/cache/staging core 8–12 engineer-weeks, macOS adapter/package 6–10, Windows adapter/package 6–10, migration/soak 4–8; approximately 24–40 engineer-weeks total. Any required kernel/minifilter/private-API work exceeds V5 & triggers new ADR instead of silent expansion.

## 18. Verification matrix

Every row runs on macOS & Windows where applicable:

| Area | Required cases |
|---|---|
| Execution | PATH, absolute, copy, rename, hard link, download, package manager, env scrub, nested shell |
| Default deny | source-built/packed Cargo, arbitrary generated binary, approved non-Rust build, copied chamber output |
| Context | terminal, SSH, tmux, launchd/Task Scheduler, service, IDE, Rust Analyzer, Tauri, npm/pnpm, agents |
| Toolchain | `+toolchain`, rustup run, env override, toolchain file, directory override, update/rollback |
| Config | both Cargo config filenames, ancestor precedence, CLI config, aliases, env, target/build/install redirects |
| Workspace | root package, virtual workspace, member cwd, worktree, symlink, UNC/long path, case variants |
| Staging | Git/dirty/worktree/submodule stamp, external path dependency, sync conflict, remap, latency, capacity, active-stage GC |
| Storage | absent, wrong identity, changed mount/letter, squat path, read-only, full, disconnect, reconnect, replacement |
| Concurrency | same target, distinct targets, analysis + CLI, GC + fetch, GC + sccache, PID reuse, crash recovery |
| Capacity | first build, underestimated build, simultaneous admission, transition 2x, quota hit, reserve-floor hit |
| Migration | same/cross volume, interruption each phase, checksum error, unknown path, rollback, stale inventory |
| Recovery | corrupt DB/WAL, missing receipt, poisoned target, broker crash/update rollback, policy drift |
| Writes | build script, proc macro, linker, npm/pnpm, node-gyp, CMake, installer & temp internal-write attempts |
| macOS isolation | hostile build child vs sibling stage/target, registry, receipts, policy, toolchain/shared-cache writes, survivor process/open handle, UID generation reuse |
| Runtime | third-party `cargo-*`, sccache lifecycle, broker-generation restart, nested Cargo, cancellation, GUI/user-session test |
| Frontends | Cargo, SwiftPM, Node/pnpm/Tauri/node-gyp, CMake, Make, raw compiler; root/closure, cache/output/temp routing, child load, artifact delivery |
| Tool admission | signed CLI, Homebrew/package-manager update, nvm Node version churn, Python wheel, `.node`/`.pyd`/DLL, rollback, expiry, denied-event UX, five-minute SLA |
| macOS policy | entitlement absent/granted, approval pending, extension killed/slow/upgraded, deadline miss & sequence gap |
| Windows policy | audit/enforce/rollback, service/chamber ACL drift, App Control base/supplemental policy update, native-module load/copy, 2x ecosystem catalog scale/churn, ISG/Managed Installer absent, recovery boot |

## 19. Required evidence

Adversarial approval packet must contain current artifacts, not prose claims:

- `machine-inventory-macos.json` & `machine-inventory-windows.json`.
- `capacity-model-<machine>.json` with every protected non-cache class.
- Clean/warm/P99 build measurements per workspace/class/toolchain.
- OS policy source, compiled policy digest & live attestation.
- Apple entitlement disposition + activation/approval evidence, or Windows App Control recovery evidence.
- macOS runner-UID allocation/drop/teardown/rotation trace + hostile sibling/control-root escape results.
- Direct-execution negative-test results.
- Internal-write trace proving zero build/cache/stage payload.
- Broker request/receipt correlation report.
- Migration manifest + rollback simulation.
- Concurrent reservation/quota/disconnect test results.
- GC recovery & registry-rebuild results.
- 72-hour soak report.
- Full staged Tauri release with Git/worktree provenance & path-dependency evidence.
- Approved third-party tool inventory + policy tests.
- Frontend-adapter manifests + parity fixtures for Cargo, SwiftPM, Node/node-gyp, CMake, Make & raw compilers.
- Tool/native-module admission SLA, supplemental-policy activation/rollback & copied-module denial results.
- Windows catalog-scale report: observed + 2x module count, rule/policy size, safe ceiling, update churn, activation P50/P95/P99, boot/memory impact & end-to-end admission.
- Grant/receipt key generation, non-exportability, rotation, revocation & compromise-recovery drill.
- Signed owner acceptance of permanent application-whitelisting operations: admission friction, daily denial triage, policy updates, key recovery & ongoing maintenance cost.

## 20. Completion gates

- Gate is evaluated per platform; other platform status cannot block or satisfy it.
- Typed `cargo test`/`cargo build` resolves signed RightKit client & completes through broker with receipt.
- Every visible `cargo`/`rustup`/`rustc`/registered tool is signed RightKit client.
- Native default-deny policy blocks real/alternate/source-built Cargo & every unapproved generated executable outside live broker request.
- Only broker service identity can access managed toolchains.
- `ENFORCE` requires exact live policy digest. Audit/disabled/drift state admits no broker work & locks designated developer session when detected.
- macOS requires granted ES entitlement, approved extension, zero load-test deadline misses & recorded acceptance of documented implicit-allow OS boundary.
- macOS hostile build children cannot write sibling/control/shared roots or survive request/UID-generation teardown.
- Windows requires enforced App Control UMCI policy, protected chamber, catalog size no greater than 50% measured safe ceiling, policy activation P99 no greater than 60 seconds, end-to-end admission no greater than five minutes & successful recovery-policy drill.
- Fresh workspace works without local configuration & creates no local target.
- Build process tree produces zero internal build/cache/stage payload writes.
- External storage identity changes never redirect through stale path/drive letter.
- Mid-build disconnect terminates job & poisons target without fallback.
- Full-disk capacity equation closes using classified inventory.
- Measured working set + reservations fits hard cache bound.
- Concurrent writers cannot exceed hard bound or reserve floor.
- Workspace root is stable from root/member/worktree invocation.
- Staging preserves Git/dirty/worktree provenance, external path dependencies & conflict-safe source mutations within frozen latency budget.
- Toolchain selection remains semantically identical through broker.
- Shared-cache maintenance cannot race consumers.
- All legacy namespaces are inventory-classified & migrated/quarantined.
- Current + rollback release artifacts survive GC.
- Registry rebuilds from markers + receipts.
- Recovery path remains brokered & external-only.
- Registered third-party `cargo-*` tools & broker-owned sccache pass lifecycle tests.
- Every mandatory frontend passes staging/output/cache/native-child parity suite with zero internal payload writes.
- Signed CLI, nvm Node & native module self-admit within five minutes without reboot, policy disable or broadened trust.
- Grant signature/hash alone cannot authorize copied output; key non-exportability, rotation, revocation & compromise recovery pass.
- Owner records acceptance of permanent whitelisting/admission operations before `ENFORCE`.
- Current platform matrix passes from clean install through enforced mode.

## 21. RightKit implementation map

Current code is migration input, not final boundary:

- `packages/release/cargo-guard.mjs`: compatibility parser/client behavior; must stop spawning real Cargo.
- `packages/release/heavy-command.mjs`: replace internal default with broker reservations & external scratch.
- `packages/release/cache-policy.mjs`: migrate release entries into shared registry.
- `packages/release/cargo-contract.mjs`: replace environment-only ownership with authenticated broker receipt.
- New shared package: policy schema, request normalization, cache model & conformance fixtures.
- New shared staging/sync package: dependency closure, Git provenance, snapshot, remapping & transactional sync-back.
- New signed clients plus versioned adapters for Cargo, SwiftPM, Node/pnpm/Tauri/node-gyp, CMake, Make & raw compilers.
- New admission package: denial events, immutable tool import, capability classification, policy generation/rollback, native-module inventory & five-minute SLA tests.
- New key service: hardware-backed grant/policy keys, separate receipt key, rotation/revocation, recovery & attestation.
- New macOS package: signed client, LaunchDaemon/XPC broker, Endpoint Security system extension & installer.
- New Windows package: signed client, Windows Service/named-pipe broker, App Control base/supplemental policy compiler/installer & VHDX manager.

## 22. Primary references

- Cargo build/cache routing: https://doc.rust-lang.org/stable/cargo/reference/build-cache.html
- Cargo reference: https://doc.rust-lang.org/cargo/reference/
- Apple Endpoint Security authorization: https://developer.apple.com/documentation/endpointsecurity/monitoring-system-events-with-endpoint-security
- Apple Endpoint Security entitlement requirement: https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.endpoint-security.client
- Apple AUTH deadline/implicit-allow behavior: https://developer.apple.com/videos/play/wwdc2020/10159/
- Apple Service Management: https://developer.apple.com/documentation/servicemanagement/
- Apple XPC: https://developer.apple.com/documentation/XPC
- Windows App Control rules: https://learn.microsoft.com/windows/security/application-security/application-control/app-control-for-business/design/select-types-of-rules-to-create
- Windows App Control enforcement rollout: https://learn.microsoft.com/windows/security/application-security/application-control/app-control-for-business/deployment/enforce-appcontrol-policies
- Windows App Control policy operations/CiTool: https://learn.microsoft.com/windows/security/application-security/application-control/app-control-for-business/operations/citool-commands
- Windows App Control supplemental-policy management: https://learn.microsoft.com/intune/device-configuration/endpoint-security/manage-app-control
- Windows App Control managed installer: https://learn.microsoft.com/windows/security/application-security/application-control/app-control-for-business/design/configure-authorized-apps-deployed-with-a-managed-installer
- Windows named-pipe security: https://learn.microsoft.com/windows/win32/ipc/named-pipe-security-and-access-rights

## 23. V4 adversarial-review dispositions

| Finding | V5 disposition |
|---|---|
| Apple entitlement | accepted; immediate Phase-0 request, pending state & denied=`BLOCKED` |
| Unknown/rebuilt Cargo indistinguishable | accepted; strict default-deny applies to every unknown executable |
| ES timeout implicitly allows | accepted; measured stop/go gate, session lockdown & explicit OS trust boundary |
| Windows write boundary | accepted; service-owned stage/chamber; generated-child WDAC proof is hard gate |
| Seatbelt suggestion | not adopted without supported API; service identity/stage is baseline |
| Mac relief blocked by Windows | accepted; independent platform gates |
| Missing non-goals/effort | accepted; explicit §3 + rollout envelope |
| Git/worktree provenance | accepted; staged Git closure + fixture |
| External path dependencies | accepted; recursive source-closure resolver |
| Sync-back/conflicts | accepted; command matrix + optimistic transaction |
| Path divergence | accepted; canonical pipeline remapping, no unmanaged byte-identity claim |
| Stage latency/accounting | accepted; measured promotion budgets, quota class & leases |
| Admission wording | accepted; atomic admission, parallel heavy slots & observable queue |
| Third-party Cargo tools | accepted; inventory, registration & capability policy |
| Multi-user ambiguity | accepted; one designated developer is explicit scope |
| sccache lifecycle | accepted; broker-owned daemon + maintenance lease |
| Broker restart | accepted; generation-bound kill/poison/reconcile |
| rustup proxy/CA | accepted; signed network recipe + secret handles |
| Unified Log durability | accepted; receipts are durable, OS logs diagnostic |
| Admin-forgeable machine key | accepted; admin-proof history is non-goal |
| Missing verification rows | accepted; §18 expanded |

## 24. V5 revision-2 adversarial-review dispositions

| Finding | Disposition |
|---|---|
| Generic builders asserted, Cargo pipeline specified | accepted; mandatory adapter schema + SwiftPM, Node/node-gyp, CMake, Make & raw-compiler staging parity added |
| Windows UMCI misses interpreter native modules | accepted; `.dll`/`.pyd`/`.node`/proc-macro load policy, immutable roots, supplemental hash policies & hard Phase-0 proof added |
| No steady-state self-service admission | accepted; `right build admit-tool`, denial UX/triage, rollback & five-minute hard gate added |
| Dynamic signing must be path-bound | premise corrected; V5 never dynamically signs generated code into global trust; all grants now explicitly bind protected path, identity, request & policy generation |
| Machine grant key is crown-jewel | accepted; separate hardware-backed non-exportable grant key, rotation/revocation & compromise recovery added |
| Sync-back uses mtime | stale premise; existing V5 captured hashes, now explicitly forbids mtime decisions |
| `rustc -o` delivery/run unspecified | accepted; stage, content-hash promotion, receipt & later request grant added |
| Seatbelt private/deprecated risk | stale premise; existing V5 rejected undocumented Seatbelt, now explicitly bars `sandbox_init` without new ADR |
| nvm Node version churn | accepted; nvm is import-only, service-owned versioned runtime + matrix row added |
| Permanent whitelisting cost | accepted; signed operational-cost acceptance is completion evidence |

## 25. Final adversarial-pass dispositions

| Finding | Disposition |
|---|---|
| macOS service identity can write every managed root | accepted; root broker + generation-bound isolated runner-UID pool, read-only inputs, capability-mediated shared writes & hostile escape spike added |
| Supplemental hash-policy scale unknown | accepted; full inventory + 2x churn, 50% safe-ceiling margin, 60-second activation P99 & five-minute admission gates added |
| macOS interpreter `dlopen` outside builds | accepted as explicit non-goal; it cannot produce build receipt or access managed roots |
| Apple entitlement response/denial decision | accepted; unbounded `BLOCKED_EXTERNAL`, independent progress & recorded permanent-`CUTOVER`/abandon ADR choice added |
| Swift adapter rollout order | accepted; mandatory before first Swift-related app cutover |
| `cargo metadata` lockfile mutation | accepted; existing lock uses `--locked`, missing lock uses explicit resolution + content-hash sync-back |

Final adversarial verdict is `APPROVED_FOR_PHASE_0`. Further planning review is not required. Next work is §5/§17 Phase-0 execution; missing evidence is expected input to that work.
