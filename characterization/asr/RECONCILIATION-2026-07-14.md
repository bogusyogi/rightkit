# Parakeet runtime reconciliation — 2026-07-14

Status: Phase 6 steps 1–3 complete on Windows. The reconciled internal
`crates/rightkit-asr` derivative is `publish = false`; no app migration or
publication has started.

## Baseline and runtime result

Both trees originate from crates.io `parakeet-rs 0.3.5`, whose packaged VCS
record points to upstream commit `7d680db3d62e6aeaf68bc93aaae0ab891a09fae7`
and tag `v0.3.5` in `altunenes/parakeet-rs`. The two app trees have 20
byte-identical top-level source files, seven differing top-level source files,
and eight additional HeardRight split-section files.

The common Unified model copies are byte-identical across `config.json`,
encoder, decoder/joint, mel graph/data, tokenizer, and vocabulary. On Windows
DirectML, both current source trees produced identical normalized transcripts
for both available local WAV fixtures. Exact hashes and transcripts are in
`evidence/windows-2026-07-14.json`.

The 20 identical files are `audio.rs`, `cohere.rs`, `config.rs`,
`decoder_tdt.rs`, `error.rs`, `model_cohere.rs`, `model_eou.rs`,
`model_multitalker.rs`, `model_nemotron.rs`, `model_tdt.rs`, `model_unified.rs`,
`model.rs`, `multitalker.rs`, `parakeet_eou.rs`, `parakeet_tdt.rs`,
`parakeet.rs`, `sortformer.rs`, `timestamps.rs`, `transcriber.rs`, and
`vocab.rs`.

## Seven differing top-level files

| File | Observed difference | Reconciliation decision |
|---|---|---|
| `decoder.rs` | Comment wording only; executable code is identical. | Use upstream wording; no fork behavior to preserve. |
| `execution.rs` | Both add DirectML device selection through `HR_DML_DEVICE_ID`; HeardRight additionally makes graph optimization configurable and defaults to Level 3. | Keep the configurable optimization level and device selection, but expose app-neutral typed configuration in `rightkit-asr`; legacy `HR_*` environment translation belongs in the HeardRight adapter. Propose the generic controls upstream. |
| `lib.rs` | ScrapeRight retains upstream optional Cohere/multitalker/sortformer exports, exports timestamp grouping, and exposes `ScrapeRightTransducer`; HeardRight prunes those optional exports and exposes `HrTransducer`. | Preserve the upstream optional feature surface and timestamp helper. Export one app-neutral `Transducer`; app adapters may temporarily alias the old branded names. |
| `model_transducer.rs` | HeardRight replaces the body with three includes. Its reconstructed body adds the Level-1 DirectML static-conformer workaround and expanded decoder-placement rationale; ScrapeRight has the older monolith without Level 1. | Rejoin one source file and keep HeardRight's correctness workaround. Replace `HR_TDT_*`/`HR_RNNT_*` environment reads with typed options plus adapter compatibility. Contribute the reproducible DirectML fix upstream before owning it indefinitely. |
| `nemotron.rs` | HeardRight replaces an otherwise equivalent monolith with two include sections; only split-boundary blank lines differ. | Rejoin the upstream-shaped monolith; no behavioral fork is justified. |
| `parakeet_unified.rs` | HeardRight replaces an otherwise equivalent monolith with three include sections; only split-boundary blank lines differ. | Rejoin the upstream-shaped monolith; no behavioral fork is justified. |
| `transducer.rs` | App-branded type/docs differ. ScrapeRight additionally exposes `transcribe_with_timestamps` and imports `process_timestamps`. | Use an app-neutral type and retain the timestamp method. Keep app-specific long-audio orchestration and product policy outside the shared crate. |

## Eight HeardRight-only split sections

| Section | Content | Decision |
|---|---|---|
| `model_transducer_sections/section01.rs` | Core transducer types, context bias, model state, and loader opening. | Rejoin `model_transducer.rs`; retain generic context-bias and loader behavior. |
| `model_transducer_sections/section02.rs` | Loader completion, DirectML Level-1 workaround, static-window detection/decoding, encoder and greedy-decode paths. | Rejoin; retain and upstream the correctness fix; replace app-prefixed environment policy with typed options. |
| `model_transducer_sections/section03.rs` | Argmax/log-softmax helpers and context-bias unit tests. | Rejoin unchanged; keep tests. |
| `nemotron_sections/section01.rs` | Vocabulary/config/model/handle definitions and loading. | Mechanical split only; rejoin upstream layout. |
| `nemotron_sections/section02.rs` | Stateful streaming/offline Nemotron runtime and tests. | Mechanical split only; rejoin upstream layout. |
| `parakeet_unified_sections/section01.rs` | Streaming config, model/handle definitions, and loading. | Mechanical split only; rejoin upstream layout. |
| `parakeet_unified_sections/section02.rs` | Unified streaming/offline decode implementation. | Mechanical split only; rejoin upstream layout. |
| `parakeet_unified_sections/section03.rs` | `Transcriber` implementation and streaming-profile test. | Mechanical split only; rejoin upstream layout. |

## Complete local modification inventory versus upstream 0.3.5

Shared by both app copies:

- `Cargo.toml`: adds `half` and ORT's `half` feature.
- `decoder.rs`: comment-only edit.
- `execution.rs`: DirectML device selection from an HeardRight-prefixed environment variable.
- `model_nemotron.rs`: optional CPU decoder/joint placement via
  `HR_NEMO_DECODER_EP`.
- `vocab.rs`: deterministic existing-piece phrase encoding, normalization, and
  two context-bias tests.
- `model_transducer.rs` and `transducer.rs`: added offline RNN-T/TDT runtime,
  static-window decode, context bias, mel graph support, and app-branded wrapper.
- `lib.rs`: wires the added transducer modules/exports.
- `Cargo.lock` is generated state; `rustfmt.toml` is tooling-only.

HeardRight-only deltas are the configurable graph-optimization field, the
Level-1 DirectML workaround, optional-feature pruning, branded `HrTransducer`
surface, and eight-way mechanical source splitting. ScrapeRight-only deltas are
the `ScrapeRightTransducer` name and public timestamp grouping/method.

## License, provenance, and ownership decision

The upstream package metadata declares `MIT OR Apache-2.0`; the distributed
crate contains the MIT text and copyright `2025 Enes Altun`, and both vendored
copies retain the identical MIT file. The future shared crate must retain that
copyright/license text, name upstream and commit `7d680db3...`, identify local
modifications, and include third-party notices in source and shipped attribution.
Model weights/manifests remain separate private artifacts and require their own
license record; the Rust crate license does not grant model rights.

Disposition: upstream-first, with a temporary owned derivative only for changes
that upstream has not accepted. Open focused upstream contributions for the
generic DirectML optimization/device configuration, static-conformer Level-1
workaround, phrase encoding/context bias, hybrid decoder placement, and generic
transducer/timestamp API. Do not publish a renamed `rightkit-asr` fork until the
code is reconciled, app-prefixed policy is removed, attribution is bundled, and
the upstream submissions or explicit rejection records exist. If a derivative
must ship, license it under MIT using the already-bundled grant (or add the full
Apache-2.0 text before claiming the dual option) and maintain a machine-readable
upstream-base plus patch ledger.

## Residual gates

- Repeat the same fixture/model digest probe on macOS against both current
  runtime paths; Windows parity does not prove CoreML parity.
- Promote at least two non-synthetic, tracked, redistribution-safe audio fixtures.
  Today's WAVs are ignored local QA assets, so their SHA-256 values are evidence
  but not yet portable inputs.
- Characterize long audio crossing the 15-second boundary, context-bias phrases,
  timestamps, cancellation, and failure cases before selecting the extracted API.
- No source migration or registry publication is authorized by this record.
