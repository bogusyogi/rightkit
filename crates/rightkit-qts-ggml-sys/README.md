# rightkit-qts-ggml-sys

Low-level FFI bindings to ggml for `rightkit-qts`. It is vendored from QTS v0.2.0 (see [PINNED.md](PINNED.md)); ggml is vendored as plain files at `vendor/ggml`, not as a git submodule. Its `CMakeLists.txt` reports version 0.9.8. [VERSIONS.md](VERSIONS.md) still describes a submodule and an older path; treat its build-matrix section as the reference, and its submodule steps as stale. The crate builds ggml as a static library with CMake (through the `cmake` build dependency) and generates or copies bindings. The Cargo `links` key is `rightkit_ggml`.

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
- Bindings: `build.rs` copies a checked-in `bindings-<target>.rs` when one exists for the target (currently `bindings-x86_64-pc-windows-msvc.rs`); otherwise it runs `bindgen` over `wrapper.h`. `bindgen` is a build dependency.

## Platform support

Metal is effective on Apple targets only. Vulkan and BLAS need their SDKs or libraries installed on the build host. The Windows bindings are checked in, so Windows builds do not need `bindgen` for that target.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`. For the pinned upstream commit and licences, see [PINNED.md](PINNED.md) and [VERSIONS.md](VERSIONS.md).

## Example

Not an app-facing API. The crate has no tests or examples of its own. Use it through `rightkit-qts-ggml`.
