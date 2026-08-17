# Managed Build Control

Operational runbook for RightKit `MANAGED_AGENT` Rust/Cargo requests.

## Invoke

Use only:

```bash
rightkit cargo <args>
rightkit rustc <args>
rightkit rustdoc <args>
```

Direct `cargo`, `rustc`, `rustdoc`, & `rustup run …` are intentionally denied.
Never bypass with absolute tool binaries, `PATH`/environment changes,
`--target-dir`, `CARGO_TARGET_DIR`, `.cargo` configuration, aliases, wrappers,
or local `target`/`.cache`. `pnpm` & `npm` remain permitted; any child Cargo must
inherit RightKit shim precedence.

## Health

Run this from a managed agent context on either platform:

```bash
rightkit cargo --version
```

Mac additionally exposes broker transport through
`$RIGHTKIT_BUILD_BROKER_SOCKET`; confirm it names a reachable Unix socket:

```bash
test -S "$RIGHTKIT_BUILD_BROKER_SOCKET"
rightkit cargo --version
```

Windows uses its configured native broker/sccache IPC endpoint. In PowerShell,
confirm endpoint configuration & then make the same managed request:

```powershell
$env:RIGHTKIT_BUILD_BROKER_SOCKET
rightkit cargo --version
```

Healthy means endpoint accepts v2 client/broker digest handshake & `rightkit
cargo --version` returns a typed successful result. Installed shims resolve
`runtime-current` on every invocation. Each candidate carries a read-only
runtime manifest with client & broker digests. Activation probes candidate,
using isolated state/storage so live leases remain untouched, switches
`runtime-current` atomically, probes selected runtime, & restores prior
symlink on failed post-switch health. Any callback that restarts live service
must supply matching rollback callback so selected symlink & running broker
return to same runtime together.

Windows installs retain `.cmd` wrappers for compatibility & install native PE
`cargo.exe`, `rustc.exe`, `rustdoc.exe`, & `rustup.exe` launchers beside them.
Install compiles launcher from checked-in source with Windows .NET Framework
`csc.exe`; launcher reads only adjacent owned manifest, resolves
`runtimeCurrent` & `nodePath`, derives its shim basename, then invokes existing
`managed-agent.mjs`. Missing or malformed ownership state exits `125`.

## Paths & receipts

Mac stores managed data under
`/Volumes/D/.rightkit-managed/rightkit-build-control`; Windows uses configured external storage
under `D:\.rightkit-managed\rightkit-build-control`. The external root contains
shared Cargo/toolchain/compiler-cache state, workspace namespace targets/build/
temp roots, broker state, & durable receipts. Mac uses a Unix socket; Windows
uses native IPC. Exact external-root configuration wins over these defaults.

Every request follows `SUBMITTED → CLASSIFIED → QUEUED → ADMITTED → STARTING →
RUNNING → REAPING → SUCCEEDED | FAILED | CANCELLED | TIMED_OUT`. Exactly one
durable terminal receipt follows certified process-tree reap. Receipt records
request identity, lane, terminal exit, managed namespace generation, bounded
output head/tail/digest, storage diagnostics, & event-log durability. SQLite WAL
owns lifecycle, receipts, reservations, maintenance leases, & migration state.

BUILD uses one machine-wide lane; INSTALLER becomes next without preemption.
CONTROL stays available during builds. WORKSPACE_WRITE serializes against BUILD
for same repository. Socket close detaches observer; only explicit CANCEL,
deadline, or broker shutdown starts TERM → grace → KILL → reap. Nested work
inherits root lane, namespace, reservation, environment, & cancellation domain.

## Diagnose & recover

| Symptom | Action |
| --- | --- |
| `125`, endpoint unavailable, or no typed result | Restore broker service/endpoint & verified external storage; rerun `rightkit cargo --version`. |
| Denied target/config/environment/alias | Remove controlled redirect & submit unchanged work through RightKit. |
| Unmanaged local output | Use receipt paths to remove only confirmed disposable output, then retry through RightKit. |
| Long wait with `QUEUED` heartbeat | Another BUILD owns machine lane; use STATUS/ATTACH & wait for admission. |
| Quota, reservation, cancellation, or broker restart | Read terminal receipt; wait for lease recovery or restore capacity, then retry through RightKit. |
| Cache diagnostics marked `DEGRADED` | sccache stays disabled unless broker ownership proof exists; direct rustc remains authoritative. |
| `REAP_PENDING` | Restart managed broker; startup reaps persisted PID/process-group ownership before writing `CANCELLED` receipt or admitting more heavy work. |
| Failed runtime update health | Keep restored current runtime; repair candidate & rerun activation health. |

Never retry via direct Rust tooling, a real-binary absolute path, environment
scrubbing, or another wrapper. Those are policy bypasses, not recovery steps.

## Cleanup & GC

Cleanup records before/after inventory & deletes only receipt-confirmed disposable
build/cache/temp roots. Protected artifacts, active leases/reservations, shared
live roots, signed/release assets, unknown user data, & uncertain paths remain.

Ordinary admission, polling, & terminal handling never recursively walk managed
trees. Heavy admission uses deadline-bounded `statfs`, free-space floor, durable
per-request reservation, & lane capacity. Active probe failure is diagnostic &
blocks only new heavy work.

Maintenance is independent, scheduled, & bounded by scan/delete count, depth, &
cursor size. GC generation-CAS leases a terminal namespace, journals intent,
renames it to managed trash, then resumes bounded deletion after crashes. Newer
namespace generations cannot be deleted by older queue entries. Unknown or live
paths remain dry-run findings.

sccache is disabled by default. RightKit may enable it only when broker-owned
server lifecycle & child-process containment are proven through the typed
ownership contract; otherwise it records `RK_SCCACHE_OWNERSHIP_UNPROVEN` & uses
direct rustc. Enabled requests take bounded JSON stats snapshots before & after
execution; terminal receipt stores delta or typed degraded diagnostics. Stats
probes time out & never terminate a compiler. Quiet compilers are never killed.
Windows BUILD remains fail-closed until native Job Object containment exists.

## Publication semantics

Build-control packages are private workspace components. They are delivered by
one exact commit on `bogusyogi/claude` `main`; no independent npm publication is
claimed. Windows deployment pulls that committed revision before installing its
self-contained runtime. Consumer projects never depend on build-control through
workspace, file, link, Git, or sibling-checkout specs.
