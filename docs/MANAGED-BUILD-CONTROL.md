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

Healthy means the endpoint accepts a request & `rightkit cargo --version` returns
a typed successful result. Runtime activation runs a socket & functional probe
before switching its current version, then rolls back on failed post-switch
health.

## Paths & receipts

Mac stores managed data under
`/Volumes/D/.rightkit-managed/rightkit-build-control`; Windows uses configured external storage
under `D:\.rightkit-managed\rightkit-build-control`. The external root contains
shared Cargo/toolchain/compiler-cache state, workspace namespace targets/build/
temp roots, broker state, & durable receipts. Mac uses a Unix socket; Windows
uses native IPC. Exact external-root configuration wins over these defaults.

Every terminal request produces a durable receipt below that root. It records
request identity, status, terminal exit, managed namespace, violations, & GC or
recovery result. Read receipt first when a request exits non-zero or `125`.

## Diagnose & recover

| Symptom | Action |
| --- | --- |
| `125`, endpoint unavailable, or no typed result | Restore broker service/endpoint & verified external storage; rerun `rightkit cargo --version`. |
| Denied target/config/environment/alias | Remove controlled redirect & submit unchanged work through RightKit. |
| Unmanaged local output | Use receipt paths to remove only confirmed disposable output, then retry through RightKit. |
| Quota, reservation, cancellation, or broker restart | Read terminal/recovery receipt; wait for lease recovery or restore capacity, then retry through RightKit. |
| Failed runtime update health | Keep restored current runtime; repair candidate & rerun activation health. |

Never retry via direct Rust tooling, a real-binary absolute path, environment
scrubbing, or another wrapper. Those are policy bypasses, not recovery steps.

## Cleanup & GC

Cleanup records before/after inventory & deletes only receipt-confirmed disposable
build/cache/temp roots. Protected artifacts, active leases/reservations, shared
live roots, signed/release assets, unknown user data, & uncertain paths remain.

Broker GC automatically considers only eligible `RELEASED` & `POISONED`
namespaces, remeasures storage, & writes a GC receipt. It never removes active or
shared live data. Treat a retained path as intentional only when its receipt
records exact path, reason, & measured size or bound.

## Publication semantics

Build-control packages are private workspace components. They are delivered by
one exact commit on `bogusyogi/claude` `main`; no independent npm publication is
claimed. Windows deployment pulls that committed revision before installing its
self-contained runtime. Consumer projects never depend on build-control through
workspace, file, link, Git, or sibling-checkout specs.
