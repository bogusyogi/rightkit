# rightkit-asr

Internal, unpublished Parakeet ONNX and Whisper decoding runtime for Right Suite applications. It reconciles the generic Parakeet ONNX runtime used by Right Suite products into one crate. It is a path-dependency characterization target: applications have not migrated to it, and it must not be published yet (`publish = false`).

Catalog capability: `speech-recognition`.

## What it owns

The shared API owns model-format loading, typed ONNX execution configuration (`ExecutionConfig`, `ExecutionProvider`), generic RNN-T/TDT decoding, context bias, timestamps, and the retained DirectML static-conformer correctness workaround (`model_transducer.rs`, `directml` feature).

Product environment-variable compatibility, model choice, model and artifact identity, private manifests, credentials, download policy, long-audio orchestration, experiments and UI stay in application adapters.

## When to use it

- Use it as the shared Parakeet or Whisper decoder when a Right Suite app needs ONNX speech recognition with typed execution settings.

Do not use it from a published product yet, and do not put product policy (model selection, downloads, environment variables) inside it.

## Cargo features

| Feature | Default | Enables |
|---|---|---|
| `cpu` | yes | CPU build marker. It adds no dependency. |
| `ort-defaults` | yes | `ort/std`, `ort/ndarray`, `ort/tracing`, `ort/download-binaries`, `ort/tls-native`, `ort/copy-dylibs`, `ort/api-24`. Keeps the suite API level and does not use `ort/default`. |
| `cuda` | no | `ort/cuda` execution provider. |
| `tensorrt` | no | `ort/tensorrt` execution provider. |
| `coreml` | no | `ort/coreml` execution provider. |
| `directml` | no | `ort/directml` execution provider, with the static-conformer graph-fusion workaround. |
| `migraphx` | no | `ort/migraphx` execution provider. |
| `openvino` | no | `ort/openvino` execution provider. |
| `webgpu` | no | `ort/webgpu` execution provider. |
| `nnapi` | no | `ort/nnapi` execution provider. |
| `sortformer` | no | Enables the `sortformer` module. |
| `multitalker` | no | Enables the `multitalker` module. It requires `sortformer`. |
| `cohere` | no | Enables the `cohere` module. |
| `load-dynamic` | no | `ort/load-dynamic`: loads the ONNX Runtime library at run time. The test lane below uses it. |
| `preload-dylibs` | no | `ort/preload-dylibs`. |

On macOS the crate also depends on `objc2`, `objc2-core-ml` and `objc2-foundation`. The `parakeet_coreml` module compiles only on macOS.

## Usage

Illustrative. The execution-config calls match `tests/runtime_contract.rs`. `ParakeetTDT::from_pretrained(path, config)` takes an optional `ExecutionConfig`, as in `src/parakeet_tdt.rs`:

```rust
use rightkit_asr::{ExecutionConfig, ExecutionProvider, ParakeetTDT};

let config = ExecutionConfig::new().with_execution_provider(ExecutionProvider::Cpu);
let model = ParakeetTDT::from_pretrained("/abs/path/to/model-dir", Some(config))?;
```

## Model requirements

The Parakeet TDT model directory contains `encoder-model.onnx`, `decoder_joint-model.onnx` and `vocab.txt` (see `ParakeetTDT::from_pretrained`). The crate docs describe a FastConformer-TDT directory with `model.onnx`, `model.onnx_data`, `config.json`, `preprocessor_config.json`, `tokenizer.json` and `tokenizer_config.json`. Audio input is 16 kHz mono.

## Parakeet CoreML path and HeardRight decode parity

macOS runs the 4-stage static Parakeet TDT bundle in-process on CoreML (`CoreMlParakeet`); ONNX keeps the `Transducer`. Both share `parakeet_core` (HeardRight beam, bias, detokenizer, policy) and `longform` (padded-window schedule, cancel/progress, SRT/VTT/JSON). See `tests/e2e_parakeet.rs` for the model gates and the files each one needs.

## Provenance and license

This derivative is based on crates.io `parakeet-rs 0.3.5`, upstream commit `7d680db3d62e6aeaf68bc93aaae0ab891a09fae7` (`altunenes/parakeet-rs`, tag `v0.3.5`). See `UPSTREAM.toml`, `PATCHES.md`, `THIRD_PARTY_NOTICES.md`, and the preserved upstream `LICENSE`. The derivative currently selects the upstream MIT grant. Model artifacts are separate works and are not licensed by this crate.

## Platform support

- macOS: ONNX Runtime, and CoreML through `parakeet_coreml`. macOS parity is still a publication gate.
- Windows: ONNX Runtime. The `directml` feature adds the DirectML path. Windows inference parity is recorded under `characterization/asr/` in the RightKit tools tree.
- Other platforms: not documented for this crate.

## Verification

Run through the managed Rust wrapper (see `docs/rules/rightkit.md`):

```text
rightkit cargo test -p rightkit-asr --no-default-features --features load-dynamic --all-targets
rightkit cargo clippy -p rightkit-asr --no-default-features --features load-dynamic --all-targets -- -D warnings
rightkit cargo fmt --check
rightkit cargo package -p rightkit-asr --allow-dirty --no-verify --list
```

The `load-dynamic` lane avoids linking the downloaded ONNX Runtime static library into test binaries. macOS parity and portable fixtures are still publication gates.

## Version and changes

Not published. See [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
