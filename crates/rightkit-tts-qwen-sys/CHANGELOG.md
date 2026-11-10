# Changelog

## 0.1.5 (unpublished)

- Windows `vulkan` build: the shader generator's CMake sub-build moves to `OUT_DIR/v`, so its deepest path no longer passes the 260 character limit under a deep `OUT_DIR` (265 characters before, about 244 after, for an `OUT_DIR` of 146 characters). Done through a new cache variable `GGML_VULKAN_SHADERS_GEN_DIR` in the vendored ggml CMake files, set only by `build.rs` on Windows with `vulkan`; every other build keeps the upstream layout.

## 0.1.4

- Declares `supported_targets` (`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`) in `[package.metadata.rightkit]`, and an unsupported target now stops with a message that lists the supported targets instead of only naming the missing bindings file (rightkit#1346 item 8). Supported builds are unchanged.

## 0.1.3 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.2 - 2026-10-10

Renamed from rightkit-qts-ggml-sys; 0.1.2 remains published under the old name.

- Set `repository` to `https://github.com/bogusyogi/rightkit` and add `rust-version = "1.98.0"`, matching the other RightKit crates. `license` stays `Apache-2.0 AND MIT` (vendored upstream and ggml); LICENSE and NOTICE are unchanged. No code, binding, or API change. Earlier history: `VERSIONS.md` and `PINNED.md`.
