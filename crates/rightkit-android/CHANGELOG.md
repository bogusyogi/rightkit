# Changelog

## 0.3.0 — unpublished staging

- BREAKING (D23): public signatures take `rightkit-credentials` 0.3 types (`Store`, `Secret` or the Android keystore bridge), which changes their type identity from 0.2. Requires rightkit-credentials 0.3.0.

## 0.2.0 (2026-10-17)

- Added (rightkit#156, bounded piece): `crash` module. `install_panic_recorder(dir)` chains a panic hook that writes one report per panic (bridge version, time, thread, source location; never the panic message), keeps the newest 8, and `pending_reports`, `read_report` (name-validated, size-bounded) and `clear_reports` serve the next launch. `AppPaths::crash_dir()` is `<files_dir>/crashes`. Still deferred: native signal crash capture and the Java `UncaughtExceptionHandler`, and AAudio capture (needs `libaaudio` linking and a device pass). Not compiled or run here.

## 0.2.0

- `jni` 0.21.1 to 0.22.4, together with `rustls-platform-verifier` 0.7.x (its `platform-verifier` feature; 0.7 depends on `jni` 0.22, so the `Env` and `JObject` handed to `init_with_env` are the verifier's own types). Exported JNI symbol names and signatures are unchanged: `Java_com_orthiclabs_rightkit_android_RightKitNative_nativeInit` takes `(EnvUnowned, JClass, JObject, JString, JString, JString, jboolean)`, the same native signature as before, and the Kotlin side and the `RightKitKeystore` contract did not change.
- Breaking (Rust API): `ContextHandle::with_env_dyn` and `ContextHandleExt::with_env` take `&mut jni::Env<'_>` instead of `&mut JNIEnv<'_>`; `AndroidContext::application_context` returns `&Global<JObject<'static>>` instead of `&GlobalRef`; `jni_entry::initialise` and `init_platform_verifier` take `&mut jni::Env<'_>`. `jni_error_code` also maps `Error::CaughtJavaException` to `JNI_EXCEPTION`.
- Behaviour kept: `with_env` attaches through `JavaVM::attach_current_thread` (permanent attach, one local frame per call), clears a pending exception when the closure fails and reports it as `AndroidError::Jni`. The Keystore bridge still reads only `RightKitBridgeException.getCode()` and never the message.
- Still unverified: not compiled or run here; needs the first CI build and an emulator pass.

## 0.1.0

- New crate (rightkit#149, rightkit#148). UniFFI 0.29 proc-macro surface (`bridge_version`, `is_initialised`, `app_dirs`, `supported_abis`, `min_sdk`, `log_debug`; records `AppDirs`, enum `LogLevel`, error `AndroidError`), JNI init (`RightKitNative.nativeInit`: captures the `JavaVM`, application `Context` and app paths once), the `ContextHandle` / `ContextHandleExt` / `StaticContext` handle for crates above this one, the JNI `KeystoreBridge` installed into `rightkit-credentials` at init, a debug-only logcat sink, an optional `platform-verifier` feature, the Gradle library module under `android/` (Kotlin `RightKit.init`, `RightKitKeystore`, packaging of `jniLibs/<abi>` and generated bindings, ABIs `arm64-v8a` and `x86_64`, minSdk 28), and the toolchain bootstrap and build scripts with a pins file.
- Unverified: nothing here has been compiled, tested, run on a device or emulator, or built with Gradle. The pure parts (paths, ABI table, pin file drift, logcat formatting, error and code mapping) have host unit tests that have not been run yet.
