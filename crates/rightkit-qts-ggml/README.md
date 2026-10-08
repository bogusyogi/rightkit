# rightkit-qts-ggml

> **Deprecated.** Depend on [`rightkit-qts-ggml-sys`](../rightkit-qts-ggml-sys) directly instead. `rightkit-qts` already does. This crate stays published for existing consumers, gets no new API, and receives only compatibility fixes.

Thin safe wrappers over `rightkit-qts-ggml-sys` for `rightkit-qts`. It is vendored from QTS v0.2.0 (see [PINNED.md](PINNED.md)). The library name is `qts_ggml`. The crate exposes only the wrapper surface the TTS stack needs, and its crate docs say it grows as that stack needs more of ggml. It is consumed through `rightkit-qts` and `rightkit-tts`. Apps should not depend on it directly.

**Use it** only as a dependency of `rightkit-qts`.

**Do not use it** for general ggml work. The API covers the TTS stack's needs, not all of ggml.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `metal` | off | `rightkit-qts-ggml-sys/metal` | Metal backend (Apple targets). |
| `vulkan` | off | `rightkit-qts-ggml-sys/vulkan` | Vulkan backend. |
| `blas` | off | `rightkit-qts-ggml-sys/blas` | BLAS backend. |
| `native` | off | `rightkit-qts-ggml-sys/native` | Builds ggml for the host CPU. |

The default feature set is empty. The CPU backend is always built.

## Platform support

Feature availability follows `rightkit-qts-ggml-sys`. `metal` is effective on Apple targets only.

Published version: see `INDEX.md` at the repository root. Release history is in [CHANGELOG.md](CHANGELOG.md); see [PINNED.md](PINNED.md) for the upstream pin.

## Example

Not an app-facing API. Use it through `rightkit-qts`. The crate has no tests or examples of its own.
