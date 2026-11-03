# Building MeshLink

MeshLink is a SwiftPM package over a static Rust library (`crates/rightkit-mesh-ffi`, which wraps `rightkit-mesh`).
The package needs `RightKitMeshFFI.xcframework` in this directory before SwiftPM can resolve it. The
framework is a build product and is git-ignored. Run from the repository root, on the Mac, once per change
to `rightkit-mesh` or `rightkit-mesh-ffi`. Output goes under `/Volumes/D/scratch` (disk hygiene).

`rightkit-mesh-ffi` is its own cargo workspace (excluded from the root workspace), so cargo commands run from
`rightkit-mesh-ffi/` and use its own `Cargo.lock`. It links our own `ios-netwatch` crate in place of upstream `netwatch`
(`rightkit-mesh-ffi/ios-netwatch/README.md`); desktop builds never see it. Both iOS targets built (dev profile) on
2026-10-18; the release build and xcframework steps below are not run yet (#197).

```sh
# RustRight sets the target directory (under /Volumes/D/scratch); take the library paths from its build output.
export CARGO_TARGET_DIR=/Volumes/D/scratch/rightkit/mesh-ffi-target   # only if the shim accepts it; else use its directory
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cd crates/rightkit-mesh-ffi

# Dev builds use the repository's RustRight policy (docs/rules/rightkit.md); the shipped library is a release build.
rustright cargo build --release --target aarch64-apple-ios
rustright cargo build --release --target aarch64-apple-ios-sim
cd ..

rm -rf swift/RightKitMeshLink/RightKitMeshFFI.xcframework
xcodebuild -create-xcframework \
  -library "$CARGO_TARGET_DIR/aarch64-apple-ios/release/librightkit_mesh_ffi.a" -headers crates/rightkit-mesh-ffi/include \
  -library "$CARGO_TARGET_DIR/aarch64-apple-ios-sim/release/librightkit_mesh_ffi.a" -headers crates/rightkit-mesh-ffi/include \
  -output swift/RightKitMeshLink/RightKitMeshFFI.xcframework
```

`crates/rightkit-mesh-ffi/include` holds `rightkit_mesh_ffi.h` and `module.modulemap` (module `RightKitMeshFFI`).
There is no Intel simulator slice; add `x86_64-apple-ios` and `lipo` the two simulator libraries if one is needed.

Checks: end-to-end only (a phone paired with a computer on a device run, #786); this package ships no unit tests.

An app takes the package by path (package `RightKitMeshLink`, product `MeshLink`).
