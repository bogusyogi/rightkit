# rightkit-mesh-ffi

The C ABI (`rkmesh_*`, `include/rightkit_mesh_ffi.h`) over `rightkit-mesh` for a phone: the joiner
side of short-code pairing (QR, then an 8-digit code, then owner approval), sessions, and one
request per stream. Handed over from Pulse (bogusyogi/rightkit#786); not published (`publish = false`).

- `ios-netwatch/`: RightKit's own implementation of the slice of `netwatch` 0.19.3 that iroh,
  iroh-relay and portmapper call, because upstream does not build for iOS. Wired by
  `[patch.crates-io]` in this crate's workspace only; desktop builds keep upstream.
- `../../swift/RightKitMeshLink`: the SwiftPM package over the static library (iOS 18).
- `android/meshlink/`: not in this repository yet. The Kotlin module over the same ABI (JNA) needs a committed `gradle.lockfile` (Gradle dependency locking, class A `gradle-lock`), which only `./gradlew dependencies --write-locks` on a host with the Android SDK can produce; it is kept at bogusyogi/pulse `main` 9369a0108 until then (rightkit#786).

This crate is its own cargo workspace; build from this directory. Phone builds turn
`rightkit-mesh`'s `fast-apple-datapath` feature off (private syscalls fail App Store review).

Wire constants that still carry the product name, on purpose: `pulse/link/1` and the
`pulse-link/*` domain strings (byte for byte with the desktop pairing). Renaming any of them is a
versioned protocol change on both ends.
