# Changelog

## Unreleased

## 0.1.0

- New crate (rightkit#149, rightkit#148). UniFFI 0.29 proc-macro surface (`bridge_version`, `is_initialised`, `app_dirs`, `supported_abis`, `min_sdk`, `log_debug`; records `AppDirs`, enum `LogLevel`, error `AndroidError`), JNI init (`RightKitNative.nativeInit`: captures the `JavaVM`, application `Context` and app paths once), the `ContextHandle` / `ContextHandleExt` / `StaticContext` handle for crates above this one, the JNI `KeystoreBridge` installed into `rightkit-credentials` at init, a debug-only logcat sink, an optional `platform-verifier` feature, the Gradle library module under `android/` (Kotlin `RightKit.init`, `RightKitKeystore`, packaging of `jniLibs/<abi>` and generated bindings, ABIs `arm64-v8a` and `x86_64`, minSdk 28), and the toolchain bootstrap and build scripts with a pins file.
- Unverified: nothing here has been compiled, tested, run on a device or emulator, or built with Gradle. The pure parts (paths, ABI table, pin file drift, logcat formatting, error and code mapping) have host unit tests that have not been run yet.
