# Local patch ledger

Baseline: `parakeet-rs 0.3.5` at
`7d680db3d62e6aeaf68bc93aaae0ab891a09fae7`. This ledger covers every source
file that differs from the packaged upstream baseline.

| ID | Files | Local change and reason | Upstream disposition |
|---|---|---|---|
| RKASR-001 | `src/decoder.rs` | Comment wording only; executable decoder code is unchanged. | Restore upstream wording when the surrounding decoder patch is refreshed. |
| RKASR-002 | `src/execution.rs` | Add typed graph-optimization level, device index, and auxiliary-provider configuration. Removes application-prefixed environment reads while retaining CPU fallback and all upstream providers. | Propose generic provider/device/optimization controls upstream. |
| RKASR-003 | `src/model_nemotron.rs` | Route the small decoder/joint graph through the typed auxiliary provider instead of an application-prefixed environment variable. | Propose generic hybrid placement upstream. |
| RKASR-004 | `src/vocab.rs` | Add deterministic normalization and greedy existing-piece phrase encoding with context-bias tests. It never creates token IDs absent from the fixed vocabulary. | Propose phrase encoding/context bias upstream. |
| RKASR-005 | `src/model_transducer.rs`, `src/transducer.rs`, `src/lib.rs` | Add the generic offline RNN-T/TDT runtime, NeMo mel-stage support, static-window decoding, DirectML graph-fusion workaround selected from the model graph, context bias, timestamp grouping, app-neutral `Transducer`, and typed `TransducerOptions`. The previous mechanical source splits were rejoined. | Submit focused generic transducer/timestamp and DirectML correctness patches upstream; retain locally only until acceptance or recorded rejection. |
| RKASR-006 | `Cargo.toml`, `tests/runtime_contract.rs` | Rename the internal derivative, set `publish = false`, retain upstream features/dependencies plus required `half` support, bundle notices/provenance, and lock the app-neutral shared API contract. | RightKit-only packaging; not an upstream source change. |

No model name, model digest, weight, private URL, credential, application tier,
or product orchestration policy is part of this derivative. The two current
applications remain unchanged until the full Phase 6 gate passes.
