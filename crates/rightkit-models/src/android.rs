//! Android hardware facts and storage root.
//!
//! UNVERIFIED: written without an Android toolchain and never run on a device. Everything here is
//! pure logic (no JNI, no `cfg(target_os = "android")`), so it compiles and its tests run on any
//! host; only the call sites that feed it real device values (Kotlin: `Build.SUPPORTED_ABIS`,
//! `ActivityManager.MemoryInfo`, `Context.getFilesDir()`) are Android-only and not built here.
//!
//! - **Hardware**: Android has no `sysctl`/registry/`nvidia-smi`. The app reads the facts through
//!   the Android API and hands them over as [`AndroidFacts`]; [`Hardware::from_android`] turns them
//!   into the same profile the other platforms report (`os = "android"`, profile id
//!   `android-<N>gb`). The usable pool stays the existing no-GPU rule (half of RAM); no
//!   Android-specific factor is invented without a measurement. [`Hardware::detect`] on Android
//!   (no facts injected) reads `/proc/meminfo` and reports no GPU.
//! - **Storage**: Android has no `HOME` or per-user suite folder, and apps are sandboxed from each
//!   other, so there is no shared store. The app injects `Context.getFilesDir()` once with
//!   [`init_android_files_dir`]; the suite root is then that directory, so the models live in
//!   `<filesDir>/models` and the tools in `<filesDir>/tools`. [`ModelStore::at_android_files_dir`]
//!   gives a store without any global state.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{Error, ErrorCode, Result};
use crate::hardware::Hardware;

/// An Android ABI, as `Build.SUPPORTED_ABIS` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidAbi {
    Arm64V8a,
    ArmeabiV7a,
    X86_64,
    X86,
}

impl AndroidAbi {
    /// Parse a `Build.SUPPORTED_ABIS` entry (`arm64-v8a`, `armeabi-v7a`, `x86_64`, `x86`).
    pub fn parse(abi: &str) -> Option<Self> {
        match abi.trim().to_ascii_lowercase().as_str() {
            "arm64-v8a" => Some(Self::Arm64V8a),
            "armeabi-v7a" | "armeabi" => Some(Self::ArmeabiV7a),
            "x86_64" => Some(Self::X86_64),
            "x86" => Some(Self::X86),
            _ => None,
        }
    }

    /// The ABI name as Android spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Arm64V8a => "arm64-v8a",
            Self::ArmeabiV7a => "armeabi-v7a",
            Self::X86_64 => "x86_64",
            Self::X86 => "x86",
        }
    }

    /// The architecture name `std::env::consts::ARCH` and [`crate::Platform`] use.
    pub fn arch(self) -> &'static str {
        match self {
            Self::Arm64V8a => "aarch64",
            Self::ArmeabiV7a => "arm",
            Self::X86_64 => "x86_64",
            Self::X86 => "x86",
        }
    }

    /// Whether the Right Suite plans to ship native libraries for this ABI: `arm64-v8a` (devices)
    /// and `x86_64` (emulator). 32-bit ABIs are not planned.
    pub fn is_suite_target(self) -> bool {
        matches!(self, Self::Arm64V8a | Self::X86_64)
    }
}

/// Device facts the app reads through the Android API and injects. Every field is optional or
/// defaulted: an unknown value stays unknown, never guessed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AndroidFacts {
    /// `ActivityManager.MemoryInfo.totalMem`, in bytes.
    pub total_ram_bytes: Option<u64>,
    /// `Build.SUPPORTED_ABIS`, preferred first.
    pub supported_abis: Vec<String>,
    /// `ActivityManager.isLowRamDevice()`. Recorded in the profile's reasons by callers; it does
    /// not change the fit arithmetic here.
    pub low_ram_device: bool,
    /// `Build.VERSION.SDK_INT`.
    pub sdk_int: Option<u32>,
    /// `Runtime.availableProcessors()`.
    pub cpu_cores: Option<usize>,
}

impl AndroidFacts {
    /// The ABIs in `supported_abis` that this crate recognises, in the device's order.
    pub fn abis(&self) -> Vec<AndroidAbi> {
        self.supported_abis
            .iter()
            .filter_map(|abi| AndroidAbi::parse(abi))
            .collect()
    }

    /// The first ABI the device lists that the suite builds for (see
    /// [`AndroidAbi::is_suite_target`]), else `None`.
    pub fn suite_abi(&self) -> Option<AndroidAbi> {
        self.abis().into_iter().find(|abi| abi.is_suite_target())
    }

    /// The first recognised ABI of any kind (the device's preferred one).
    pub fn primary_abi(&self) -> Option<AndroidAbi> {
        self.abis().into_iter().next()
    }
}

impl Hardware {
    /// The profile of an Android device from injected [`AndroidFacts`]. Source is `injected`.
    /// `arch` is the suite ABI when the device lists one, else its first recognised ABI, else this
    /// build's own architecture. No GPU is reported: accelerated Android backends are not part of
    /// this crate's fit rules yet (CPU inference only).
    pub fn from_android(facts: &AndroidFacts) -> Self {
        let arch = facts
            .suite_abi()
            .or_else(|| facts.primary_abi())
            .map(|abi| abi.arch().to_string())
            .unwrap_or_else(|| std::env::consts::ARCH.to_string());
        let mut hw = Hardware {
            os: "android".into(),
            arch,
            memory_gb: facts
                .total_ram_bytes
                .map(|bytes| round1(bytes as f64 / 1_073_741_824.0)),
            gpus: Vec::new(),
            profile: String::new(),
            source: "injected".into(),
            profile_aliases: Vec::new(),
        };
        hw.profile = hw.profile_id();
        hw
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// `MemTotal` from `/proc/meminfo` text, in bytes.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
pub(crate) fn meminfo_total_bytes(text: &str) -> Option<f64> {
    let kb: f64 = text
        .lines()
        .find(|line| line.starts_with("MemTotal:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb * 1024.0)
}

/// Process-wide injected files directory (see [`init_android_files_dir`]).
pub(crate) struct InjectedRoot(OnceLock<PathBuf>);

impl InjectedRoot {
    pub(crate) const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// Record `files_dir` once. The same directory again is accepted; a different one is refused,
    /// so the root cannot move under a running store.
    pub(crate) fn set(&self, files_dir: &Path) -> Result<()> {
        let dir = validate_files_dir(files_dir)?;
        let kept = self.0.get_or_init(|| dir.clone());
        if *kept == dir {
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::Conflict,
                format!(
                    "the Android files directory is already {}; it cannot change to {}",
                    kept.display(),
                    dir.display()
                ),
            ))
        }
    }

    pub(crate) fn get(&self) -> Option<PathBuf> {
        self.0.get().cloned()
    }
}

static FILES_DIR: InjectedRoot = InjectedRoot::new();

/// A usable `Context.getFilesDir()` value: absolute, no `..` component, not empty.
pub(crate) fn validate_files_dir(files_dir: &Path) -> Result<PathBuf> {
    let bad = |why: &str| {
        Error::new(
            ErrorCode::InvalidSpec,
            format!("Android files directory {:?}: {why}", files_dir),
        )
    };
    if files_dir.as_os_str().is_empty() {
        return Err(bad("empty"));
    }
    if !files_dir.is_absolute() {
        return Err(bad("must be absolute"));
    }
    if files_dir
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(bad("must not contain `..`"));
    }
    Ok(files_dir.to_path_buf())
}

/// Inject the app's `Context.getFilesDir()` once, at startup, before any store is built. From then
/// on [`crate::suite::paths::suite_root`] is this directory (models in `<dir>/models`, tools in
/// `<dir>/tools`, `app_data_base` in `<dir>/app-data`), and no operating-system folder lookup
/// runs. The same path again is accepted; a different one is refused. Relative or `..`-bearing
/// paths are refused. Creates nothing on disk.
pub fn init_android_files_dir(files_dir: &Path) -> Result<()> {
    FILES_DIR.set(files_dir)
}

/// The injected files directory, if [`init_android_files_dir`] ran.
pub fn android_files_dir() -> Option<PathBuf> {
    FILES_DIR.get()
}

/// `<files_dir>/models`, the models root for an app whose files directory is `files_dir`. Pure: no
/// global state, nothing created.
pub fn android_models_root(files_dir: &Path) -> Result<PathBuf> {
    Ok(crate::suite::paths::models_root_at(&validate_files_dir(
        files_dir,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(abis: &[&str], gib: f64) -> AndroidFacts {
        AndroidFacts {
            total_ram_bytes: Some((gib * 1_073_741_824.0) as u64),
            supported_abis: abis.iter().map(|s| s.to_string()).collect(),
            ..AndroidFacts::default()
        }
    }

    #[test]
    fn abi_names_round_trip_and_map_to_arch() {
        for (text, abi, arch) in [
            ("arm64-v8a", AndroidAbi::Arm64V8a, "aarch64"),
            ("armeabi-v7a", AndroidAbi::ArmeabiV7a, "arm"),
            ("x86_64", AndroidAbi::X86_64, "x86_64"),
            ("x86", AndroidAbi::X86, "x86"),
        ] {
            assert_eq!(AndroidAbi::parse(text), Some(abi));
            assert_eq!(AndroidAbi::parse(&text.to_uppercase()), Some(abi));
            assert_eq!(abi.as_str(), text);
            assert_eq!(abi.arch(), arch);
        }
        assert_eq!(AndroidAbi::parse("mips"), None);
        assert!(AndroidAbi::Arm64V8a.is_suite_target() && AndroidAbi::X86_64.is_suite_target());
        assert!(!AndroidAbi::ArmeabiV7a.is_suite_target() && !AndroidAbi::X86.is_suite_target());
    }

    #[test]
    fn a_modern_phone_is_an_aarch64_profile_with_half_its_ram_usable() {
        let hw = Hardware::from_android(&facts(&["arm64-v8a", "armeabi-v7a", "armeabi"], 5.6));
        assert_eq!(hw.os, "android");
        assert_eq!(hw.arch, "aarch64");
        assert_eq!(hw.memory_gb, Some(5.6));
        assert_eq!(hw.profile, "android-6gb");
        assert_eq!(hw.source, "injected");
        assert!(hw.gpus.is_empty());
        assert!((hw.available_gb() - 2.8).abs() < 1e-9);
    }

    #[test]
    fn the_suite_abi_wins_over_an_earlier_unsupported_one_and_unknowns_stay_unknown() {
        let both = facts(&["armeabi-v7a", "arm64-v8a"], 4.0);
        assert_eq!(both.primary_abi(), Some(AndroidAbi::ArmeabiV7a));
        assert_eq!(both.suite_abi(), Some(AndroidAbi::Arm64V8a));
        assert_eq!(Hardware::from_android(&both).arch, "aarch64");
        let only_32 = facts(&["armeabi-v7a"], 2.0);
        assert_eq!(only_32.suite_abi(), None);
        assert_eq!(Hardware::from_android(&only_32).arch, "arm");
        let none = AndroidFacts::default();
        let hw = Hardware::from_android(&none);
        assert_eq!(hw.arch, std::env::consts::ARCH);
        assert_eq!(hw.memory_gb, None);
        assert_eq!(hw.profile, "android-0gb");
    }

    #[test]
    fn a_requirement_over_the_usable_pool_is_unsupported_not_permanent() {
        use crate::hardware::{FitKind, Requirements};
        let hw = Hardware::from_android(&facts(&["arm64-v8a"], 4.0));
        let small = Requirements {
            memory_gb: Some(1.5),
            ..Requirements::default()
        };
        let large = Requirements {
            memory_gb: Some(3.0),
            ..Requirements::default()
        };
        assert_eq!(hw.estimate_fit(&small, &[]).kind, FitKind::Estimated);
        let fit = hw.estimate_fit(&large, &[]);
        assert_eq!(fit.kind, FitKind::Unsupported);
        assert!(fit.reasons[0].contains("not a permanent limit"));
    }

    #[test]
    fn meminfo_total_is_read_in_bytes() {
        let text = "MemTotal:        5845120 kB\nMemFree:          123456 kB\n";
        assert_eq!(meminfo_total_bytes(text), Some(5_845_120.0 * 1024.0));
        assert_eq!(meminfo_total_bytes("MemFree: 1 kB\n"), None);
        assert_eq!(meminfo_total_bytes("MemTotal: lots kB\n"), None);
    }

    #[test]
    fn files_dir_must_be_absolute_and_free_of_parent_components() {
        let abs = std::env::temp_dir().join("data").join("app").join("files");
        assert_eq!(validate_files_dir(&abs).unwrap(), abs);
        let parent = abs.join("..").join("x");
        for bad in [
            Path::new(""),
            Path::new("files"),
            Path::new("./files"),
            parent.as_path(),
        ] {
            let error = validate_files_dir(bad).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidSpec, "{bad:?}");
        }
        assert_eq!(android_models_root(&abs).unwrap(), abs.join("models"));
        assert!(android_models_root(Path::new("files")).is_err());
    }

    #[test]
    fn an_android_store_lives_in_files_dir_models_and_refuses_relative_dirs() {
        let abs = std::env::temp_dir().join("data").join("app").join("files");
        let store = crate::suite::ModelStore::at_android_files_dir(&abs, "heardright").unwrap();
        assert_eq!(store.models_root(), abs.join("models"));
        assert_eq!(store.app_id(), "heardright");
        assert!(crate::suite::ModelStore::at_android_files_dir(Path::new("rel"), "x").is_err());
    }

    #[test]
    fn the_injected_root_is_set_once_and_the_same_value_is_idempotent() {
        let slot = InjectedRoot::new();
        let a = std::env::temp_dir().join("a-files");
        let b = std::env::temp_dir().join("b-files");
        assert_eq!(slot.get(), None);
        slot.set(&a).unwrap();
        slot.set(&a).unwrap();
        assert_eq!(slot.get(), Some(a.clone()));
        assert_eq!(slot.set(&b).unwrap_err().code, ErrorCode::Conflict);
        assert_eq!(slot.get(), Some(a));
        assert!(InjectedRoot::new().set(Path::new("rel")).is_err());
    }
}
