# rightkit-android

The Android bridge for Right Suite apps: one Rust crate plus one Gradle library module. It exists because no other crate may link JNI or UniFFI: apps reach the `JavaVM`, the application `Context`, the Keystore and the generated Kotlin API through here.

**Use it** in any Right Suite Android app (HeardRight first). **Do not use it** for app-specific exports (model loading, PCM sessions): those live in the app's own cdylib, which links this crate.

Status: written without compiling or running anything (design: heardright `docs/plans/2026-10-16-android.md`, section 4). See "Unverified".

## What it provides

| Piece | Where | Purpose |
| --- | --- | --- |
| UniFFI surface | `src/lib.rs`, `uniffi.toml` | `bridge_version`, `is_initialised`, `app_dirs`, `supported_abis`, `min_sdk`, `log_debug`; `AppDirs`, `LogLevel`, `AndroidError`. Kotlin package `com.orthiclabs.rightkit.android.core`. |
| JNI init | `src/jni_entry.rs`, `RightKitNative.kt`, `RightKit.kt` | `RightKit.init(context)` captures the `JavaVM`, application `Context` and `filesDir`/`cacheDir`/`nativeLibraryDir` once. Idempotent for the same paths; different paths are refused. No `JNI_OnLoad`. |
| Context handle | `src/context.rs` | `ContextHandle` (object-safe), `ContextHandleExt::with_env`, `handle()`, `StaticContext` (host test double). |
| Keystore bridge | `src/keystore.rs`, `RightKitKeystore.kt` | Installs the `rightkit-credentials` Android store (`KeystoreBridge` over JNI; AES-256-GCM key stays in `AndroidKeyStore`). |
| Logcat sink | `src/logcat.rs` | `log_debug`; off unless `RightKit.init(debugLogging = true)`. Debug only; JSONL (`rightkit-logs`) stays the record. |
| Gradle module | `android/` | Packages `jniLibs/<abi>/*.so` and the generated Kotlin; ABIs `arm64-v8a` and `x86_64`; minSdk 28 (a choice, the plan names none). |
| Scripts | `scripts/` | Toolchain bootstrap and build, both PowerShell and sh, one pins file. See `TOOLCHAIN.md`. |

## Using it from an app

Kotlin, in `Application.onCreate` of every process (app, IME or service process each load their own copy):

```kotlin
RightKit.init(this, debugLogging = BuildConfig.DEBUG)
// with an app cdylib that links rightkit-android:
RightKit.init(this, debugLogging = BuildConfig.DEBUG, libraryName = "heardright_android")
```

Settings: `include(":rightkit-android")` and point `projectDir` at `crates/rightkit-android/android`; add `implementation(project(":rightkit-android"))`. Run `scripts/build-android.*` first (the module refuses to build without the `.so` files; `-Prightkit.skipNativeCheck=true` skips that).

The app needs `android:extractNativeLibs="true"` (or `packaging { jniLibs { useLegacyPackaging = true } }`) when Rust loads a sibling `.so` by path, as `rightkit-onnx` does with `libonnxruntime.so` from `nativeLibraryDir`. Exclude `files/rightkit-credentials/` from Auto Backup and device transfer (`android:fullBackupContent`, `android:dataExtractionRules`): a restored store file cannot be decrypted without its Keystore key.

Rust, in the app's cdylib:

```rust
// Keep the JNI symbol and the bridge's UniFFI component in the app's one .so.
pub use rightkit_android::jni_entry::*;
uniffi::setup_scaffolding!();               // the app's own component (library mode)

use rightkit_android::{handle, ContextHandleExt};
let ctx = handle()?;                         // Err(NotInitialised) before RightKit.init
let files = ctx.paths().files_dir.clone();
ctx.with_env(|env, app_context| { /* JNI calls on an attached thread */ Ok(()) })?;
```

## How the other crates take the context

The bridge sits above them; they never depend on it.

* **rightkit-credentials**: `init` calls `rightkit_credentials::android::install(Arc::new(JniKeystoreBridge), files_dir)`. After `RightKit.init`, `system_backend()` works on Android. Before it, every call is `Error::Unavailable`.
* **rightkit-http**: root certificates come from `rustls-platform-verifier`, which needs the `JavaVM` and `Context`. Enable this crate's `platform-verifier` feature and `init` initialises it (unverified, see below), or call `rustls_platform_verifier::android::init_with_env` yourself from `ContextHandleExt::with_env`. The verifier's Kotlin component must be on the app classpath (its Maven artifact, per its README).
* **rightkit-models, rightkit-logs, rightkit-app-settings**: plain strings from `app_dirs()` (`models_dir`, `logs_dir`, `settings_dir` under `files_dir`).
* **rightkit-onnx**: `native_library_dir` from `app_dirs()` is where `libonnxruntime.so` is loaded from.
* Crates above (an app's cdylib): `handle()` and `ContextHandleExt::with_env`.

## Features

| Feature | Default | Effect |
| --- | --- | --- |
| `bindgen` | off | Builds the `uniffi-bindgen` binary for `scripts/build-android.*`. Never in a shipped build. |
| `platform-verifier` | off | Adds `rustls-platform-verifier` and initialises it during `init`. |

## Tests

Host-runnable (no JVM): path validation, ABI and triple table, pin-file parsing and drift against `build.gradle.kts` and `TOOLCHAIN.md`, logcat formatting, JNI error and exception-code mapping, "before init" behaviour of the exported functions and the Keystore bridge, the Kotlin/Rust class and code-name agreement. Not run yet.

## Unverified

* Nothing here has been compiled, linted, packaged or run. UniFFI 0.29 attribute and `uniffi.toml` key spellings, `jni` 0.21.1 call signatures, the Gradle and AGP/Kotlin pins and the `platform-verifier` call against the version `rightkit-http` pins are from memory.
* `#[no_mangle]` symbols from an rlib reaching an app cdylib (the `pub use rightkit_android::jni_entry::*` re-export), and several UniFFI components in one `.so`.
* The Keystore path (key generation, IV handling, StrongBox, invalidation, multi-process locking) on a device or emulator; thread attach and detach from `rightkit-credentials` worker threads.
* The secret passes through Java byte arrays that Rust cannot zero.
* minSdk 28 and the pinned NDK, cargo-ndk, AGP and Kotlin versions.
