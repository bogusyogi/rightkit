# Changelog

## 0.1.0 (unreleased)

- Added `tools/bundle-qualify.mjs` (rightkit#781, #827): qualifies a candidate's COMPLETE bundle (weights, tokenizer, vocoder or reader and port, stats, runtime dependencies) against the 700,000,000-byte cap (target 500,000,000) from sizes alone, before any download. Input is a bundle manifest or, with `--hf owner/repo --revision <sha>`, the Hugging Face tree metadata (names and sizes; no model file is fetched). It refuses a bundle with a required role missing, an unpinned revision, or no recorded license or source, separates package bytes from peak RAM, and exits 0 / 1 / 2 (qualifies / does not / bad input). The measurement runs themselves (first audible response, p95, quality, cancellation, resident memory on the Mac and the Dell) need the downloads Adrian has not approved and are not claimed.
- Private: a scripts-only lab with machine-specific paths and no importable surface, kept in the repo and not published to npm (branch consolidation, 2026-11-03).
- First RightKit release, moved from HeardRight. Env switches `HEARDRIGHT_*` / `HR_*` are now `RIGHTKIT_ML_*`.
