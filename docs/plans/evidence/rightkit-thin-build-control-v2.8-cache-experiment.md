# RightKit Thin Build Control v2.8 — Cache Experiment Record

**Status:** baseline persisted; experiments pending.  
**Date of baseline snapshot:** 2026-08-22T20:15:14Z  
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

## 3. Experiment matrix (defined, not yet run)

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

Pending. Experiments require representative product workspaces (one heavy,
one medium; at least one Tauri app) and dedicated machine time; they were not
run during the v2.8 implementation pass. Populate this section before any
production cache-policy activation.
