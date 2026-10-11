# rightkit-qts-ggml-sys

Low-level FFI bindings to ggml for `rightkit-qts`. It is vendored from QTS v0.2.0 (see [PINNED.md](PINNED.md)); ggml is vendored as plain files at `vendor/ggml`, not as a git submodule. Its `CMakeLists.txt` reports version 0.9.8. [VERSIONS.md](VERSIONS.md) gives the correct vendored path and version, but its build matrix lists backends this crate does not declare (`cuda`, `hip`, and others) and says default features enable Metal and Vulkan, while `Cargo.toml:18` sets `default = []`. The feature table below and `Cargo.toml` are the reference. The crate builds ggml as a static library with CMake (through the `cmake` build dependency) and copies checked-in bindings. The Cargo `links` key is `rightkit_ggml`.

**Use it** only as a dependency of `rightkit-qts-ggml`.

**Do not use it** directly in app code. The API is raw FFI. Use `rightkit-qts-ggml` for safe wrappers.

## Features

| Feature | Default | Effect |
|---|---|---|
| `metal` | off | Builds ggml with `GGML_METAL` on Apple targets. The build warns and ignores it on other targets. |
| `vulkan` | off | Builds ggml with `GGML_VULKAN`. Requires the Vulkan SDK (`VULKAN_SDK` is read by `build.rs`). |
| `blas` | off | Builds ggml with `GGML_BLAS`. `GGML_BLAS_VENDOR` and `BLA_VENDOR` select the vendor. |
| `native` | off | Builds ggml with `GGML_NATIVE=ON` (host CPU tuning). Otherwise `GGML_NATIVE=OFF`. |

The default feature set is empty. The build always disables shared libraries, enables static ggml (`GGML_STATIC=ON`), and turns off examples, tests, and ccache.

## Build inputs

- `GGML_SRC` overrides the ggml source directory. The default is `vendor/ggml`.
- `CMAKE_GENERATOR`: on Windows, Ninja is used when it is on the path and no generator is set.
- Bindings: `build.rs` copies the checked-in `bindings-<target>.rs` for the build target. `bindgen` is an optional build dependency behind the `regenerate-bindings` feature, which rewrites that file from `vendor/ggml/include` (see [VERSIONS.md](VERSIONS.md)). A target with no checked-in file fails the build with that instruction.

## Platform support

Metal is effective on Apple targets only. Vulkan and BLAS need their SDKs or libraries installed on the build host. The Windows bindings are checked in, so Windows builds do not need `bindgen` or libclang.

Published version: see `INDEX.md` at the repository root. Release history is in [CHANGELOG.md](CHANGELOG.md). For the pinned upstream commit and licences, see [PINNED.md](PINNED.md) and [VERSIONS.md](VERSIONS.md).

## Example

Not an app-facing API. The crate has a bindings-stamp test (`tests/bindings_match.rs`) and two unit smoke tests in `src/lib.rs`, and no examples. Use it through `rightkit-qts-ggml`.
