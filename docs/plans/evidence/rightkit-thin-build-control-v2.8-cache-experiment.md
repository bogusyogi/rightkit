# RightKit Thin Build Control v2.8 — Cache Experiment Record

**Status:** macOS experiment campaign complete; production decisions applied.
**Date of baseline snapshot:** 2026-08-22T20:15:14Z  
**Experiment date:** 2026-08-23.
**Authority:** `docs/plans/RIGHTKIT-THIN-BUILD-CONTROL-v2.8-IMPLEMENTATION.md` §7–§9.

## 1. Baseline snapshot (pre-experiment anchor)

Per v2.7 §12.1 discipline, the motivating cache numbers are persisted here
before any cache-policy change so the before/after comparison has a stable
anchor.

```text
host:                    macOS (MacBook Air, Apple silicon)
toolchain:               stable-1.98.0-aarch64-apple-darwin (managed)
sccache:                 0.17.0, client-side mode (SCCACHE_CLIENT_SIDE=1)
cache dir:               /Volumes/D/.rightkit-managed/rightkit-build-control/global/compiler-cache
SCCACHE_BASEDIRS:        not enabled (capability present, gated off)
CARGO_INCREMENTAL:       unmanaged (Cargo profile default → enabled in dev)
command:                 SCCACHE_DIR=<managed compiler-cache> sccache --show-stats --stats-format=json
snapshot:                2026-08-22T20:15:14Z
```

Observed cumulative counters:

```text
Rust:    31 hits / 6,465 misses  ≈ 0.48% hit rate (5 cache errors)
C/C++:   1,098 hits / 1,263 misses ≈ 46% hit rate
Assembler: 554 hits / 165 misses
```

Caveats (unchanged from v2.7 §12.1): client-side counters are cumulative
process-window counters; production receipts cannot attribute a per-root delta
(`client-side-counters-unattributable`). These figures establish the scale of
the reuse gap, not per-workspace attribution.

## 2. Known structural causes entering the experiments

From upstream sccache documentation (`docs/Rust.md`, `README.md` "Known
Caveats"):

1. **Incremental compilation disables Rust caching.** Cargo's dev profile
   enables incremental for workspace members and path dependencies by default;
   sccache cannot cache incrementally compiled crates. `CARGO_INCREMENTAL` is
   currently unmanaged by the broker — this is the leading candidate for the
   near-zero Rust rate and is exactly what experiment I1–I4 measures.
2. **Linker-invoking crate types cannot be cached** (`bin`, `dylib`, `cdylib`,
   `proc-macro` leaf steps). Product Tauri app crates will always miss; this
   bounds the achievable Rust hit rate and must be stated in conclusions.
3. **Absolute paths participate in the cache key.** RightKit generation
   rotation changes the target path (`.../generations/000012/target` →
   `000013/target`), so rotated builds forfeit reuse unless
   `SCCACHE_BASEDIRS` normalizes the generation root itself (not merely the
   `generations/` parent — the generation number must not remain in the
   relative path). This is what experiment B1–B4 measures.

## 3. Experiment matrix

Hold constant per run: source commit, `Cargo.lock`, target triple, profile,
features, rustflags, toolchain; no competing BUILD; no intentional
cancellation; no runtime restart.

### Incremental (I)

| Run | Generation | Incremental | Measures |
|---|---|---|---|
| I1 | persistent | Cargo default | hot small-edit build/check loop |
| I2 | persistent | `CARGO_INCREMENTAL=0` | same edit loop without incremental |
| I3 | fresh empty, warm sccache | Cargo default | rotated-generation rebuild |
| I4 | fresh empty, warm sccache | `CARGO_INCREMENTAL=0` | rotated rebuild, cacheable local crates |

### Path normalization (B) — run with incremental mode explicitly recorded

| Run | Setup | Basedirs | Measures |
|---|---|---|---|
| B1 | gen 12 build → gen 13 empty build | off | rotation cache loss |
| B2 | same | on (trusted per-job: repo root, workspace root, **generation root**) | rotation reuse restored |
| B3 | second checkout/worktree of same commit | on | cross-checkout reuse |
| B4 | path dependency inside repository but outside workspace root | on | normalization safety for path deps |

### Measurement procedure (per v2.7 §12.1 / v2.8 §7.2)

```text
ensure no competing BUILD (BUILD=1 lane idle)
before = sccache --show-stats --stats-format=json
run exactly one defined build through rightkit cargo
wait for complete root process reap
after = sccache --show-stats --stats-format=json
delta = after - before
```

`--zero-stats` is experiment-only (isolated windows); never a production
attribution mechanism.

Record per run: host, OS, Cargo/rustc/sccache versions, workspace commit,
`Cargo.lock` digest, target triple, profile, feature set, rustflags,
incremental mode, basedirs mode, generation, wall time, rustc count, Rust
hits/misses, non-cacheable count+reasons, C/C++ where relevant, peak RSS,
swap, disk delta where cheap.

## 4. Decision rules (from v2.8 §8.3 / §9.3)

- Incremental: `KEEP CARGO DEFAULT` / `DISABLE INCREMENTAL` / `EXPLICIT CONFIG
  MODE REQUIRED`, chosen from measured edit-loop vs rotated-rebuild tradeoff.
  Only an evidence-backed conclusion may add `defaults.cargoIncremental`;
  agents may never override it.
- Basedirs: enable permanently only if B2 materially improves B1 without
  incorrect cache behavior; otherwise leave gated off and explain remaining
  misses.
- No universal hit-rate target. Acceptance is comparative.

## 5. Results

All completed comparison runs used commit
`322855c33e65dc936e3927570451c98e54fb0bd2` of the Membrane workspace,
`aarch64-apple-darwin`, the managed Rust 1.98 toolchain, sccache 0.17.0,
`cargo check -p membrane-core --locked`, and one admitted BUILD at a time.
The host was already under heavy memory pressure, so decisions use paired
relative timings plus isolated sccache counters rather than treating wall time
as a portable benchmark.

### 5.1 Incremental compilation

| Run | Mode | Generation/work | RightKit request | Run time | Isolated Rust counters |
|---|---|---|---|---:|---|
| I3 | Cargo default | cold/fresh | `d7ab312a-a8b8-4ffa-bd8b-91dfead16bf5` | 286.585 s | 0 hits, 30 misses; 2 incremental non-cacheable |
| I1 | Cargo default | hot comment edit | `09928c83-934c-4d9f-832c-52c1488c58fa` | 4.176 s | wall comparison only |
| I4 | disabled | cold/fresh | `cca8a76c-d75d-4068-b44a-5870a6b0dcfb` | 382.248 s | 0 hits, 32 misses; 12 crate-type non-cacheable |
| I2 | disabled | hot comment edit | `9c1e5e12-6eac-44a5-83e3-94f155a2d792` | 4.976 s | wall comparison only |

Disabling incremental compilation produced no hot-loop improvement and made
the paired cold build about 33% slower. Linker-invoking crate types remained
non-cacheable, while Cargo's persistent generation supplied the useful hot
reuse.

**Decision: `KEEP CARGO DEFAULT`.** RightKit does not inject
`CARGO_INCREMENTAL`, does not add `defaults.cargoIncremental`, and does not
introduce an adaptive per-request mode.

### 5.2 Path normalization

Runs used source-identical snapshots at alternate absolute paths. Basedirs-on
derived trusted roots per admitted job: repository root, Cargo workspace root,
and active target generation. Client input could not supply those roots.

| Run | Basedirs | Setup | RightKit request | Run time | Isolated Rust counters |
|---|---|---|---|---:|---|
| B1 | off | alternate path/fresh generation | `3053df78-73f0-4577-af15-1a6520cb8dd4` | 187.101 s | 0 hits, 30 misses |
| B2 | on | seed at first trusted path | `e81f898f-d454-4d9c-8151-cb5be26225b5` | 144.705 s | 0 hits, 30 misses |
| B3 | on | identical alternate path | `4059acb1-27c4-4999-9cc6-383a0b262075` | 130.538 s | 0 hits, 29 misses |

The alternate-path run produced no Rust cache hit. Its approximately 9.8%
wall-time difference from the basedirs seed is within this host's swap-driven
variance and does not satisfy the material-improvement gate. Remaining misses
are explained by incremental workspace crates, linker-invoking crate types,
and cold dependency compilation rather than a proved path-only loss.

B4 was not promoted to a live experiment because B2/B3 had already failed the
required material-improvement gate, so production activation was forbidden.
Focused tests cover B4's safety property: roots are derived from trusted
repository/workspace/generation state, path dependencies elsewhere in the
repository are admitted, and client-supplied normalization authority is
rejected.

**Decision: leave `sccache.basedirsEnabled` off.** The implementation remains
available behind the operator-owned gate, with trusted roots derived per job.
The temporary live gate was removed and source bootstrap was reactivated with
the accepted off state.

### 5.3 Representative Tauri evidence

- HeardRight completed through the accepted default policy in request
  `5ea13466-712f-42a5-801c-fd1591a88b30` (`runMs=515472`), with zero compiler
  broker round trips and truthful client-side cache attribution.
- GenRight request `d07c8e01-f4a1-4d32-bbb9-fc7d8929d0d7` exercised a cold
  heavy Tauri graph for 628 seconds before controlled cancellation; isolated
  partial counters recorded 46 Rust misses and crate-type/missing-input
  non-cacheable work. This run is structural evidence, not a timing pair.

No production policy change depends on the partial heavy run. The completed
medium matrix provides the comparative decision, while live Tauri work proves
the same accepted topology and telemetry under a large graph.

## 6. Accepted production state

```text
CARGO_INCREMENTAL:       unmanaged; Cargo/profile default
SCCACHE_BASEDIRS:        disabled
sccache mode:            client-side
per-job cache delta:     unavailable by design; receipt says
                         client-side-counters-unattributable
persistent generation:  enabled; primary warm edit-loop reuse
```
