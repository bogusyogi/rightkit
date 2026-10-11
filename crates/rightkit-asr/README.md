# rightkit-asr

Parakeet ONNX and Whisper decoding runtime for Right Suite applications. It reconciles the generic Parakeet ONNX runtime used by Right Suite products into one crate. It is published to crates.io; the registry-recorded version is 0.1.0, and this source is 0.2.0 (see [INDEX.md](../../INDEX.md)).

Catalog capability: `speech-recognition`.

## What it owns

The shared API owns model-format loading, typed ONNX execution configuration (`ExecutionConfig`, `ExecutionProvider`), generic RNN-T/TDT decoding, context bias, timestamps, and the retained DirectML static-conformer correctness workaround (`model_transducer.rs`, `directml` feature).

Product environment-variable compatibility, model choice, model and artifact identity, private manifests, credentials, download policy, long-audio orchestration, experiments and UI stay in application adapters.

## When to use it

- Use it as the shared Parakeet or Whisper decoder when a Right Suite app needs ONNX speech recognition with typed execution settings.

Do not put product policy (model selection, downloads, environment variables) inside it.

## Cargo features

| Feature | Default | Enables |
|---|---|---|
| `cpu` | yes | CPU build marker. It adds no dependency. |
| `whisper-tokenizers` | yes | HF `tokenizers` for the Whisper CoreML decoder, the CTC and EOU Parakeet decoders and Cohere (`Cargo.toml:63`). |
| `ort-defaults` | yes | Nothing. It is an empty feature kept so existing lists resolve (`Cargo.toml:68`). The suite API level lives in `rightkit-ort`; this crate does not use `ort/default`. |
| `ort-tracing` | no | `rightkit-ort/tracing`: forwards ONNX Runtime log lines into `tracing` (`Cargo.toml:71`). |
| `ort-download` | no | `rightkit-ort/download-binaries`, `rightkit-ort/tls-native`, `rightkit-ort/copy-dylibs`: build-time ONNX Runtime fetch (`Cargo.toml:73`). |
| `cuda` | no | `rightkit-ort/cuda` execution provider. |
| `tensorrt` | no | `rightkit-ort/tensorrt` execution provider. |
| `coreml` | no | `rightkit-ort/coreml` execution provider. |
| `directml` | no | `rightkit-ort/directml` execution provider, with the static-conformer graph-fusion workaround. |
| `migraphx` | no | `rightkit-ort/migraphx` execution provider. |
| `openvino` | no | `rightkit-ort/openvino` execution provider. |
| `webgpu` | no | `rightkit-ort/webgpu` execution provider. |
| `nnapi` | no | `rightkit-ort/nnapi` execution provider. |
| `sortformer` | no | Enables the `sortformer` module. |
| `multitalker` | no | Enables the `multitalker` module. It requires `sortformer`. |
| `cohere` | no | Enables the `cohere` module. It also enables `whisper-tokenizers` (`Cargo.toml:84`). |
| `load-dynamic` | no | No-op kept so existing feature lists resolve; `rightkit-ort` always loads ONNX Runtime dynamically (`Cargo.toml:85-86`). |
| `preload-dylibs` | no | `rightkit-ort/preload-dylibs`. |

Unreleased change: the `eyre` dependency is removed. `rightkit-vad` is taken with `default-features = false` (`Cargo.toml:41`), so this crate adds no Silero scorer and no ORT download feature; that crate's own default is `silero` (`rightkit-vad/Cargo.toml:40`). This crate's `ort-defaults` is empty (`Cargo.toml:68`), so the default build does not fetch ONNX Runtime; the fetch is the opt-in `ort-download` feature (`Cargo.toml:73`). Bind the runtime through `rightkit-ort`.

On macOS the crate also depends on `objc2`, `objc2-core-ml` and `objc2-foundation`. The `parakeet_coreml` module compiles only on macOS.

## Usage

Illustrative. The execution-config calls match `tests/runtime_contract.rs`. `ParakeetTDT::from_pretrained(path, config)` takes an optional `ExecutionConfig`, as in `src/parakeet_tdt.rs`:

```rust
use rightkit_asr::{ExecutionConfig, ExecutionProvider, ParakeetTDT};

let config = ExecutionConfig::new().with_execution_provider(ExecutionProvider::Cpu);
let model = ParakeetTDT::from_pretrained("/abs/path/to/model-dir", Some(config))?;
```

## Whisper run control

`WhisperOptions` carries the controls a host needs around a long decode. The CoreML backend differs from whisper.cpp as described after the table:

| Option | Effect |
| --- | --- |
| `cancel: Option<CancelToken>` | Trip from any thread. Stops an in-flight window and the run between windows (CoreML: between windows). Returns `Error::Interrupted` with the finished windows. |
| `progress: Option<ProgressCallback>` | `WhisperProgress { windows_completed, windows_estimated, samples_completed, samples_total, fraction }` per window and from whisper.cpp inside a window. |
| `window_deadline` | Per-window bound (default 180 s, unchanged). |
| `total_deadline` | Whole-recording bound (default none). `recording_deadline(audio_duration(n))` is the old CLI route's 120 s + 4x audio. |
| `detect_deadline` | Bound for language identification (cooperative: checked around the two native calls, which cannot be aborted). |
| `quality: DecodeQuality` | `beam_size`, `best_of`, `carry_context`. Default is greedy, no context. `DecodeQuality::whisper_cli()` is whisper-cli's default decode; see its docs for the source lines. |

On the CoreML backend, `cancel` and `total_deadline` are checked between windows only, and `progress` is reported once per window (`src/whisper/windowing.rs:192`). `window_deadline` is not applied inside a CoreML window, because the CoreML decoder does not read the window deadline (`src/whisper/coreml/decoder/mod.rs:220`). Any `quality` other than the default, text-only mode, token timestamps, `max_len`, `split_on_word` or `detect_language` returns `Error::Config` (`src/whisper/mod.rs`, in `run`).

```rust
use rightkit_asr::{recording_deadline, audio_duration, load_wav_mono_16k, AudioSource, CancelToken, Whisper, WhisperModel, WhisperOptions};

let pcm = load_wav_mono_16k("/abs/path/audio.wav")?;
let cancel = CancelToken::new();
let options = WhisperOptions {
    cancel: Some(cancel.clone()),
    total_deadline: Some(recording_deadline(audio_duration(pcm.len()))),
    ..Default::default()
};
let mut whisper = Whisper::load(model, options)?; // `model`: a WhisperModel, CoreMlDir { dir } or Ggml { library, model }
// whisper.set_cancel(...) swaps the token between recordings on a resident model.
match whisper.transcribe_detailed(AudioSource::Pcm16k(pcm)) {
    Ok(t) => println!("{}", t.text),
    Err(e) if e.interrupted().is_some() => {
        let done = &e.interrupted().unwrap().partial; // windows that finished
        println!("stopped: {}", done.text);
    }
    Err(e) => return Err(e),
}
```

Transcript text: `text` joins trimmed segments with one space, or with nothing for languages in `UNSPACED_LANGUAGES` (`ja`, `zh`, `yue`, `th`, `lo`, `km`); `raw_text` (per segment and whole) is whisper.cpp's text untouched.

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

The `load-dynamic` lane is `--no-default-features` plus a no-op feature (`Cargo.toml:85-86`); `rightkit-ort` loads ONNX Runtime dynamically in every lane, and only `ort-download` fetches it at build time. macOS parity and portable fixtures are still publication gates.

## Version and changes

Release notes: see [CHANGELOG.md](CHANGELOG.md).

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead. This replaces the old `RIGHTKIT_ALLOW_MISSING_MODELS` switch, which made a missing model fail by default.

- `tests/e2e_parakeet.rs` (the `coreml` module, including the `.mlpackage` compile test): the CoreML Parakeet bundle, enabled by `RIGHTKIT_PARAKEET_COREML_DIR` or `RIGHTKIT_PARAKEET_COREML_PKG_DIR` (`.mlpackage`). `onnx_transducer_decodes_the_fixtures_when_the_export_is_present` is ignored and also needs `RIGHTKIT_PARAKEET_ONNX_DIR` and the ORT dylib.
- `tests/transcript_roundtrip.rs`: the CoreML Parakeet bundle, enabled by `RIGHTKIT_PARAKEET_COREML_DIR`.
- `tests/e2e_whisper.rs` (the `coreml` module): the WhisperKit CoreML directory, enabled by `RIGHTKIT_WHISPER_COREML_DIR`. `vad_chunked_decode_retimes_onto_the_source_timeline` also needs `RIGHTKIT_VAD_MODEL` and the ONNX Runtime dylib (`RIGHTKIT_ORT_TEST_DYLIB`).
- `tests/e2e_whisper.rs` (the `cpp` module) and `tests/e2e_whisper_ggml.rs`: whisper.cpp 1.9.3 (the library at `RIGHTKIT_WHISPER_LIB`; `load` refuses any other version, `src/whisper/cpp.rs:25`) plus a multilingual ggml model (`RIGHTKIT_WHISPER_GGML`).
- `src/whisper/cpp.rs`, unit test `new_symbols_resolve_in_pinned_library`: `RIGHTKIT_WHISPER_LIB`.
- `tests/e2e_whisper_ggml.rs` cancel, deadline, progress and `DecodeQuality` tests: the same library and model gate.
- `tests/certify_heardright.rs`: an opt-in run against a live HeardRight speech service, enabled by `RIGHTKIT_ASR_CERTIFY=1`, with the HeardRight Parakeet bundle at `RIGHTKIT_PARAKEET_COREML_DIR` (or its default folder).
