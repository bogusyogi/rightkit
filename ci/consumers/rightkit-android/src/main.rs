// rightkit-android 0.3.0 consumed by exact registry pin, outside the workspace.
//
// Proves the pure surface an app's own cdylib reads before RightKit.init runs (rightkit-android
// abi.rs, paths.rs and the exported helpers in lib.rs): the ABI table with its Gradle names and
// Rust triples, MIN_SDK and COMPILE_SDK, AppPaths validation with the derived AppDirs
// subdirectories and a refused relative path, and is_initialised, app_dirs, supported_abis,
// min_sdk and bridge_version before any init.
// Does not prove: the JNI entry point, RightKit.init, the Keystore bridge, the platform verifier,
// panic capture on a device, logcat output, or any Android runtime behaviour. Those need a device
// or emulator and are unverified.
// State: pure string and table checks. No JNI, no network, no files, no real user data.
use rightkit_android::{Abi, AndroidError, AppPaths, COMPILE_SDK, MIN_SDK, SUPPORTED_ABIS};

const FILES: &str = "/data/user/0/com.example.consumer/files";
const CACHE: &str = "/data/user/0/com.example.consumer/cache";
const NATIVE: &str = "/data/app/~~consumer/com.example.consumer/lib/arm64";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ABI table: Gradle names, Rust triples and their round trip.
    assert_eq!(SUPPORTED_ABIS, [Abi::Arm64V8a, Abi::X86_64]);
    for abi in SUPPORTED_ABIS {
        assert_eq!(Abi::from_android_name(abi.android_name()), Some(abi));
        assert_eq!(Abi::from_rust_triple(abi.rust_triple()), Some(abi));
    }
    assert_eq!(Abi::Arm64V8a.android_name(), "arm64-v8a");
    assert_eq!(Abi::Arm64V8a.rust_triple(), "aarch64-linux-android");
    assert_eq!(Abi::from_android_name("mips"), None);
    assert_eq!(MIN_SDK, 28);
    assert!(COMPILE_SDK >= MIN_SDK);

    // AppPaths: the three directories Kotlin injects, normalised, with the derived subdirectories.
    let paths = AppPaths::new(FILES, CACHE, NATIVE)?;
    let dirs = paths.dirs();
    assert_eq!(dirs.files_dir, FILES);
    assert_eq!(dirs.native_library_dir, NATIVE);
    assert_eq!(dirs.models_dir, format!("{FILES}/models"));
    assert_eq!(dirs.settings_dir, format!("{FILES}/settings"));
    assert_eq!(dirs.logs_dir, format!("{FILES}/logs"));
    assert_eq!(paths.crash_dir(), format!("{FILES}/crashes"));
    assert_eq!(
        AppPaths::new(&format!("{FILES}/"), CACHE, NATIVE)?.files_dir,
        FILES
    );

    // Error path: a relative path and a parent component are refused, and the message does not
    // echo the path.
    let refused = AppPaths::new("relative/private-name", CACHE, NATIVE).unwrap_err();
    assert!(matches!(refused, AndroidError::Invalid { .. }));
    assert!(!refused.to_string().contains("private-name"));
    assert!(matches!(
        AppPaths::new("/a/../b", CACHE, NATIVE),
        Err(AndroidError::Invalid { .. })
    ));

    // Exported helpers before RightKit.init: no context is captured, so app_dirs fails closed.
    assert!(!rightkit_android::is_initialised());
    assert_eq!(
        rightkit_android::app_dirs(),
        Err(AndroidError::NotInitialised)
    );
    assert_eq!(rightkit_android::min_sdk(), MIN_SDK);
    assert_eq!(
        rightkit_android::supported_abis(),
        vec!["arm64-v8a".to_string(), "x86_64".to_string()]
    );
    assert_eq!(rightkit_android::bridge_version(), "0.3.0");

    println!("OK rightkit-android 0.3.0 (registry consumer)");
    Ok(())
}
