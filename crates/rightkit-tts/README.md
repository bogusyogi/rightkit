# rightkit-tts

Shared local text-to-speech for Right Suite apps. It owns two backends and the worker that runs one of them:

- `system`: renders to an isolated WAV file through macOS `say` and `afconvert`, or Windows SAPI (`src/system.rs`). It never plays audio.
- `engine`: the Qwen3-TTS QTS worker, through `rightkit-qts`. `QtsClient` owns the worker lifetime and its idle unload policy (`QtsClient::new(idle_secs)`). The worker is the `rightkit-tts-worker` binary.

Both return PCM WAV plus a word-timed manifest that can be passed to `rightkit-transcript`.

**Use it** when an app needs local speech output: the OS voice with no model, or the QTS model with the worker.

**Do not use it** to play audio, to choose voices in the UI, or to download models. Model files and the worker path are supplied by the app or by environment variables.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `system` | on | `rightkit-transcript`, `thiserror` | The OS voice backend (`say`/`afconvert` on macOS, SAPI on Windows) and the word-timed transcript. |
| `client` | on | `rightkit-ort`, `rightkit-process`, `rightkit-fs` (`digest`), `serde`, `serde_json` | QTS worker client (`QtsClient`, chunking, model manifest checks). |
| `engine` | off | `client`, `qts` (`rightkit-qts`), `libc` on Unix | The Qwen3-TTS QTS worker backend and the `rightkit-tts-worker` binary. |
| `metal` | off | `engine`, `qts/metal` | QTS with the Metal backend (Apple). |
| `vulkan` | off | `engine`, `qts/vulkan` | QTS with the Vulkan backend. |

Environment variables used by the crate:

- `RIGHTKIT_TTS_MODEL_DIR` (legacy `GENRIGHT_QTS_MODEL_DIR`): QTS model directory. The default is `<data>/models/qts`.
- `RIGHTKIT_TTS_WORKER` (legacy `GENRIGHT_QTS_WORKER`): worker binary path.
- `ORT_DYLIB_PATH`: ONNX Runtime library for the vocoder. Debug builds only: release builds never read it, and
  the client strips it from the worker's environment. Release callers pass the runtime with
  `QtsClient::with_ort_runtime(path)`, or the installed runtime is found by search.

Caller-owned locations: `QtsClient::with_data_root(path)` resolves models, voices and the worker's runtime
under `path` (absolute). `RIGHTKIT_TTS_MODEL_DIR` still overrides the model folder.

Explicit setters beat the environment: `with_model_dir`, `with_vocoder`, `with_mode_location(ModeLocation)` /
`with_mode_model_dir(mode, dir)` (each mode may live in its own folder), `with_worker_exe(path)` (exact; absent is
`worker_missing`, never a fall back; `worker_exe()` reports what a launch would use), `with_idle_secs(n)` (beats
`RIGHTKIT_TTS_IDLE_SECS` / `GENRIGHT_QTS_IDLE_SECS`; `0` is a zero window). The worker keeps one engine per mode and
does not respawn when the mode changes. `QtsClient::engine_info(backend)` uses the client's own configuration.

Model modes: `SynthOptions::mode` selects `Base` (0.6B, the default), `CustomVoice` (speaker) or
`VoiceDesign` (instruct). The 1.7B modes need their exports in the model folder.

## Platform support

- macOS: `system` uses `say` and `afconvert`. `metal` applies to Apple targets.
- Windows: `system` uses SAPI. `vulkan` is available for Windows hosts with a Vulkan SDK.
- Unix: `cfg(unix)` code is in `src/worker.rs`.

Release notes: see `CHANGELOG.md` in this crate.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/e2e.rs`. Its fixtures are read from `/private/tmp/gre2e-qts`, a machine-specific path. Check that test before running it on another host.

```rust,ignore
// Render with the OS voice through the system backend (src/system.rs), or
// start the QTS worker with QtsClient::new(idle_secs) when the engine feature is on.
```

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-tts`. Add `--features engine` to cover the QTS worker.

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead.

- `tests/e2e.rs`, `qts_real_synthesis_writes_non_silent_audio_and_transcript` (run with `--features engine`): needs the QTS model directory `/private/tmp/gre2e-qts/models/qts` and ONNX Runtime `/private/tmp/gre2e-qts/runtime/qts/libonnxruntime.dylib`. Enabled by staging both there.
