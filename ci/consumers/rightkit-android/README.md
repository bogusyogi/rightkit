# rightkit-android 0.3.0 registry consumer (draft, PK09)

Standalone consumer, outside the workspace, pinning `rightkit-android =0.3.0` exactly. It exercises
the pure surface an app's own cdylib reads before `RightKit.init`: the ABI table (`Abi`,
`SUPPORTED_ABIS`, `MIN_SDK`, `COMPILE_SDK`), `AppPaths::new` with `dirs()` and `crash_dir()`, the
refused relative path, and the exported helpers `is_initialised`, `app_dirs`, `supported_abis`,
`min_sdk` and `bridge_version`.

State: pure string and table checks. No JNI, no network, no files, no real user data. The paths
are fixture strings that never touch the filesystem. Run only by the CI consumer lane.

Not covered: the JNI entry point and `RightKit.init`, the Keystore bridge, the platform verifier,
panic capture on a device, logcat output, the Gradle module and the generated Kotlin bindings. Those
need a device or emulator and are unverified. PK09 must be revisited if the bridge API changes.
