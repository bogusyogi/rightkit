# Minimum RightKit for Android

Issue: bogusyogi/rightkit#117. First consumer: HeardRight (plan: heardright `docs/plans/2026-10-16-android.md`, heardright#13). Date: 2026-10-17.

Status: design. Everything below is read from source. Nothing has been compiled for an Android target and nothing has run on an emulator or device; "code-ready" means "the source has the Android path and no known blocker", not "built".

## Decisions in force (Adrian, 2026-10-16)

- Native Kotlin shell (Jetpack Compose) with one Rust cdylib. No Tauri mobile.
- No IME / keyboard. Dictation is a launcher shortcut plus in-app recording, as on iOS.
- Own-cloud sync: CloudKit on Apple, Google Drive `appDataFolder` everywhere else, no service of ours. The `http` backend, `rightkit-sync-keys` and the sync worker are parked (#118).

## 1. Target triples and toolchain

| Item | Value | Source |
| --- | --- | --- |
| Rust targets | `aarch64-linux-android` (device), `x86_64-linux-android` (emulator) | `crates/rightkit-android/scripts/toolchain.versions` |
| ABIs | `arm64-v8a`, `x86_64` | same, `rightkit-android/android/build.gradle.kts` |
| NDK | `27.2.12479018` (r27c), side by side | `TOOLCHAIN.md` (pin unverified) |
| minSdk / compileSdk | 28 / 35 (minSdk is a choice, not in the plan; HeardRight to confirm) | `TOOLCHAIN.md` |
| Build driver | `cargo-ndk 3.5.4`, run by `scripts/build-android.(ps1\|sh)` (`--platform 28`, dev profile only) | `TOOLCHAIN.md` |
| JDK / AGP / Kotlin | JBR 17+ from Android Studio / 8.7.3 / 2.0.21 | `TOOLCHAIN.md` |
| Bootstrap | `scripts/bootstrap-android.(ps1\|sh)`: check by default, `-Install` adds targets, cargo-ndk, sdkmanager packages; never accepts licences | `TOOLCHAIN.md` |

Who provides the NDK. The Android Studio SDK Manager supplies the SDK and NDK once per host (a person installs Android Studio and accepts licences). `rightkit-android`'s bootstrap script verifies and, where safe, installs the pinned pieces. The broker side belongs to RustRight: the managed `rightkit cargo` must accept `--target aarch64-linux-android` and find the NDK linker; that is tracked at https://github.com/Orthic-Labs/rustright/issues/8 and device runs wait on it. Wiring `tools/setup-workspace.py` to the bootstrap pins file is a follow-up for that script's owner.

Build hosts: one dev build per host, owner chat only. Dell (lead: win) is preferred for CI-style compile checks. Android compile lanes in CI (`cargo ndk ... check`) are the first proof for every row below; no crate is "built for Android" until that lane is green.

## 2. Ordered crate list and Android status

Order is dependency order for the first runnable app (record, transcribe, show text). Status key: **code-ready** (source has the Android path, needs the CI compile lane and a device check), **needs work** (named gap), **not needed** (no Android use now).

| # | Crate (version) | Status | Evidence in source | Gap |
| --- | --- | --- | --- | --- |
| 1 | rightkit-android 0.1.0 | code-ready, unverified | UniFFI surface, JNI init, Keystore bridge, logcat sink, Gradle module; README "Unverified" lists every unproven call | CI compile lane; `platform-verifier` call against the pinned `rustls-platform-verifier`; device run |
| 2 | rightkit-fs 0.4.2 | code-ready | `app_dirs` has an Android branch; `atomic.rs` deliberately skips raw `renameat2` on Android; `libc` is unix-only | Compile lane. Auto Backup exclusion is an app rule (see `rightkit-android` README) |
| 3 | rightkit-logs 0.3.1 | needs work | Android RAM read from `/proc/meminfo` in `system/identity.rs` | No Android log directory convention or logcat sink wired to the JSONL record; `rightkit-android` exposes `logs_dir`, logs must accept it |
| 4 | rightkit-credentials 0.2.1 | code-ready, unverified | `src/android.rs`: AES-256-GCM key in AndroidKeyStore, ciphertext in app-private storage, injected `KeystoreBridge`, per-directory `flock` | Keystore never run on a device; secret passes through Java byte arrays that cannot be zeroed; backup exclusion |
| 5 | rightkit-http 0.3.2 (+ platform verifier) | needs work | Blocking client is rustls plus `ring`, no OpenSSL; `TrustRoots::Platform` goes through `rustls-platform-verifier`; `TrustRoots::Bundled` needs no JNI | Platform verifier needs the one-time JNI init and its Kotlin artifact: `RightKit.init` with `rightkit-android`'s `platform-verifier` feature (unverified against the pinned version). Until then fails closed. Proxy behaviour on Android is documented, not tested |
| 6 | rightkit-models 0.4.1 | code-ready, unverified | `src/android.rs`: `AndroidFacts` -> `Hardware::from_android` (profile `android-<N>gb`), storage root from `filesDir`; pure logic, host-testable | `Hardware::detect` on Android needs the Kotlin facts (`Build.SUPPORTED_ABIS`, `ActivityManager.MemoryInfo`); no measured pool factor for phones |
| 7 | rightkit-onnx 0.2.6 | needs work | `android_runtime_candidate` / `configure_android_runtime(native_lib_dir)`; Android `ort` dependency with `load-dynamic` and `xnnpack` (CPU provider); `probe_providers` checks at run time | No NNAPI (deliberately absent, README says "later, measured step"); the shipped `libonnxruntime.so` (ORT Android AAR) must contain XNNPACK; `extractNativeLibs=true` required; nothing built |
| 8 | rightkit-vad 0.1.2 | code-ready | Silero rides on rightkit-onnx; only the CPU provider is hosted on Android, others refused | Compile lane |
| 9 | rightkit-asr 0.2.3 | needs work | `platform.rs`: `whisper_cpp` refused on Android; CoreML dependencies are `cfg(target_os = "macos")` | Prove the Parakeet ONNX path compiles for `aarch64-linux-android` (CoreML-gated types, C regex/tokenizer features); Whisper on Android is deferred |
| 10 | rightkit-app-settings 0.2.2 | code-ready | Takes an injected root path, no directory lookup; `std::os::unix::fs::MetadataExt` exists on Android | Compile lane |
| 11 | rightkit-license 0.2.2 | code-ready | Pure Rust Ed25519 verification, no cfgs; subject doc names the Google account as the Android entitlement key | Compile lane. Play Billing token minting is a server change outside this repo |
| 12 | rightkit-device 0.1.3 | code-ready (verify) | No cfgs per the plan's count | Compile lane; device id must not use `ANDROID_ID`-style identifiers (privacy) |
| 13 | rightkit-sync 0.2.1 (`drive`) | needs work | `drive` and the new `sync_once` scheduler have no `target_os` cfgs; token comes from the host (Kotlin `AuthorizationClient`); see `crates/rightkit-sync/README.md` | Compile lane with `--no-default-features --features drive`; Android provider wiring in the app cdylib; WorkManager worker (Kotlin); TLS root store as row 5; Drive behaviour never exercised against real Drive (consistency, `appDataFolder` change feed) |
| 14 | rightkit-qa 0.x (`android` feature) | code-ready, unverified | `src/android.rs`: adb-backed install, `am start` with test extras, logcat by pid, control forward, teardown; test packages only, device by serial | Never run against an emulator or device |
| 15 | rightkit-audio 0.2.3 | not needed for v1 | Kotlin `AudioRecord` pushes 16 kHz mono PCM to Rust; `capture` is cpal with CoreAudio/WASAPI extras | Later: AAudio capture backend, route-change and Bluetooth SCO recovery (deferred) |
| 16 | rightkit-crash 0.3.0 | not needed for v1 | No Android code; features `support-bundle`, `window-errors` | Later: tombstone and ANR capture |

Not needed on Android: rightkit-overlay, -hotkeys, -automation, -tauri-shell, -daemon, -sidecar, -ffmpeg, -send, and any IME or keyboard crate.

The sync crates parked by Adrian (`rightkit-sync-keys`, the `http` feature, `@rightkit/sync-worker`) are not part of the minimum and stay as they are.

## 3. Native Kotlin shell boundary

- The app is Kotlin (Compose). It owns UI, permissions, `AudioRecord`, Play Billing, WorkManager, `AuthorizationClient` (Drive token), and the launcher shortcut.
- One Rust `cdylib` per app (`heardright_android`) links `rightkit-android` as an rlib, so one `.so` carries every UniFFI component. App-specific exports (model load, PCM session, transcript, sync wrapper) live in that cdylib, never in `rightkit-android`.
- `RightKit.init(context, debugLogging, libraryName)` in `Application.onCreate` of every process that touches Rust: it captures `JavaVM`, application `Context`, `filesDir`/`cacheDir`/`nativeLibraryDir`, installs the Keystore bridge into `rightkit-credentials`, and (with the feature) initialises the TLS verifier.
- Other RightKit crates never depend on `rightkit-android`; they take plain paths and injected bridges. Rust code that needs JNI uses `handle()` and `ContextHandleExt::with_env`.
- Crossing the boundary: UniFFI (generated Kotlin, package `com.orthiclabs.rightkit.android.core`) for the typed API; a bare JNI symbol only for `nativeInit`. Blocking Rust calls (sync, transcription) run on a worker thread, never the main thread.
- Packaging: `android:extractNativeLibs="true"` so Rust can load `libonnxruntime.so` by path from `nativeLibraryDir`; exclude `files/rightkit-credentials/` from Auto Backup and device transfer.
- Today `rightkit-android` exposes only its own bridge (init, paths, Keystore, logcat). Other crates are surfaced through the app cdylib, and sync's Kotlin call contract is documented in the `rightkit-sync` README.

## 4. Signing through right-release (Play upload key)

- Release builds go through `right-release build`, upload through `right-release upload`, kept separate (`docs/agent-rules.md`). Play upload needs an Android App Bundle (`.aab`) signed with the Play **upload key**; Play App Signing holds the app signing key.
- The upload key and its passphrase live in the release machine's secret store, never in the repo, never in a Gradle file, never in chat. Gradle reads them from the environment at release time only.
- HeardRight's `docs/rules/release-signing.md` must be read first (heardright#25); it was not present when this was written, so the exact keystore handling is unverified.
- Not verified: whether `right-release` already has an Android target (`bundleRelease`, Play internal track upload). The `right-release` source is outside this repository. Treat Android support in `right-release` as an open requirement (follow-up 14).
- Debug and QA builds use the debug keystore and a package name whose last segment is `dev`/`qa`/`test`/`debug` (a rightkit-qa requirement), so they can coexist with the production install and cannot be mistaken for it.

## 5. QA on emulator and device through rightkit-qa

- `rightkit-qa` feature `android` drives `adb`: install the test APK, `am start` with test-mode extras, logcat filtered by pid into the run directory, optional forward of the dev-only in-app control server, force-stop and uninstall at teardown. Device chosen by serial only (no "the only device" shortcut), test packages only.
- Emulator (x86_64): UI, permissions, install, lifecycle, sync against a fake Drive. Device (arm64): ASR timing, XNNPACK provider presence, Keystore, audio route. Models and tests must not touch real user data (dev build with run-time test mode, per the Dell dev-build rule); real-store tests are gated.
- Acceptance path for the first app: the 5 s utterance on a physical arm64 device (plan section 5).
- Test requirements that apply on Android: single guarded reruns, no broad process kills, and every QA step bounded (the module already kills an overrunning `adb` call).

## 6. Follow-up issues to file (not filed)

One per crate needing work, plus the gaps that block them. Each acceptance line is checkable from CI or a device run.

1. **rightkit: Android compile lane in CI.**
   Acceptance: a CI job runs `cargo ndk -t arm64-v8a -t x86_64 --platform 28 check` for rightkit-android, -fs, -credentials, -http, -models, -app-settings, -license, -device, -vad, -logs, and `rightkit-sync --no-default-features --features drive`, and is green; failures name the crate.
2. **rightkit-logs: Android log directory and logcat bridge.**
   Acceptance: `rightkit-logs` opens its JSONL log under an injected `logs_dir` on Android with no `HOME`/`XDG` lookup; debug logging can mirror to logcat only when enabled; host tests cover path injection; the Android compile lane is green.
3. **rightkit-http: Android TLS roots proven.**
   Acceptance: `rightkit-android`'s `platform-verifier` init is confirmed against the pinned `rustls-platform-verifier` version; a blocking GET to a public HTTPS host succeeds on an emulator after `RightKit.init`, and fails closed (no fallback) before it; `TrustRoots::Bundled` works without init; README states which to use.
4. **rightkit-onnx: Android runtime proven and NNAPI decision.**
   Acceptance: `configure_android_runtime(nativeLibraryDir)` loads `libonnxruntime.so` from the ORT Android AAR on arm64 and x86_64; `probe_providers` reports XNNPACK present and absent correctly; a Silero VAD session runs on a device; NNAPI stays off with a documented measured-opt-in plan (follows heardright#26).
5. **rightkit-asr: Parakeet ONNX compiles and runs for Android.**
   Acceptance: `rightkit-asr` builds for `aarch64-linux-android` with CoreML and whisper.cpp absent (feature or cfg gated, no unconditional Apple dependency); `EngineSupport` refuses Whisper on Android with a typed error; a Parakeet decode of a fixture clip matches the desktop transcript on a device.
6. **rightkit-models: Android hardware facts and storage proven.**
   Acceptance: the Kotlin facts feed `Hardware::from_android` through `rightkit-android`; a model installs atomically into `filesDir/models` with the newest-working fallback; fit selection is tested for 4, 6 and 8 GB profiles; no pool factor is introduced without a measurement.
7. **rightkit-credentials: Keystore store verified on device.**
   Acceptance: store, read, replace and delete round-trip on an emulator and an arm64 device through `RightKitKeystore`; a deleted Keystore key yields an error, never "absent"; multi-process `flock` behaviour is tested with a service process; the backup-exclusion manifest entries are shipped in the Gradle module's sample.
8. **rightkit-android: device verification of the bridge.**
   Acceptance: on an emulator, `RightKit.init` succeeds once, is idempotent for equal paths and refuses different paths; `app_dirs`, `log_debug`, the Keystore bridge and the verifier init all run; the README "Unverified" list is reduced to what was not run.
9. **rightkit-sync: Drive on Android end to end.**
   Acceptance: the `drive` feature builds for both Android targets; a Kotlin `Worker` calls `sync_once` (via the app cdylib) and maps `WorkOutcome` to `Result.success/retry/failure`, enqueuing the next run from `next_delay_secs`; two devices converge through a real Drive `appDataFolder` (record the findings the fake could not show: list consistency, changes-feed scope, subfolders); a revoked token surfaces as `Unavailable` and backs off.
10. **rightkit-qa: first Android journey on emulator and device.**
    Acceptance: one journey installs a `dev` test APK, launches with test-mode extras, collects pid-filtered logcat into the run directory, and tears down leaving no package behind, on an emulator and on an arm64 device selected by serial; a refused non-test package name is covered by a test.
11. **rightkit-audio (deferred): AAudio capture backend.**
    Acceptance: `capture` builds for Android with no Windows/Apple optional dependencies pulled in; cpal AAudio capture is measured against `AudioRecord` for latency and route-change recovery (headset, Bluetooth SCO); a decision is recorded to keep Kotlin capture or switch.
12. **rightkit-crash (deferred): Android tombstone and ANR capture.**
    Acceptance: a native crash in the cdylib leaves a privacy-safe record the next launch can bundle; an ANR is detected without a thread of ours blocking the main thread; the crate builds for Android with desktop-only features off.
13. **rightkit: toolchain pins confirmed and wired into setup.**
    Acceptance: the first run on the Dell and the Mac confirms or corrects every pin in `toolchain.versions` (NDK, build tools, cargo-ndk, AGP, Kotlin, minSdk) and `TOOLCHAIN.md`; `tools/setup-workspace.py` reads the pins file; the broker accepts the Android targets (rustright#8).
14. **right-release: Android App Bundle build and Play internal-track upload.**
    Acceptance: `right-release build` produces a signed `.aab` with the upload key from the secret store (no key in the repo or logs); `right-release upload` is build-free and publishes to the Play internal track; a debug-signed build is refused for upload; documented in `docs/agent-rules.md` alongside the existing rules.
