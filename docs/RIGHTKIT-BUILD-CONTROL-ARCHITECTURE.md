# RightKit Build Control — Architecture & Operations

**Scope:** `@rightkit/build-control-{core,broker,clients,policy,staging}` — the managed
Rust/Cargo build broker shared by every Orthic/Right product.

**Status:** source complete; operationally **unaccepted**. See
[BUILD-CONTROL-REMEDIATION.md](BUILD-CONTROL-REMEDIATION.md) for what is proven, what is
still open, and the incident history that produced this design. Read that document before
claiming any part of this one is "done".

This is the single architecture and operations document for build control. It replaces
`RIGHTKIT-RECOVERY-SUMMARY.md`, `RIGHTKIT-BROKER-RECOVERY.md`, and
`MANAGED-BUILD-CONTROL.md`, which were three overlapping and partly contradictory sources.

---

## 1. Why this exists

Every managed agent's Rust work goes through one broker per machine. Without it, parallel
agents ran unbounded `cargo` invocations against shared toolchains and product-local
`target/` directories: multi-GB output nobody owned, compilers orphaned by killed parents,
and a machine-wide lane that could wedge until someone noticed. The broker exists to make
build work **owned, bounded, and recoverable** — not to make it faster.

---

## 2. Invocation contract

```bash
rightkit cargo <args>
rightkit rustc <args>
rightkit rustdoc <args>
rightkit rustup target add <registered-triple>
```

Direct `cargo`, `rustc`, `rustdoc`, and `rustup run …` are denied by policy
(`build-control-policy`). Never bypass through an absolute binary path, `PATH` or
environment changes, `--target-dir`, `CARGO_TARGET_DIR`, a workspace build-config file,
aliases, wrappers, toolchain selectors, or a local `target`/`.cache` root.

`pnpm` and `npm` remain permitted; any child Cargo invocation must inherit RightKit shim
precedence from `PATH`.

**The policy gate admits `rightkit` only as one uncomposed command.** A pipe, redirect,
semicolon, subshell, or newline in the same command line makes it a bypass candidate and it
is refused. This is intentional and is not a defect — batch by running the command alone
and post-processing separately.

**A workspace build-configuration file is rejected outright.** The broker walks every
ancestor directory of the workspace and refuses any `.cargo/config[.toml]` that redirects
tools, outputs, flags, or aliases. Consequently **manifest `[profile.*]` sections are the
only place workspace-wide build cost can be shaped** — there is no linker or `RUSTFLAGS`
override available to a product repo.

---

## 3. Lanes and lifecycle

```text
SUBMITTED → CLASSIFIED → QUEUED → ADMITTED → STARTING → RUNNING
          → REAPING → SUCCEEDED | FAILED | CANCELLED | TIMED_OUT
```

| Lane | Purpose | Concurrency |
|---|---|---|
| `CONTROL` | version/metadata/tree probes — must stay responsive during builds | `controlConcurrency` |
| `BUILD` | compilation; one machine-wide lane | `buildConcurrency` |
| `WORKSPACE_WRITE` | lockfile/manifest mutation (`update`, `add`, `remove`, `generate-lockfile`, `vendor`, and the pinned cargo-edit `upgrade`/`set-version`); serializes against BUILD per repository | `workspaceWriteConcurrency` |
| `TOOLCHAIN_WRITE` | target-std provisioning; drains every lane and runs alone | exclusive |
| `MAINTENANCE` | GC, retention, inventory; broker-internal only | `maintenanceConcurrency` |

Unknown commands **fail closed** (`RK_UNKNOWN_COMMAND`). Classification is an explicit
registry, never inferred.

Socket close detaches an observer; it does not cancel work. Only an explicit `CANCEL`, a
deadline, or broker shutdown starts `TERM → grace → KILL → reap`. Nested Cargo inherits the
root request's lane, permit, namespace, reservation, environment, and cancellation domain
rather than queueing again.

Exactly one durable terminal receipt follows a certified process-tree reap. The receipt
records request identity, lane, terminal exit, namespace generation, bounded output
head/tail/digest, storage diagnostics, and event-log durability. SQLite WAL owns lifecycle,
receipts, reservations, maintenance leases, and migration state.

---

## 4. Recovery is total

Recovery runs at startup, **before the socket binds**. It therefore may never throw: a
throw there is not an error path, it is a machine that can no longer build.

For each open request:

- Not yet launched (`ADMITTED`, no ownership) → `CANCELLED`, reason `broker-restart-before-launch`.
- Ownership present and the tree provably reaped → `CANCELLED`, reason `broker-restart`.
- Ownership missing, or the reap cannot be proven → **quarantined**: terminalized with a
  typed receipt carrying `quarantine.code`
  (`RK_RECOVERY_OWNERSHIP_AMBIGUOUS` / `RK_RECOVERY_REAP_INCOMPLETE`), the reap detail, and
  `processMayBeLive` when the reap escalated.

`processMayBeLive: true` means a descendant may have survived the restart. It is an
operator signal, not a clean cancellation, and no runtime path reads it.

**Quarantine also gates the namespace.** A namespace is one directory per
repository/target and its generation is not part of the path, so terminalizing the row
alone would let a later job reuse the directory a surviving compiler is still writing into.
Quarantine records a durable gate and `#prepareNamespace` refuses heavy work there with
typed `RK_NAMESPACE_QUARANTINED`. The gate applies to any unproven reap. CONTROL work is
unaffected, so the machine stays usable.

Clearing the gate is currently manual: remove the `namespace-quarantine:<namespace>`
broker setting once the orphaned tree is confirmed gone.

> **This was a real defect, fixed 2026-08-21.** Both quarantine cases previously threw
> (`job-service.mjs`), so a single unrecoverable row left the broker unable to start, with
> no exit but manual SQLite surgery. Proof: `recovery yields exactly one receipt at every
> open lifecycle boundary` in `job-service.test.mjs`.

A reap refuses rather than signalling when persisted creation-time identity does not match
the live PID — that is PID reuse, and terminating the stranger is worse than leaving it.

---

## 5. Storage, admission, and GC

- Admission uses deadline-bounded `statfs`, a free-space floor, a durable per-request
  reservation, and lane capacity. It never recursively walks managed trees.
- A storage-probe failure is diagnostic: it blocks **new heavy** admission and never kills
  active work.
- GC never blocks admission or terminalization. It takes a namespace generation lease,
  journals intent, renames to managed trash, then deletes in bounded background batches,
  resuming after crashes. A newer namespace generation cannot be deleted by an older queue
  entry. Unknown or live paths stay dry-run findings.
- Failed and cancelled builds retain warm targets under normal LRU policy.
- `targetCacheCapBytes` bounds the warm target cache; `targetCacheFloorMs` protects
  recently-used roots so a workspace permanently over cap cannot evict what it just built.

Managed state lives on external storage — Mac `/Volumes/D/.rightkit-managed`, Windows
`D:\.rightkit-managed` — and the broker must leave **no** product-local `target`,
`.cache/cargo-target`, or unexplained debug output.

---

## 6. Runtime identity and cutover

Runtime comes only from **committed, immutable, content-addressed source**. Mutable
Homebrew paths are not runtime dependencies.

Activation stages a candidate, verifies content, probes it against isolated state so live
leases are untouched, switches `runtime-current` atomically, re-verifies the executed
bytes, and rolls the pointer and the service back together on failure.

```bash
node tools/rightkit/packages/build-control-broker/src/runtime-cutover-macos-cli.mjs \
  --config <external-root>/config.json --external-root <external-root> \
  --source-packages <checkout>/tools/rightkit/packages \
  --source-revision <git rev> --source-tree-digest <git rev of packages tree> \
  --version <immutable-id> --label dev.rightkit.build-control-managed --uid $(id -u) \
  --endpoint <external-root>/broker.sock --plist ~/Library/LaunchAgents/<label>.plist \
  --v1-state <external-root>/state/broker.json \
  --health-workspace <external-root>/fixture-rust
```

`--source-revision` and `--source-tree-digest` must be real Git digests and the packages
subtree must be clean — **production runtime cannot come from an uncommitted tree.**

Two traps worth knowing:

- **The pointer can be flipped without restarting the service.** Then the running daemon
  executes the previous bytes while `runtime-current` names newer ones, and no fresh client
  can complete the digest handshake. Always restart as part of a cutover.
- **A client/broker digest mismatch is the expected symptom** of the above, not a bug.

---

## 7. Client/tool registration

Tools are digest-pinned in `config.json`. A changed pinned tool returns typed
`TOOL_CHANGED` with a registration action; it is never silently trusted nor reported as a
generic failure.

External Cargo plugins (`cargo-<name>`) are registered as `externalCommands` entries with a
`toolId`, an explicit `lane`, and a SHA-256 `digest`. The digest, lane, and tool id are the
safety mechanism. Two rules bound registration:

1. The command must be a bounded token (`/^[a-z][a-z0-9-]*$/`), and never a key inherited
   from `Object.prototype`.
2. It may never shadow a routed built-in (`build`, `clean`, …) **nor a Cargo built-in this
   broker deliberately does not route** (`fix`, `add`, `publish`, `login`, …). Those must
   keep rejecting as unknown commands rather than being redirected to a chosen binary.

A plugin is invoked by Cargo's convention, `cargo-<cmd> <cmd> …`; the subcommand is
forwarded, not stripped.

Note which commands are Cargo's own and which are plugins: `update`, `add`, and `remove` are
built-ins and route directly, so they are never registered as external tools. `upgrade` and
`set-version` belong to cargo-edit and stay unreachable until their digests are pinned.

> **Two real defects here, both fixed 2026-08-21.** The command name was gated by a
> hardcoded `about`/`nextest` literal in **two** validators — the capability registry and
> config validation. Opening only the registry left config load rejecting the same row, and
> because the broker exits non-zero before binding while launchd `KeepAlive` restarts it,
> the result was a **crash loop**. Separately, the subcommand was stripped before spawning,
> which only appeared to work because `cargo-about` tolerates its absence.

**Standing trap:** plugins registered from `~/.cargo/bin`, and a Homebrew-path `sccache`,
sit on mutable paths. A `cargo install --force` or `brew upgrade` changes the digest and
trips `TOOL_CHANGED`, blocking builds until re-registered. Safe failure, but it will bite.
Moving these into the content-addressed runtime is the real fix and is **not done**.

---

## 8. Cache (sccache)

sccache is off unless broker-owned server lifecycle and child-process containment are
proven through the typed ownership contract; otherwise the broker records
`RK_SCCACHE_OWNERSHIP_UNPROVEN` and uses direct rustc. Enabled requests take bounded JSON
stats snapshots before and after execution and store the delta, or typed degraded
diagnostics, in the terminal receipt. Stats probes time out and never terminate a compiler.
Quiet compilers are never killed.

Enabling it is a config cutover, not a code change:

| Field | Off | Managed |
|---|---|---|
| `sccache.mode` | `disabled` | `managed` |
| `sccache.lifecycleOwner` | `null` | `broker` |
| `sccache.requireOwnershipProof` | `false` | `true` |
| `defaults.cacheMode` | `disabled` | `managed` |
| `defaults.sccacheToolId` | absent | `sccache` |

`config.json` is hand-maintained per machine and is not reproducible by re-bootstrapping.
**Back it up before editing, and never re-bootstrap with `--force` to "fix" it.**

---

## 9. Host sizing

Sizing is per-machine and is a **memory** decision, not a core-count one. Peak concurrent
compilers are `buildConcurrency × cargoBuildJobs`; budget roughly 2 GB of RAM each. Over-
subscribing produces swap, kernel-killed compilers, orphaned trees, and receipts with
`exitCode: null` — the signature of the 2026-08-14 incident.

| Host | Threads | RAM | Sizing note |
|---|---|---|---|
| Mac (M1 Air) | 8 | 8.6 GB | 8 compilers on 8.6 GB is ~1 GB each and oversubscribed; 4 is the honest ceiling. |
| Windows (i9-13900H) | 20 | 63.7 GB | Was configured for 4 compilers — roughly a fifth of the machine. |

`targetCacheCapBytes` should reflect the actual free space on the managed volume; a cap far
below the working set thrash-evicts on every large workspace.

---

## 10. Bootstrap from nothing

The bootstrap scripts are tracked; **their output never is.**

| | Tracked in git | Generated per machine |
|---|---|---|
| Engine | `build-control-broker/src/*.mjs`, config schema, cutover logic | — |
| macOS | the script that generates the plist | `~/Library/LaunchAgents/dev.rightkit.build-control-managed.plist` |
| Windows | the script that generates the task XML | the registered scheduled task |
| Both | the script that generates `config.json`'s shape and defaults | `<external-root>/config.json` |
| Both | — | `runtime-current`, staged runtimes, state DB, receipts |

### macOS

```bash
node tools/rightkit/packages/build-control-broker/src/bootstrap-macos-cli.mjs \
  --external-root /Volumes/D/.rightkit-managed
```

Probes each known tool on `PATH`, hashes what it finds, writes `config.json` and the plist,
then runs the same drain-checked activation path as every other cutover. A tool absent from
`PATH` is simply absent from the config; nothing is invented. Requires a small Cargo
workspace at `<external-root>/fixture-rust` for the functional health probe.

### Windows

Run from an **elevated** PowerShell — re-registering an existing task needs elevation.
Strip both managed `agent-bin` directories from `PATH` first:

```powershell
$env:PATH = ($env:PATH -split ';' | Where-Object { $_ -notlike '*\.rightkit-managed\agent-bin*' -and $_ -notlike '*RightKitManagedAgent\agent-bin*' }) -join ';'
node tools/rightkit/packages/build-control-broker/src/bootstrap-windows-cli.mjs --external-root D:\.rightkit-managed
```

If `agent-bin` leads `PATH`, the tool probe's first match for `cargo`/`rustc` is the
RightKit **shim**, which then gets hashed as the tool. The broker spawns the shim as the
tool and every job exits 125 with `broker endpoint is not configured` — a misleading
message whose real cause is a shim-hashed-as-tool config.

The Windows task runs as `S4U` (no interactive console window ever) with
`RestartOnFailure` (1-minute interval, 999 retries) — the `KeepAlive` equivalent, which
only the Task Scheduler XML schema can express.

Both bootstrap scripts drain, then stop, then activate. If a managed build is in flight,
bootstrap refuses rather than killing it.

---

## 11. Diagnosis

| Symptom | Action |
|---|---|
| `125`, endpoint unavailable, or no typed result | Restore broker service/endpoint and verified external storage; rerun `rightkit cargo --version`. |
| `RightKit client/broker runtime digest mismatch` | The pointer moved without a service restart, or the client is built from a different revision. Restart the service as part of a cutover. |
| Broker restarts repeatedly, exit 1, never binds | Config and running code disagree. Check the service's stderr log; validate config against the **running** runtime before restarting. |
| Denied target/config/environment/alias | Remove the redirect and resubmit unchanged work through RightKit. |
| Long `QUEUED` heartbeat | Another BUILD owns the machine lane; use STATUS/ATTACH and wait for admission. |
| Cache diagnostics `DEGRADED` | sccache stays off without broker ownership proof; direct rustc remains authoritative. |
| `REAP_PENDING` | Restart the broker; startup reaps persisted ownership before writing a `CANCELLED` receipt or admitting heavy work. |
| Quarantined receipt with `processMayBeLive` | A descendant may have survived a restart. Check for orphaned compilers before admitting heavy work on that namespace. |
| Failed runtime update health | Keep the restored current runtime; repair the candidate and rerun activation health. |

Never retry via direct Rust tooling, an absolute binary path, environment scrubbing, or
another wrapper. Those are policy bypasses, not recovery steps.

---

## 12. Known-incomplete areas

These are stated here so they are not rediscovered as surprises. Their evidence and proof
status live in [BUILD-CONTROL-REMEDIATION.md](BUILD-CONTROL-REMEDIATION.md).

- **Windows Job Object containment** is written (`native/windows-job/windows_job.c`) and
  its JS wiring is fully tested against an injected fake addon, but the C code has never
  been compiled or run on a real Windows kernel. Windows BUILD containment is unproven.
- **Legacy `POISONED` v1 state** crashes the v1→v2 migration (`RK_V1_UNKNOWN_STATE`). The
  engine should remap it; today it needs a manual repair.
- **The Windows cutover CLI can silently no-op** — its direct-execution guard never matches
  a backslashed Windows argv path.
- **Mutable tool paths** — see §7.
- **`runtime.test.mjs` fixture failures** (7 tests): the fixture's temp repo resolves to the
  repository toplevel, so `verifySourceProvenance` rejects it. Test-side only; the real
  cutover path is unaffected.

---

## 13. Publication

Build-control packages are private workspace components delivered by one exact commit on
`bogusyogi/claude` `main`. No independent npm publication is claimed. Windows deployment
pulls that committed revision before installing its self-contained runtime. Consumer
projects never depend on build control through workspace, file, link, Git, or
sibling-checkout specs.
