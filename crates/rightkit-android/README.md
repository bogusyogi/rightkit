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
| Panic capture | `src/crash.rs` | `install_panic_recorder(dir)` writes a bounded report per Rust panic (location and thread, never the message) to `AppPaths::crash_dir()`; `pending_reports`, `read_report`, `clear_reports` for the next launch. Native signal crashes and AAudio capture are deferred (#156). |
| Gradle module | `android/` | Packages `jniLibs/<abi>/*.so` and the generated Kotlin; ABIs `arm64-v8a` and `x86_64`; minSdk 28 (a choice, the plan names none). |
| Scripts | `scripts/` | Toolchain bootstrap and build, both PowerShell and sh, one pins file. See `TOOLCHAIN.md`. |

## Using it from an app

Kotlin, in `Application.onCreate` of every process (the app process and any service process each load their own copy):

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

* Nothing here has been compiled, linted, packaged or run. UniFFI 0.29 attribute and `uniffi.toml` key spellings, `jni` 0.22.4 call signatures (read from the crate source, not compiled), the Gradle and AGP/Kotlin pins and the `platform-verifier` call against `rustls-platform-verifier` 0.7 are from memory.
* `#[no_mangle]` symbols from an rlib reaching an app cdylib (the `pub use rightkit_android::jni_entry::*` re-export), and several UniFFI components in one `.so`.
* The Keystore path (key generation, IV handling, StrongBox, invalidation, multi-process locking) on a device or emulator; thread attach and detach from `rightkit-credentials` worker threads.
* The secret passes through Java byte arrays that Rust cannot zero.
* minSdk 28 and the pinned NDK, cargo-ndk, AGP and Kotlin versions.

## Optional model installer (0.3.2 candidate)

Enable Cargo `models` and Gradle `-Prightkit.models=true` together. Without both,
existing consumers keep their current surface. Models pulls `rightkit-models =0.7.2`,
URL parsing and `platform-verifier`. Initialise RightKit before calling the installer.
The Gradle module supplies the verifier's matching Android component and JNI keep rule.
An app that includes the module adds the verifier's Maven repository from
`android/settings.gradle.kts` to its own settings. Keep the repository filter so it
resolves only the verifier. Coroutines 1.6.4 supplies StateFlow.

Gradle uses `gradle.lockfile` with models enabled and `gradle-core.lockfile` otherwise.
Refresh each mode with `gradle dependencies --write-locks` and the corresponding
`rightkit.models` setting when updating dependencies.

The published crates.io package includes `android/**` and `scripts/**`. Include its
`android/` directory as a Gradle project using a source location resolved from the
app's pinned crate metadata. There is no Maven coordinate or published AAR route.
Run `scripts/build-android.sh --models` or `scripts/build-android.ps1 -Models`
through the authorised gate. Scripts use the packaged crate's manifest by default;
for an app cdylib pass `--manifest-path` / `-ManifestPath`, `--crate` / `-Crate`
and `--library` / `-Library`. The app must enable `rightkit-android/models` in Cargo.
Use `-Prightkit.libraryName=<library>` for app cdylibs. Binding generation reads the
actual arm64 library with a matching Kotlin package and native library name;
both arm64-v8a and x86_64 native libraries must ship with those bindings.
UniFFI 0.32 uses a per-crate global config override, with Android mode enabled.
These scripts generate the RightKit bridge component only; generate app-specific
UniFFI components separately with their own package configuration.

`ModelInstallRequest` carries host-owned app ID, model ID, payload directory,
HTTPS Worker origin, published route, grant model, Android platform, device ID,
user agent, archive SHA-256 and size. Revision must equal the lowercase archive
SHA-256. No Android model endpoint, archive digest or byte size is provided here.
`ModelDownloadManager` exposes StateFlow, startup recovery, start, cancel and close.
Call close before cancelling its coroutine scope; it retains native handles until
blocking work returns. Readiness verifies installed files fully before reuse.
Payload lives at `<filesDir>/models/<model_id>/<payload_dir>/`; TarGz strips one
wrapper directory. The store reserves `.dl`, `.shared` and `manifest.json`.
The installer returns an error when the verified revision lacks the requested payload
directory. The app supplies its screen and catalogue. Pass `failureLabel` for app wording.

Grant and download use the same canonical HTTPS origin. The bearer is host-scoped,
download redirects are disabled, archive hash and size are required, extraction
and atomic replacement belong to rightkit-models. Cancellation is cooperative:
0.7.2 checks during download but cannot interrupt extraction or final activation;
a late cancel can complete successfully. Startup recovery restores an interrupted
replacement according to the Store contract. Failed recovery remains visible.

Qualification: generate both ABIs and real UniFFI 0.32 Kotlin, compile Gradle with
models disabled and enabled, then exercise installed app init, successful pinned
delivery, wrong pins, 429, redirect refusal, cancelled download, late cancellation,
failed replacement and process-death recovery. This candidate has source evidence
only. Live delivery additionally needs a published Android archive/manifest and
app construction, initialisation, recovery and install calls wired to this module.
