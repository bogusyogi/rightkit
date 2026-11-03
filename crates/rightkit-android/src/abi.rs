//! The Android ABIs this bridge builds and the pinned SDK levels.
//!
//! The values here are mirrored in `android/build.gradle.kts` and `scripts/toolchain.versions`;
//! the tests in this module fail when the three drift apart.

/// Lowest Android API level the module supports. The plan (heardright 2026-10-16-android.md) names
/// no minSdk; 28 (Android 9) follows its NNAPI note ("needs 8.1+, recommends 9+"). Confirm.
pub const MIN_SDK: u32 = 28;
/// `compileSdk` of the Gradle module.
pub const COMPILE_SDK: u32 = 35;
/// Side-by-side NDK revision (r27c, LTS).
pub const NDK_VERSION: &str = "27.2.12479018";

/// An Android ABI shipped in the AAR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Abi {
    Arm64V8a,
    X86_64,
}

/// Every supported ABI, device first, emulator second.
pub const SUPPORTED_ABIS: [Abi; 2] = [Abi::Arm64V8a, Abi::X86_64];

impl Abi {
    /// The name Gradle `abiFilters` and the `jniLibs/<abi>` directory use.
    pub fn android_name(self) -> &'static str {
        match self {
            Abi::Arm64V8a => "arm64-v8a",
            Abi::X86_64 => "x86_64",
        }
    }

    /// The Rust target triple (`rustup target add`).
    pub fn rust_triple(self) -> &'static str {
        match self {
            Abi::Arm64V8a => "aarch64-linux-android",
            Abi::X86_64 => "x86_64-linux-android",
        }
    }

    pub fn from_android_name(name: &str) -> Option<Abi> {
        SUPPORTED_ABIS
            .into_iter()
            .find(|a| a.android_name() == name)
    }

    pub fn from_rust_triple(triple: &str) -> Option<Abi> {
        SUPPORTED_ABIS
            .into_iter()
            .find(|a| a.rust_triple() == triple)
    }

    /// The ABI this build targets, or `None` off Android and for unsupported architectures.
    pub fn current() -> Option<Abi> {
        if !cfg!(target_os = "android") {
            return None;
        }
        if cfg!(target_arch = "aarch64") {
            Some(Abi::Arm64V8a)
        } else if cfg!(target_arch = "x86_64") {
            Some(Abi::X86_64)
        } else {
            None
        }
    }
}
