# Changelog

## Unreleased

- Lockfile: `iroh-mdns-address-lookup` 0.6.0 -> 0.6.1 with the mesh crate's pin (#1572). No source difference from the Pulse copy remained.

## 0.1.0 (unpublished)

- Handed over from bogusyogi/pulse `main` at 9369a0108 (`rightkit-mesh-ffi/`, `rightkit-mesh-ffi/ios-netwatch/`, `android/meshlink/`) into this repository (#786). Package renamed `pulse-mesh-ffi` to `rightkit-mesh-ffi`; the dependency on the mesh is now `rightkit-mesh` (path), `rightkit-fs` and `rightkit-crypto` are path dependencies. No source change in `src/`, `include/` or `ios-netwatch/src/`. The Pulse `Cargo.lock` was not carried over; the gate generates one.
- The unit tests of the Pulse tree and the Swift golden-transcript test (`MeshLinkTests`) were not carried over (no unit tests; end-to-end only).
- Unchanged from Pulse and still unproven: end-to-end pairing between a phone and a computer, the Android build, the FFI release build (the xcframework is built from the dev profile libraries).
- The Android `meshlink` Kotlin module is not part of this release: it has no `gradle.lockfile` and one cannot be written without running Gradle on a host with the Android SDK (rightkit#786, class A `gradle-lock`).
