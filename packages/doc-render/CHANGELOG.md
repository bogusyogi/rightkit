# Changelog

## 0.1.2 — 2026-10-07

- Every relative import in `dist` carries its `.js` extension (compiled with NodeNext), so the package loads under plain Node ESM (e.g. Vitest without inlining).
- The Shiki Oniguruma WASM URL (`shiki/onig.wasm?url`) is imported lazily on first highlighter build, not at module load; bundlers still emit the asset. `configureShikiWasm(url)` lets a host serve the WASM itself.

## 0.1.1 — 2026-10-07

- `buildBlocks(nodes, { hintPrefixes })` limits which `<!-- PREFIX: KIND -->` comments count as explicit hints (default `["doc", "viewright"]`). ViewRight passes `["viewright"]` for exact parity.
