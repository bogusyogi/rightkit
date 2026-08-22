# RightKit Thin Build Control v2.7 — Native Acceptance

**Date:** 2026-08-22  
**Implementation revision:** `54f2991b7b534b1a5dc23eae9d780f34b27cc3d7`  
**Scope:** macOS containment closure, Windows Job Object acceptance, predecessor removal.  
**Measurement status:** deferred by user; no cache A/B or BUILD-capacity result is claimed.

## Result

| Host | Native runtime | Result |
|---|---|---|
| macOS | launchd + POSIX process group/session | PASS |
| Windows | Task Scheduler + native Job Object `KILL_ON_JOB_CLOSE` | PASS |

Both hosts retain managed `rightkit cargo ...`, shared Cargo home, shared
compiler-cache directory, persistent workspace generations, one global BUILD
lane, digest-pinned tool paths, runtime identity handshake, root receipts, and
whole-tree cancellation.

## macOS evidence

Final runtime `bootstrap-54f2991b-1787420923105` activated with source revision
`54f2991b7b534b1a5dc23eae9d780f34b27cc3d7` after active-root drain proof.

| Behavior | Root receipt | Result |
|---|---|---|
| warm generation reuse | `41acb265-f252-4a5c-81e4-c8f8f48f10a9` | `SUCCEEDED`, generation 1, `REAPED`, compiler broker round trips 0 |
| explicit client-signal cancellation | `946060e5-eb0a-4d48-9c49-f571c4d874c0` | `CANCELLED`, `CLIENT_SIGNAL`, generation 1 retained, `REAPED` |
| raw managed rustc | `6c5d4432-ee69-486c-9762-31c5daf22b50` | `SUCCEEDED`, `REAPED` |
| unrelated live build during cutover | `ab271fe8-45c2-4c25-bb6b-d717ea584baf` | cutover refused; build was not interrupted; clippy completed `SUCCEEDED` |
| final live runtime smoke | `fa37b0a6-2385-464c-b714-2a004bfd77c0` | `SUCCEEDED`, compiler process 1, broker round trips 0, `REAPED` |

## Windows evidence

Native addon was built on Windows and loaded synchronously by candidate-runtime
health proof. Runtime `bootstrap-54f2991b-1787420195275` activated with source
revision `54f2991b7b534b1a5dc23eae9d780f34b27cc3d7` and native addon SHA-256
`e81f23a3900421db8ffdad74e9cb660c3a0401d2e4c80c36691c8472216c0204`.

| Behavior | Root receipt | Result |
|---|---|---|
| first native Cargo build | `c515cf78-1bce-46e9-a5e5-c8673c249945` | `SUCCEEDED`, generation 1, Job Object `REAPED` |
| warm repeat | `c65a4d8b-f25c-4ca2-9644-84457ee00c1a` | `SUCCEEDED`, generation 1 retained |
| raw managed rustc | `131ef353-0f2f-434d-a37a-50b8a59a9234` | `SUCCEEDED`, Job Object `REAPED` |
| explicit cancellation | `0ab36684-6faa-4d43-9d29-3be1c1e8edf3` | `CANCELLED`, `USER_CANCEL`, generation 1 retained, Job Object `REAPED` |
| non-zero command | `1f789d19-8d69-48d9-b68c-d9ed4c3cebc8` | `FAILED`, generation 1 retained, Job Object `REAPED` |
| post-failure command | `8b15e7b9-d2c9-4a3e-8397-16e0a11eca4b` | `SUCCEEDED`, generation 1 retained |
| broker crash recovery | `933c1c89-19ce-49af-868f-a1b179f0942f` | `ABANDONED`, generation 2 fenced, prior Job Object `REAPED` |
| post-restart build | `40f5d3a6-208f-47b4-a91d-ce1797db67d8` | `SUCCEEDED`, generation 3, Job Object `REAPED` |
| post-cleanup live smoke | `36005de1-45f7-4058-86d3-b25407e76b13` | `SUCCEEDED`, generation 3 retained, Job Object `REAPED` |

Windows native-focused Node suite passed 57/57. macOS broker suite passed
172/172. Final Windows managed Cargo smoke completed in 0.23 seconds.

## Telemetry contract

Root timing, queue timing, cancellation provenance, generation, immutable
compiler identity, process ownership, reap status, output digest, cache mode,
and compiler broker round trips are live on both hosts.

Pinned sccache 0.17 client-side counters were observed as process-local zeroes
despite real Windows compilation. Revision `328f2e073` therefore stopped
publishing false root deltas and false compiler counts. Client-side receipts now
record:

```text
cache.status = SERVICE_OK
cache.reason = client-side-counters-unattributable
cache.delta = null
cacheCounterScope = client-side-counters-unattributable
compilerProcessCount = null
```

This is explicit telemetry unavailability, not measured cache performance.

## Cleanup

Revision `328f2e073` removed predecessor SQLite state, lifecycle job service,
maintenance leases/worker, reservation scheduler, fsync-heavy event log,
broker-owned sccache server, legacy fixtures, and v1 drain/migration code. Thin
GC removes only oldest retired generations above configured cap; current and
active-writer generations are never candidates. Revision `54f2991b7` makes GC
readiness-independent so live cache sizing cannot delay broker activation.
