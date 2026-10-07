# Changelog

## 0.1.1 — 2026-10-07

- `buildBlocks(nodes, { hintPrefixes })` limits which `<!-- PREFIX: KIND -->` comments count as explicit hints (default `["doc", "viewright"]`). ViewRight passes `["viewright"]` for exact parity.
