# rightkit-asr

Parakeet ONNX and Whisper decoding runtime for Right Suite applications. It reconciles the generic Parakeet ONNX runtime used by Right Suite products into one crate. It is published to crates.io; the latest registry version is 0.2.4, and this source is 0.3.0, unpublished (see [INDEX.md](../../INDEX.md)).

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
| `ort-defaults` | yes | Nothing. It is an empty feature kept so existing lists resolve (`Cargo.toml:68`). The suite API level lives in `rightkit-onnx`; this crate does not use `ort/default`. |
| `ort-tracing` | no | `rightkit-onnx/tracing`: forwards ONNX Runtime log lines into `tracing` (`Cargo.toml:71`). |
| `ort-download` | no | `rightkit-onnx/download-binaries`, `rightkit-onnx/tls-native`, `rightkit-onnx/copy-dylibs`: build-time ONNX Runtime fetch (`Cargo.toml:73`). |
| `cuda` | no | `rightkit-onnx/cuda` execution provider. |
| `tensorrt` | no | `rightkit-onnx/tensorrt` execution provider. |
| `coreml` | no | `rightkit-onnx/coreml` execution provider. |
| `directml` | no | `rightkit-onnx/directml` execution provider, with the static-conformer graph-fusion workaround. |
| `migraphx` | no | `rightkit-onnx/migraphx` execution provider. |
| `openvino` | no | `rightkit-onnx/openvino` execution provider. |
| `webgpu` | no | `rightkit-onnx/webgpu` execution provider. |
| `nnapi` | no | `rightkit-onnx/nnapi` execution provider. |
| `xnnpack` | no | `rightkit-onnx/xnnpack`: adds `ExecutionProvider::Xnnpack` (Android; unverified). |
| `sortformer` | no | Enables the `sortformer` module. |
| `multitalker` | no | Enables the `multitalker` module. It requires `sortformer`. |
| `cohere` | no | Enables the `cohere` module. It also enables `whisper-tokenizers` (`Cargo.toml:84`). |
| `load-dynamic` | no | No-op kept so existing feature lists resolve; `rightkit-onnx` always loads ONNX Runtime dynamically (`Cargo.toml:85-86`). |
| `preload-dylibs` | no | `rightkit-onnx/preload-dylibs`. |

Unreleased change: the `eyre` dependency is removed. `rightkit-vad` is taken with `default-features = false` (`Cargo.toml:41`), so this crate adds no Silero scorer and no ORT download feature; that crate's own default is `silero` (`rightkit-vad/Cargo.toml:40`). This crate's `ort-defaults` is empty (`Cargo.toml:68`), so the default build does not fetch ONNX Runtime; the fetch is the opt-in `ort-download` feature (`Cargo.toml:73`). Bind the runtime through `rightkit-onnx`.

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

## Verified whisper.cpp load

`WhisperCpp::load_verified(library, model, threads, deadline)` and `Whisper::load_verified(model, options)` are the loads for a shipped app. Before anything is opened they check the whisper.cpp library and every ggml file the catalog lists for this host beside it (`libggml*`) against the embedded native-library catalog of `rightkit-ffmpeg`, all or nothing, and then load the canonical path that was verified. A file that is not the catalog's (a different digest, an unlisted name, a symbolic link, an absent sibling) or a row that is still `planned` is a `VerifiedLoadError::Verification` and nothing is mapped; a file that matched but would not load is a `VerifiedLoadError::Load`. `library` must be an absolute path. The digests are checked at the call, not while the loader maps the files, so keep them in a directory only the app can write. The model file is not part of the check, and `WhisperModel::CoreMlDir` has no native library of ours and loads as `Whisper::load` does. `WhisperCpp::load` and `Whisper::load` stay the unverified path for tests and for development builds with a locally built library.

Windows: the catalog's whisper.cpp and ggml rows for Windows are `planned` (no digest yet), so `load_verified` fails closed there today. When they are pinned, a Windows host still has to prove the load: the loaded path is the canonical path, a verbatim `\\?\C:\...` path on Windows, which has not been run through the loader there.

## Keyword spotting (`kws`)

`rightkit_asr::kws` spots fixed keyword phrases in streaming 16 kHz audio with a Sherpa-ONNX Zipformer2 KWS transducer (for example `sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20`): `encoder-*.onnx`, `decoder-*.onnx`, `joiner-*.onnx`, `tokens.txt` and a keyword list in Sherpa's format (`Z EH1 F ER0 S T AA1 P :2 #0.15 @ZEPHYR_STOP`). It is a port of the Sherpa-ONNX 1.13.4 spotter; see the module docs for the API and `CHANGELOG.md` for the parity record.

- The app owns command policy (which keyword sets, audio windows, when to reset a stream); the crate owns features, the encoder cache, the beam and the keyword graph.
- `active_path_ttl_frames` and `trailing_blank_reset` replace patches some apps carried on Sherpa. Upstream behaviour is `active_path_ttl_frames: None` with `trailing_blank_reset: true`; the defaults never reset a stream silently.
- CPU only (`threads` sets intra-op threads per session). Bind ONNX Runtime through `rightkit-onnx` before `KeywordSpotter::new`.

## Model requirements

The Parakeet TDT model directory contains `encoder-model.onnx`, `decoder_joint-model.onnx` and `vocab.txt` (see `ParakeetTDT::from_pretrained`). The crate docs describe a FastConformer-TDT directory with `model.onnx`, `model.onnx_data`, `config.json`, `preprocessor_config.json`, `tokenizer.json` and `tokenizer_config.json`. Audio input is 16 kHz mono.

## Parakeet CoreML path and HeardRight decode parity

macOS runs the 4-stage static Parakeet TDT bundle in-process on CoreML (`CoreMlParakeet`); ONNX keeps the `Transducer`. Both share `parakeet_core` (HeardRight beam, bias, detokenizer, policy) and `longform` (padded-window schedule, cancel/progress, SRT/VTT/JSON). See `tests/e2e_parakeet.rs` for the model gates and the files each one needs.

## Provenance and license

The `kws` module is a port of Sherpa-ONNX 1.13.4 and kaldi-native-fbank 1.22.3 (both Apache-2.0) and KISS FFT (BSD-3-Clause); see `THIRD_PARTY_NOTICES.md`. This derivative is based on crates.io `parakeet-rs 0.3.5`, upstream commit `7d680db3d62e6aeaf68bc93aaae0ab891a09fae7` (`altunenes/parakeet-rs`, tag `v0.3.5`). See `UPSTREAM.toml`, `PATCHES.md`, `THIRD_PARTY_NOTICES.md`, and the preserved upstream `LICENSE`. The derivative currently selects the upstream MIT grant. Model artifacts are separate works and are not licensed by this crate.

## Platform support

- macOS: ONNX Runtime, and CoreML through `parakeet_coreml`. macOS parity is still a publication gate.
- Windows: ONNX Runtime. The `directml` feature adds the DirectML path. Windows inference parity is recorded under `characterization/asr/` in the RightKit tools tree.
- Android (planned, nothing compiled or run): the Parakeet ONNX engines only. All Apple code is behind `cfg(target_os = "macos")` and is not compiled; `EngineSupport::current()` reports what the build hosts. whisper.cpp (`WhisperCpp::load`, `WhisperModel::Ggml`) returns an `Error::unsupported` error on Android because Whisper on Android is deferred. Optional `xnnpack` feature for the CPU path. Decode policy defaults on Android are the macOS-tuned ones until measured.
- Other platforms: not documented for this crate.

## Verification

Run through the managed Rust wrapper (see `docs/rules/rightkit.md`):

```text
rustright cargo test -p rightkit-asr --no-default-features --features load-dynamic --all-targets
rustright cargo clippy -p rightkit-asr --no-default-features --features load-dynamic --all-targets -- -D warnings
rustright cargo fmt --check
rustright cargo package -p rightkit-asr --allow-dirty --no-verify --list
```

The `load-dynamic` lane is `--no-default-features` plus a no-op feature (`Cargo.toml:85-86`); `rightkit-onnx` loads ONNX Runtime dynamically in every lane, and only `ort-download` fetches it at build time. macOS parity and portable fixtures are still publication gates.

## Version and changes

Release notes: see [CHANGELOG.md](CHANGELOG.md).

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead. This replaces the old `RIGHTKIT_ALLOW_MISSING_MODELS` switch, which made a missing model fail by default.

- `tests/e2e_parakeet.rs` (the `coreml` module, including the `.mlpackage` compile test): the CoreML Parakeet bundle, enabled by `RIGHTKIT_PARAKEET_COREML_DIR` or `RIGHTKIT_PARAKEET_COREML_PKG_DIR` (`.mlpackage`). `onnx_transducer_decodes_the_fixtures_when_the_export_is_present` is ignored and also needs `RK_PARAKEET_ONNX_DIR` and the ORT dylib.
- `tests/transcript_roundtrip.rs`: the CoreML Parakeet bundle, enabled by `RIGHTKIT_PARAKEET_COREML_DIR`.
- `tests/e2e_whisper.rs` (the `coreml` module): the WhisperKit CoreML directory, enabled by `RIGHTKIT_WHISPER_COREML_DIR`. `vad_chunked_decode_retimes_onto_the_source_timeline` also needs `RIGHTKIT_VAD_MODEL` and the ONNX Runtime dylib (`RIGHTKIT_ORT_TEST_DYLIB`).
- `tests/e2e_whisper.rs` (the `cpp` module) and `tests/e2e_whisper_ggml.rs`: whisper.cpp 1.9.3 (the library at `RIGHTKIT_WHISPER_LIB`; `load` refuses any other version, `src/whisper/cpp.rs:25`) plus a multilingual ggml model (`RIGHTKIT_WHISPER_GGML`).
- `src/whisper/cpp.rs`, unit test `new_symbols_resolve_in_pinned_library`: `RIGHTKIT_WHISPER_LIB`.
- `tests/e2e_whisper_ggml.rs` cancel, deadline, progress and `DecodeQuality` tests: the same library and model gate.
- `tests/certify_heardright.rs`: an opt-in run against a live HeardRight speech service, enabled by `RIGHTKIT_ASR_CERTIFY=1`, with the HeardRight Parakeet bundle at `RIGHTKIT_PARAKEET_COREML_DIR` (or its default folder).
