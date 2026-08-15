# RightKit build broker — bootstrap & recovery

Before this document existed, the broker on every machine was hand-built once
and never captured anywhere: the launchd plist, the scheduled task, and
`config.json` all lived only on that machine's disk, none of it in git. A
reset machine had no path back except retyping a dozen absolute-path
arguments from memory. This is the fix — one command per OS, checked into
`tools/rightkit/packages/build-control-broker/src/`, that rebuilds a working
broker from nothing but this checkout.

## What's tracked vs. what's generated

| | Tracked in git | Generated on this machine, not tracked |
|---|---|---|
| **Engine** | `build-control-broker/src/*.mjs`, config schema, cutover logic | — |
| **macOS** | the bootstrap script that generates the plist | `~/Library/LaunchAgents/dev.rightkit.build-control-managed.plist` |
| **Windows** | the bootstrap script that generates the task XML | the registered scheduled task itself |
| **Both** | the bootstrap script that generates `config.json`'s shape & defaults | `<external-root>/config.json` — machine-specific tool paths & hashes |
| **Both** | `runtime-current`, staged runtimes, state DB, receipts | all pure runtime state; never commit these |

The right mental model: **the bootstrap scripts are tracked; their output
never is.** Losing the machine loses only output the scripts can regenerate.

## Recovering after a reset

### macOS

```bash
node tools/rightkit/packages/build-control-broker/src/bootstrap-macos-cli.mjs --external-root /Volumes/D/.rightkit-managed
```

This probes every known tool on PATH (`cargo`, `rustc`, `rustdoc`, `rustup`,
`sccache`, `pnpm`, `npm`, `swift`, `swiftc`, `cmake`, `make`, `clang`, `gcc`,
`cargo-about`), hashes what it actually finds, writes `config.json`, writes
the launchd plist, and runs the same activation path
(`runMacOSCutover`) as every other cutover — proven, drain-checked, with
rollback on failure. A tool absent from PATH is simply absent from the
config; nothing is invented.

Requires one thing this script cannot create: a tiny cargo workspace at
`<external-root>/fixture-rust` for the cutover's functional health probe. If
missing, the script says so and stops — copy one from an existing machine or
`cargo new --bin` a minimal one.

Re-run with `--force` to regenerate `config.json` from a fresh tool probe
(e.g. after a toolchain upgrade); the plist is always regenerated.

### Windows

```powershell
node tools/rightkit/packages/build-control-broker/src/bootstrap-windows-cli.mjs --external-root D:\.rightkit-managed
```

Same shape, `where.exe` instead of `which`, a Task Scheduler XML definition
instead of a plist. Two things this fixes that were previously silent
failure modes on Windows specifically:

1. **The task runs as `S4U`, not `InteractiveToken`.** `node.exe` is a
   console-subsystem binary; a task that logs into the interactive desktop
   session flashes a visible console window every time it restarts. `S4U`
   runs as the user without an interactive session — no window, ever.
2. **`RestartOnFailure` is set** (1-minute interval, 999 retries) — the
   `KeepAlive` equivalent. `schtasks.exe`'s own CLI flags cannot express
   this; only the Task Scheduler XML schema can, which is why the bootstrap
   writes XML and registers it via `schtasks /Create /XML`, not plain flags.

### Order matters

Both bootstrap scripts call the same drain-then-stop-then-activate sequence
every other cutover in this package uses
(`proveManagedRequestsDrained` → stop the old service → `proveV1Drained` →
install). If a managed build is in flight, bootstrapping refuses rather than
killing it — this is not a special case; it is the same guard added after an
earlier session actually did kill a mid-compile build by stopping the
service before checking for one.

## Windows process containment — the one piece not yet provable here

`process-tree.mjs`'s Windows path used to fail closed unconditionally:
*"Windows startup stays fail-closed until a native Job Object owner
exists."* That owner now exists as source —
`native/windows-job/windows_job.c`, a small N-API addon implementing the
standard `CREATE_SUSPENDED` → `AssignProcessToJobObject` →
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` → `ResumeThread` sequence — but it was
written on a Mac with no Windows host to compile or run it against. Every
other fix in this document was verified by actually running it; this one
could not be.

**Before trusting it in a real build:**

```powershell
cd tools/rightkit/packages/build-control-broker/native/windows-job
node-gyp rebuild
node --test ../../src/process-tree.test.mjs ../../src/windows-job.test.mjs
```

The JS-side wiring (`process-tree.mjs`, `windows-job.mjs`) is fully tested
with an injected fake addon — 29/29 in `process-tree.test.mjs` — proving the
*logic* (when to assign, when to fall back, how cross-restart reap verifies
identity before touching a possibly-reused PID) is correct. What those tests
cannot prove is that the C code itself compiles and does what the comments
say on a real Windows kernel. Only `node-gyp rebuild` succeeding, followed by
a real managed build actually running under it, proves that.

Until it's built, `loadWindowsJobAddon()` returns `null` and every containment
path in `process-tree.mjs` falls back to exactly its prior fail-closed
behavior — nothing regresses by this code existing unbuilt.

Two honest simplifications from the textbook pattern, both documented at the
call site in `process-tree.mjs` and `windows_job.c`:

- The IPC-bearing launcher (`spawnManagedProcess`) is spawned through Node's
  own `spawn()`, not the addon's `CREATE_SUSPENDED` path — Node's IPC channel
  requires Node's own process creation. Containment is layered on
  immediately afterward via `assignRunningProcessToNewJob`, which accepts a
  small, explicitly-documented race window instead of the airtight guarantee
  `CREATE_SUSPENDED` gives `spawnContained` (written, available, unused by
  the launcher for this reason).
- Cross-restart reap (`reapPersistedProcessTree`) cannot use a live job
  handle — Win32 handles die with the process that held them. In practice
  this rarely needs to do anything: an anonymous job with
  `KILL_ON_JOB_CLOSE` is torn down by the OS itself the instant the owning
  broker process's last handle closes, on *any* exit including a crash, with
  no code required to run. The reap path exists for the residual case and
  verifies PID + creation-time identity before touching anything, exactly
  matching the POSIX path's PID-reuse protection.

## sccache & config-file fixes shipped alongside this

Two Windows failures diagnosed this session, fixed at the engine level so
every app inherits the fix rather than rediscovering it:

- **`SCCACHE_IGNORE_SERVER_IO_ERROR=1`** is now set wherever `right-release`
  activates sccache (`release-state.mjs`). A dropped sccache connection
  (observed as a bare Windows error 10054) now falls through to plain
  `rustc` instead of failing the build.
- **A leading UTF-8 BOM** (written by default by `Set-Content`/`Out-File` in
  PowerShell) no longer breaks config parsing. Every JSON config the broker
  reads — `config.json`, runtime manifests, the v1 drain state, legacy
  migration state — strips one before parsing. Previously this failed as an
  opaque `Unexpected token` with no hint the file's own bytes were the
  problem.

## Verifying any of this yourself

```bash
pnpm --dir tools/rightkit/packages/build-control-broker test
```

Runs the full suite including `bootstrap-macos-cli.test.mjs` (verifies a
bootstrapped config is accepted by the broker's own live schema validator,
not a mock of it), `runtime-cutover-windows-cli.test.mjs` (13 tests, proves
the drain-before-stop ordering & the task XML's `S4U`/`Hidden`/
`RestartOnFailure` settings), `bootstrap-windows-cli.test.mjs`, and
`process-tree.test.mjs`'s Windows containment coverage. 173/179 in the full
package suite as of this writing; the 6 failures are pre-existing,
unrelated to any of the above, and reproduce identically with every change
in this document stashed out.
