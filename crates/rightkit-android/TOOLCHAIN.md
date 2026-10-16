# Android toolchain (rightkit#148)

Status: pins are UNVERIFIED. Nothing here has been installed or run by the author. The first run on the Dell (lead: win) confirms or corrects the pins; change `scripts/toolchain.versions` and this file together (a unit test checks they agree).

## Pins

| Item | Pin | Where it is read |
| --- | --- | --- |
| NDK | `27.2.12479018` (r27c, LTS, side by side) | `scripts/toolchain.versions`, `android/build.gradle.kts` (`ndkVersion`), `src/abi.rs` |
| SDK platform | `android-35` (`compileSdk`) | same |
| Build tools | `35.0.0` | `scripts/toolchain.versions` |
| minSdk | `28` (not in the plan; follows its NNAPI note; confirm) | `android/build.gradle.kts`, `src/abi.rs` |
| ABIs | `arm64-v8a` (device), `x86_64` (emulator) | `abiFilters`, `src/abi.rs` |
| Rust targets | `aarch64-linux-android`, `x86_64-linux-android` | `rustup target add` |
| cargo-ndk | `3.5.4` (`cargo install cargo-ndk --version 3.5.4 --locked`) | `scripts/toolchain.versions` |
| JDK | 17 or newer: Android Studio's bundled JBR | `JAVA_HOME` |
| AGP / Kotlin | `8.7.3` / `2.0.21` (standalone module build only) | `android/settings.gradle.kts` |

## Scripts

`scripts/bootstrap-android.ps1` (Windows) and `scripts/bootstrap-android.sh` (macOS, Linux). Same behaviour, same pins file.

```
# check only, changes nothing, exit 1 when something is missing
pwsh crates/rightkit-android/scripts/bootstrap-android.ps1
sh   crates/rightkit-android/scripts/bootstrap-android.sh

# install what can be installed safely
pwsh crates/rightkit-android/scripts/bootstrap-android.ps1 -Install
sh   crates/rightkit-android/scripts/bootstrap-android.sh --install
```

What `-Install` does: `rustup target add` for the two targets; `cargo install cargo-ndk --version <pin> --locked`; `sdkmanager` for the platform, build-tools, platform-tools and the pinned NDK (only when an existing `sdkmanager` and accepted licences are found).

What it never does: accept SDK licences (a person runs `sdkmanager --licenses`), download or run an unpinned installer, edit environment variables, profiles, the registry or system settings. It prints the variables to set (`ANDROID_HOME`, `ANDROID_NDK_HOME`, `NDK_HOME`, `JAVA_HOME`).

## Prerequisites a person installs once

1. Android Studio (provides the JBR and the SDK Manager UI). Windows SDK default: `%LOCALAPPDATA%\Android\Sdk`; macOS: `~/Library/Android/sdk`; Linux: `~/Android/Sdk`.
2. SDK Manager > SDK Tools > "Android SDK Command-line Tools (latest)".
3. `sdkmanager --licenses` (read and accept).

## Build

`scripts/build-android.(ps1|sh)` runs the check, then `cargo ndk -t arm64-v8a -t x86_64 --platform 28 -o android/src/main/jniLibs build -p rightkit-android --release`, then `uniffi-bindgen generate --library ... --language kotlin` into `android/src/main/uniffi-generated`, and writes `BUILD-RECEIPT.txt` (git revision, pins, SHA-256 of each `.so`). Set `RIGHTKIT_CARGO="rightkit cargo"` to go through the managed wrapper. Builds belong to the owner chat (one dev build per host).

## Setup-workspace

The plan routes toolchain installs through `tools/setup-workspace.py` rather than ad hoc. That script is outside this crate; wiring it to call `bootstrap-android.*` (or to read `toolchain.versions`) is a follow-up for its owner.

## Not covered here

The broker's target triples (the managed `rightkit cargo` accepting `--target aarch64-linux-android`, NDK linker discovery) belong to RustRight; tracked in a separate RustRight issue.

## Unverified

cargo-ndk 3.5.4 as the pin and its Windows behaviour; NDK `source.properties` revision format; `--platform` flag spelling; AGP 8.7.3 with Kotlin 2.0.21 and JDK 17; the sdkmanager argument form `--sdk_root=`.
