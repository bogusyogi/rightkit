# RightKit ASR characterization

This directory records the pre-extraction comparison of HeardRight's and
ScrapeRight's vendored `parakeet-rs` runtimes. It does not publish, migrate, or
modify either app.

`characterize.mjs` generates two isolated temporary Cargo projects so the two
forks can retain their identical package name/version without colliding in one
lockfile. Both probes share Cargo's target cache, load the same model directory,
resample the same mono WAVs deterministically to 16 kHz, and compare normalized
transcripts. The emitted JSON includes every model-component and fixture SHA-256.

Run the unit contract:

```powershell
node --test rightkit/characterization/asr/characterize.test.mjs
```

The runtime probe requires `ORT_DYLIB_PATH` and `PATH` to include the signed
Right Suite ONNX Runtime/DirectML directory. Pass each vendored crate, one model
directory, and one or more `--fixture` arguments. Cargo build output is resumable
under `.cache/rightkit-asr-characterization/`; the app trees remain read-only.

Current decision and evidence: `RECONCILIATION-2026-07-14.md` and
`evidence/windows-2026-07-14.json`.
