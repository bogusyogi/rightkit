# RightKit Managed-Agent Build Control V1

Status: approved & frozen for implementation, revision 4
Relationship: companion profile to frozen V5; V5 remains `STRICT_SYSTEM`
Platforms: macOS 13+; Windows 11 Pro+

## 1. Outcome

`MANAGED_AGENT` prevents configured coding agents from bypassing RightKit for Rust/Cargo build execution or creating Rust build, cache, target or temporary payload outside RightKit-managed external storage. It preserves familiar Rust commands through RightKit clients while avoiding native system-wide executable allowlisting.

This profile addresses accidental or policy-violating agent execution. Hostile software able to download or manufacture alternate toolchains is outside this profile & remains covered only by V5 `STRICT_SYSTEM`.

## 2. Invariants

1. Agent `PATH` exposes RightKit Cargo clients before real Rust tools.
2. Client submits typed request to local broker & never launches real toolchain.
3. Broker resolves invoked tools only from registered immutable SHA-256 paths. Managed Cargo deployment also pins rustc, rustdoc & sccache subordinate paths.
4. Broker owns build environment, reservations, writer leases, cancellation & receipts.
5. Cargo registry/git data & compiler cache may be shared globally.
   Concurrent requests conservatively charge overlapping global-cache growth to every active reservation; overcount is allowed, undercount is not.
6. Global Cargo home/compiler cache remain external; targets, build & temp remain namespaced by workspace identity, toolchain, target triple & build profile beneath one external root. Policy profile is not a build-profile namespace dimension.
7. Cache, build, staging & temporary paths never fall back to internal storage.
8. Rust/Cargo request is rejected when argv, environment or Cargo configuration redirects controlled paths or Rust tool executables.
9. Pre/post task scan reports unmanaged output directories beneath workspace.
10. Hard quota admission & automatic GC bound managed storage.

## 3. Managed root

```text
<external-root>/
  rightkit-build-control/
    global/{cargo-home,compiler-cache,toolchains}/
    workspaces/<sha256(repository-identity,toolchain,target,build-profile)>/{targets,build,temp}/
  .rk-sccache.sock                # POSIX only; Windows uses native sccache IPC
    receipts/
```

`workspace-id` is derived from canonical workspace identity, never display name. Broker creates short external temp roots plus POSIX sccache socket before tool launch; Windows uses native sccache IPC while cache payload remains external. One global undifferentiated target directory is forbidden.

## 4. Controlled inputs

Managed policy targets only `cargo`, `rustc`, `rustdoc`, plus `rustup run … cargo|rustc|rustdoc`, unmanaged absolute real-Rust paths, & PATH/environment/shell-wrapper bypasses. It rejects split or inline `--target-dir`/`--config`, Cargo `+toolchain`, `CARGO_HOME`, `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`, `CARGO_INSTALL_ROOT`, `RUSTC`, `RUSTDOC`, compiler-wrapper overrides & temporary-directory overrides. Broker replaces controlled values with its own normalized environment. `--` terminates option parsing.

`pnpm`, `npm`, `node-gyp`, CMake, Make, Swift, `xcodebuild` & raw compilers remain usable. Their policy proof must show a child Cargo invocation inherits the RightKit shim; they are not themselves denied or brokered by this revision.

Workspace scan covers at least `target`, `.cache/*target*`, `.right-release/cache`, `.build`, `.cargo-target`, `CMakeFiles`, `cmake-build-*`, `DerivedData` & tool-specific caches recorded by adapter policy. Dependency source trees such as `node_modules` are not classified or traversed as build caches. Checked-in fixtures are excluded only through explicit policy.

## 5. Components

- `@rightkit/build-control-core`: profiles, request normalization, override rejection, namespace derivation & scan policy.
- `@rightkit/build-control-broker`: digest verification for Cargo/rustc/rustdoc/sccache, quota/reservation/lease state, managed environment, short external temp/socket, poison/recovery, durable receipts & GC selection.
- `@rightkit/build-control-clients`: request-only CLI plus shim manifest for Cargo & Rust.
- `@rightkit/build-control-policy`: agent policy parser, native Codex/Claude policy declarations & PreToolUse gap coverage for absolute paths, wrappers, aliases, alternate names & compound commands.
- `@rightkit/build-control-staging`: external transactional staging & sync-back contract.

V5 native macOS Endpoint Security, Windows App Control, signing, installer & hostile-executable admission remain unchanged & optional for this profile.

### Explicit agent entrypoint

`rightkit` is an explicit managed entrypoint. It accepts only `rightkit build <registered-frontend> -- <args>` or `rightkit <registered-shim> [args]`, converts either form to a typed `MANAGED_AGENT` request, & never launches a build executable. Malformed syntax, unregistered names, missing broker transport or invalid typed broker result return exit `125`.

### Agent-only policy

Managed enforcement applies only to configured Codex, Claude, Cline & CommandCode agent launch contexts. Activation mutates only those agents' user configuration/policy; it does not alter ordinary interactive shells, IDEs, CI or system-wide executable policy. Each policy matrix must prove agent `PATH` precedence, broker endpoint injection, external-root identity, request-only Rust `rightkit`/shim behavior, & pre/post workspace scan failure.

Claude native `permissions.deny` for direct Rust tools plus explicit `permissions.allow` for exact RightKit commands are primary enforcement. PreToolUse parser coverage is supplementary: it closes absolute-path, wrapper, alias, alternate-name & compound-command gaps; it does not replace native deny/allow policy.

## 6. Rollout

1. Ship libraries & conformance tests.
2. Configure external root, immutable tool manifest, quotas & broker endpoint.
3. Activate only Codex, Claude, Cline & CommandCode user policy plus agent-only Rust shim directory at front of each managed agent `PATH`.
4. Run shadow scan across current apps; inventory & migrate unmanaged caches.
5. Enable fail-closed broker admission plus pre/post scan failures; a local-output violation returns typed exit `125` & poisons its request.
6. Verify clean, warm, test & release flows for each adapter.
7. Enable automatic GC after receipt & lease recovery tests pass.
8. Run separate Codex, Claude, Cline & CommandCode policy matrices, then macOS & Windows managed-agent matrices. Platform reports must prove only agent contexts are managed; both normal CLI environments & V5 strict enforcement remain independent.

No Apple entitlement, system extension, WDAC policy or new adversarial planning cycle gates this rollout.

## 7. Acceptance

- Every Rust/Cargo shim creates a normalized `MANAGED_AGENT` request & fails closed without broker.
- Controlled argv/environment override matrix is rejected.
- Same workspace/toolchain/target/profile maps to same normalized external namespace; any differing dimension maps separately.
- Quota overrun, conflicting writer, cancellation & disconnect cannot leak durable reservation/lease state or reusable poisoned target; every terminal result has durable receipt before success is reported.
- Broker environment routes Cargo home, targets, build intermediates, compiler cache, toolchains, user-home/XDG state & temp beneath external root.
- Scanner identifies unmanaged workspace outputs; local-output violation returns typed exit `125`.
- `rightkit build cargo -- <args>` & `rightkit cargo|rustc|rustdoc ...` submit only typed broker requests; malformed forms & unavailable broker fail closed with exit `125`.
- Codex, Claude, Cline & CommandCode matrices prove configured agent-only policy with no ordinary interactive-shell, IDE or system-policy mutation. Claude native deny/allow is primary; each agent's policy/hook layer covers bypass gaps.
- macOS & Windows matrices prove Rust shim/`rightkit` routing, external root/temp behavior, local-output exit `125`, durable receipt, restart recovery, platform-native broker transport & pnpm/npm child-Cargo shim inheritance.
- Agent-only enforcement is verified separately from `STRICT_SYSTEM`; no V5 native policy claim follows from this profile.
- Focused core, broker, clients & staging tests pass.
- V5 strict evidence status remains independent.

## 8. Revision 3 durability & closure gates

- macOS runtime deployment is self-contained: no workspace dependency links. Version directories publish atomically through one current pointer, run a bounded health probe, & roll back to last healthy version on failure.
- Startup converts every stale `ACTIVE` lease to poisoned recovery evidence, clears its reservation, & reconciles managed `usedBytes` from actual disk state.
- GC evaluates namespace-level `RELEASED` plus `POISONED` cache data by retention/LRU/quota pressure. It never deletes an active lease or shared root needed by a live namespace.
- Scanner traversal has explicit entry/depth/time bounds, never follows symlinks or dependency/vendor trees, detects pre-existing plus newly created nested Rust outputs, & emits actionable paths in rejection receipts.
- Cleanup records a before/after inventory. It deletes only confirmed disposable build/cache/temp roots; uncertain or protected roots remain explicit exceptions.
- Mac policy evidence includes Codex Desktop/CLI, Claude Code CLI, Cline CLI & CommandCode. Windows retains its connected-host Codex/Claude matrix.
- Closure requires bounded `rightkit cargo --version` plus a real locked build, durable receipts, external target/cache/temp, no local Rust output, restart persistence, released-namespace reclamation, quota/free-floor termination & green service health.

## 9. Revision 4 documentation, publication & parity gates

- Canonical workspace agent rules require `rightkit cargo …` plus managed `rightkit rustc|rustdoc …`, prohibit direct or disguised Rust-tool bypasses, keep pnpm/npm available, & require their Cargo children to inherit RightKit.
- [`MANAGED-BUILD-CONTROL.md`](../MANAGED-BUILD-CONTROL.md) is canonical for macOS & Windows health, external roots, receipts, recovery, cleanup, retention/GC & fail-closed troubleshooting.
- Cleanup PASS means every previously retained path & capped root has a bounded classification. It never means zero legacy output unless evidence explicitly reports zero; signed/release artifacts & unknown user data stay retained with exact path, measured size or bound, reason & policy.
- These private workspace packages publish through one exact commit on `bogusyogi/claude` `main`; no npm publication is claimed.
- Windows pulls that published commit before deployment, uses a self-contained runtime under `D:\\.rightkit-managed`, & applies shared committed policy to Codex, Claude, Cline & CommandCode while preserving existing settings/hooks.
- Windows parity proves direct, absolute, environment & rustup-run bypass denial; successful RightKit locked build; allowed pnpm/npm child-Cargo routing; external-only outputs; scheduled-task restart; released-cache reclamation; durable receipt; & zero active leases.
- Canonical PASS requires deterministic macOS/Windows parity evidence plus documentation, remote containment, legacy-inventory, focused/full/schema/Cortex & diff gates.
