# Changelog

## 0.2.0 (private, not published)

Taken on the recommended option of the shape review (decision 8), pending the owner's confirmation.

- BREAKING: the package is the reusable ASR tooling only, with a manifest and entry points. Kept and moved:
  the bake-off adapters (`asr-bakeoff/adapters`), the WER scorer (`wer/score-asr-wer.mjs`, bin `rk-asr-wer`),
  the bundle-size qualifier (`asr-bakeoff/bundle-qualify.mjs`, bin `rk-bundle-qualify`) and the CoreML
  conversion and benchmark scripts (`coreml/`). New: `asr-bakeoff/CONTRACT.md` (the adapters' and scorer's
  shared contract, which the adapters referred to but which was never moved), `requirements/asr-adapters.txt`
  and `requirements/coreml.txt` (Python manifests), `package.json` `bin`, `exports` and `test`.
- No machine paths: adapter model and output directories default to `./.cache/asr-models/<model>` and
  `./out/<model>` under the working directory (were `parents[5]` and `parents[2]` of the script, which
  pointed outside the package); the VAD converter takes the ONNX path as an argument (was HeardRight's
  `src-tauri/resources`); the scorer no longer hashes a HeardRight Python file and has no built-in
  vocabulary (`--terms`, `--term-group` replace the six HeardRight terms and the `scripts_md` group; its
  output keys are `exact_term_recovery` and `term_group`).
- Removed from RightKit (they stay with HeardRight): the dated experiment folders under `bakeoff/`
  (17), local-model training and screening, polish replay, bias experiments, Parakeet streaming
  benches, wake-word and corpus tools under `tools/`, the bake-off's HeardRight control runner, report
  and acquisition roster, the productivity-corpus builders, the Cohere quantization smoke scripts, and the
  CoreML diagnostics bound to HeardRight's model registry (`transcribe_one`, `pico_eval`, `norm_fair`,
  `record`, `dump_f32`, `repeat`, `diagnose`, `export_fused_hypothesis_probe`,
  `prepare_production_model_archive`, `vad/parity_check`) and the CoreML native shims.
- The plan's crate name `rightkit-ml-lab` names this package; there is no crate.

## 0.1.0 (unreleased)

- Added `tools/bundle-qualify.mjs` (rightkit#781, #827): qualifies a candidate's COMPLETE bundle (weights, tokenizer, vocoder or reader and port, stats, runtime dependencies) against the 700,000,000-byte cap (target 500,000,000) from sizes alone, before any download. Input is a bundle manifest or, with `--hf owner/repo --revision <sha>`, the Hugging Face tree metadata (names and sizes; no model file is fetched). It refuses a bundle with a required role missing, an unpinned revision, or no recorded license or source, separates package bytes from peak RAM, and exits 0 / 1 / 2 (qualifies / does not / bad input). The measurement runs themselves (first audible response, p95, quality, cancellation, resident memory on the Mac and the Dell) need the downloads Adrian has not approved and are not claimed.
- Private: a scripts-only lab with machine-specific paths and no importable surface, kept in the repo and not published to npm (branch consolidation, 2026-11-03).
- First RightKit release, moved from HeardRight. Env switches `HEARDRIGHT_*` / `HR_*` are now `RIGHTKIT_ML_*`.
