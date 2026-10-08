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
| `system` | on | `rightkit-transcript` | The OS voice backend (`say`/`afconvert` on macOS, SAPI on Windows) and the word-timed transcript. |
| `client` | on | `rightkit-process`, `sha2`, `serde`, `serde_json` | QTS worker client (`QtsClient`, chunking, model manifest checks). |
| `engine` | off | `client`, `qts` (`rightkit-qts`), `libc` on Unix | The Qwen3-TTS QTS worker backend and the `rightkit-tts-worker` binary. |
| `metal` | off | `engine`, `qts/metal` | QTS with the Metal backend (Apple). |
| `vulkan` | off | `engine`, `qts/vulkan` | QTS with the Vulkan backend. |

Environment variables used by the crate:

- `RIGHTKIT_TTS_MODEL_DIR` (legacy `GENRIGHT_QTS_MODEL_DIR`): QTS model directory. The default is `<data>/models/qts`.
- `RIGHTKIT_TTS_WORKER` (legacy `GENRIGHT_QTS_WORKER`): worker binary path.
- `ORT_DYLIB_PATH`: ONNX Runtime library for the vocoder.

## Platform support

- macOS: `system` uses `say` and `afconvert`. `metal` applies to Apple targets.
- Windows: `system` uses SAPI. `vulkan` is available for Windows hosts with a Vulkan SDK.
- Unix: `cfg(unix)` code is in `src/worker.rs`.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`. The crate's manifest sets `publish = false`.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/e2e.rs`. Its fixtures are read from `/private/tmp/gre2e-qts`, a machine-specific path. Check that test before running it on another host.

```rust,ignore
// Render with the OS voice through the system backend (src/system.rs), or
// start the QTS worker with QtsClient::new(idle_secs) when the engine feature is on.
```

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-tts`. Add `--features engine` to cover the QTS worker.
