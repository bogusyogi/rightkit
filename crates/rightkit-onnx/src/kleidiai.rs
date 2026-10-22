//! Known-bad ONNX Runtime guard for Apple SME2 hosts (bogusyogi/rightkit#889).
//!
//! ONNX Runtime 1.26.0 and older keep a KleidiAI SME2 Conv cache that leaks about 92 KB per Conv
//! run on Apple silicon with `FEAT_SME2` (M4 and newer). Upstream fixed it in 1.27.0
//! (microsoft/onnxruntime#28363). This module holds the pure decision rules and the two host
//! probes the rest of the crate uses:
//!
//! * **Bind time** ([`crate::configure_runtime_with_options`], and so [`crate::configure_runtime`]
//!   and [`crate::configure_runtime_verified`]): on a macOS arm64 host whose
//!   `hw.optional.arm.FEAT_SME2` sysctl is 1, a runtime older than [`crate::kleidiai::MIN_SAFE_ORT_VERSION`] is
//!   refused with [`crate::RuntimeError::KnownBadRuntime`]. The version is read from the library
//!   file actually being bound (ONNX Runtime's own `OrtGetApiBase().GetVersionString()`), never
//!   from a catalog pin. [`crate::RuntimeBindingOptions::allow_known_bad_runtime`] is the explicit
//!   opt-out for QA reproductions; with it the bind succeeds and every session built through
//!   `SessionOptions` gets `mlas.disable_kleidiai = 1` for the rest of the process.
//! * **Session time** (`SessionOptions::with_kleidiai`): `with_kleidiai(false)` sets the
//!   same entry on any runtime. On ONNX Runtime 1.27.0 or newer KleidiAI stays on by default.
//!
//! An unreadable or unparseable version on SME2 is refused unless the opt-out is set;
//! the opt-out forces the same session mitigation as a known-bad version.
//! A runtime bound without this crate (an `ORT_DYLIB_PATH` the app set itself) is not guarded.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Oldest ONNX Runtime without the SME2 Conv cache leak.
pub const MIN_SAFE_ORT_VERSION: OrtVersion = OrtVersion {
    major: 1,
    minor: 27,
    patch: 0,
};

/// The issue the refusal names.
pub const ISSUE: &str = "bogusyogi/rightkit#889";

/// Host feature the refusal names.
pub const HOST_FEATURE: &str = "hw.optional.arm.FEAT_SME2";

/// Session config entry that turns the KleidiAI kernels off (`kOrtSessionOptionsMlasDisableKleidiAi`,
/// present since ONNX Runtime 1.26; older runtimes ignore an unknown entry).
pub const CONFIG_KEY_DISABLE: &str = "mlas.disable_kleidiai";

/// Set when a runtime older than [`MIN_SAFE_ORT_VERSION`] was bound on an SME2 host through the
/// documented opt-out; sticky for the process (one runtime per process).
static FORCED_OFF: AtomicBool = AtomicBool::new(false);

/// An ONNX Runtime release number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrtVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl OrtVersion {
    /// Parse `1.26.0`, `1.27` (patch 0) or a version with a `-`/`+` suffix (`1.28.0-dev`,
    /// `1.26.0+abc`). Anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let core = text
            .trim()
            .split(['-', '+', ' '])
            .next()
            .filter(|core| !core.is_empty())?;
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = match parts.next() {
            Some(patch) => patch.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for OrtVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What the guard needs to know about the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host {
    /// macOS on aarch64 (the only place the leak exists).
    pub apple_arm64: bool,
    /// `hw.optional.arm.FEAT_SME2` is 1.
    pub sme2: bool,
}

impl Host {
    /// Probe this host. Off macOS arm64 nothing is probed and the guard does not apply.
    pub fn detect() -> Self {
        let apple_arm64 = cfg!(all(target_os = "macos", target_arch = "aarch64"));
        Self {
            apple_arm64,
            sme2: apple_arm64 && sysctl_flag(HOST_FEATURE),
        }
    }

    /// True on a host where an old runtime leaks.
    pub fn affected(self) -> bool {
        self.apple_arm64 && self.sme2
    }
}

/// The guard's decision for one runtime on one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The host cannot hit the leak (not macOS arm64, or no SME2).
    NotApplicable,
    /// Affected host, runtime at or above [`MIN_SAFE_ORT_VERSION`].
    Safe,
    /// Reserved for compatibility; unknown versions now produce `Refuse` or `ForceDisable`.
    Unknown,
    /// Affected host, known-bad runtime, opt-out not given: refuse the bind.
    Refuse,
    /// Affected host, known-bad runtime, opt-out given: bind, and force
    /// `mlas.disable_kleidiai = 1` on every session.
    ForceDisable,
}

/// Pure decision: no probing, no I/O.
pub fn assess(host: Host, version: Option<OrtVersion>, allow_known_bad: bool) -> Verdict {
    if !host.affected() {
        return Verdict::NotApplicable;
    }
    match version {
        None if allow_known_bad => Verdict::ForceDisable,
        None => Verdict::Refuse,
        Some(version) if version >= MIN_SAFE_ORT_VERSION => Verdict::Safe,
        Some(_) if allow_known_bad => Verdict::ForceDisable,
        Some(_) => Verdict::Refuse,
    }
}

/// Apply [`assess`] to a runtime file. `read_version` is only called on an affected host.
/// `Ok(true)` means the bind is allowed and sessions must force KleidiAI off.
pub(crate) fn check_bind_with(
    host: Host,
    path: &Path,
    allow_known_bad: bool,
    read_version: impl FnOnce(&Path) -> Result<OrtVersion, String>,
) -> Result<bool, crate::RuntimeError> {
    if !host.affected() {
        return Ok(false);
    }
    let version = read_version(path).ok();
    match assess(host, version, allow_known_bad) {
        Verdict::Refuse => Err(crate::RuntimeError::KnownBadRuntime {
            path: path.to_path_buf(),
            version: version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".into()),
            host_feature: HOST_FEATURE,
            issue: ISSUE,
        }),
        Verdict::ForceDisable => Ok(true),
        Verdict::NotApplicable | Verdict::Safe | Verdict::Unknown => Ok(false),
    }
}

/// Bind-time check against the real host and the real library file.
pub(crate) struct BindCheck<T = RuntimeLibrary> {
    force_disable: bool,
    library: Option<T>,
}

impl<T> BindCheck<T> {
    pub(crate) fn commit(self) {
        if self.force_disable {
            force_off_for_process();
        }
        // Retain only after the binding (including conflict checks) succeeds.
        if let Some(library) = self.library {
            std::mem::forget(library);
        }
    }
}

fn check_probe<T>(
    host: Host,
    path: &Path,
    allow: bool,
    probe: Result<(OrtVersion, T), String>,
) -> Result<BindCheck<T>, crate::RuntimeError> {
    let (version, library) = match probe {
        Ok((version, library)) => (Ok(version), Some(library)),
        Err(error) => (Err(error), None),
    };
    let force_disable = check_bind_with(host, path, allow, |_| version)?;
    Ok(BindCheck {
        force_disable,
        library,
    })
}

pub(crate) fn check_bind(
    path: &Path,
    allow_known_bad: bool,
) -> Result<BindCheck, crate::RuntimeError> {
    let host = Host::detect();
    if !host.affected() {
        return Ok(BindCheck {
            force_disable: false,
            library: None,
        });
    }
    check_probe(host, path, allow_known_bad, probe_runtime_version(path))
}

/// Record that sessions must disable KleidiAI for the rest of the process.
pub(crate) fn force_off_for_process() {
    FORCED_OFF.store(true, Ordering::Release);
}

/// True when the opt-out bound a known-bad runtime on an SME2 host: every session built through
/// `SessionOptions` then sets [`CONFIG_KEY_DISABLE`] and `with_kleidiai(true)` cannot
/// override it.
pub fn forced_off() -> bool {
    FORCED_OFF.load(Ordering::Acquire)
}

/// Whether a session built with `explicit_disable` (`SessionOptions::with_kleidiai(false)`) sets
/// [`CONFIG_KEY_DISABLE`]. KleidiAI is an Arm kernel library, so an explicit request is only
/// meaningful on aarch64; a forced disable applies everywhere it was recorded.
pub fn session_disables(explicit_disable: bool, aarch64: bool, forced: bool) -> bool {
    forced || (explicit_disable && aarch64)
}

/// `sysctlbyname(name)` as an integer flag: true only when the call succeeds and returns 1.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn sysctl_flag(name: &str) -> bool {
    use std::ffi::{c_char, c_int, c_void, CString};

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }

    let Ok(name) = CString::new(name) else {
        return false;
    };
    let mut value: c_int = 0;
    let mut len = std::mem::size_of::<c_int>();
    // SAFETY: `name` is NUL-terminated and outlives the call; `value` and `len` are valid for the
    // sizes passed; no new value is set (null, 0).
    let status = unsafe {
        sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut c_int).cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    status == 0 && value == 1
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn sysctl_flag(_name: &str) -> bool {
    false
}

/// The version of the ONNX Runtime library at `path`, read through the library's own
/// `OrtGetApiBase().GetVersionString()` (the call ort makes when it loads it). The library is
/// closed after this standalone probe. Binding retains it only after all checks succeed.
pub fn read_runtime_version(path: &Path) -> Result<OrtVersion, String> {
    probe_runtime_version(path).map(|(version, _library)| version)
}

pub(crate) struct RuntimeLibrary {
    #[cfg(target_os = "macos")]
    handle: *mut std::ffi::c_void,
}

#[cfg(target_os = "macos")]
impl Drop for RuntimeLibrary {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn dlclose(handle: *mut std::ffi::c_void) -> std::ffi::c_int;
        }
        // SAFETY: this guard owns exactly one successful dlopen reference.
        unsafe {
            dlclose(self.handle);
        }
    }
}

// The owner stays live during every probe step and is dropped on any error.
#[cfg(any(target_os = "macos", test))]
fn inspect_library<T>(
    library: T,
    inspect: impl FnOnce() -> Result<OrtVersion, String>,
) -> Result<(OrtVersion, T), String> {
    let version = inspect()?;
    Ok((version, library))
}

#[cfg(target_os = "macos")]
fn probe_runtime_version(path: &Path) -> Result<(OrtVersion, RuntimeLibrary), String> {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlerror() -> *const c_char;
    }
    const RTLD_LAZY: c_int = 0x1;
    const RTLD_LOCAL: c_int = 0x4;

    // Layout of `OrtApiBase` in onnxruntime_c_api.h: GetApi, then GetVersionString.
    #[repr(C)]
    struct OrtApiBase {
        _get_api: unsafe extern "C" fn(u32) -> *const c_void,
        get_version_string: unsafe extern "C" fn() -> *const c_char,
    }
    type GetApiBase = unsafe extern "C" fn() -> *const OrtApiBase;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", path.display()))?;
    // SAFETY: `c_path` is a NUL-terminated absolute path that outlives the call.
    let handle = unsafe { dlopen(c_path.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: `dlerror` returns null or a NUL-terminated string valid until the next dl call.
        let message = unsafe {
            let text = dlerror();
            if text.is_null() {
                "unknown loader error".to_owned()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        };
        return Err(format!("cannot load {}: {message}", path.display()));
    }
    let library = RuntimeLibrary { handle };
    inspect_library(library, || {
        // SAFETY: `handle` is a live library handle; the symbol name is NUL-terminated.
        let symbol = unsafe { dlsym(handle, c"OrtGetApiBase".as_ptr()) };
        if symbol.is_null() {
            return Err(format!("{} does not export OrtGetApiBase", path.display()));
        }
        // SAFETY: `OrtGetApiBase` has the `GetApiBase` signature in every ONNX Runtime release.
        let get_base = unsafe { std::mem::transmute::<*mut c_void, GetApiBase>(symbol) };
        // SAFETY: the function takes no arguments and returns a pointer to a static struct.
        let base = unsafe { get_base() };
        if base.is_null() {
            return Err("OrtGetApiBase returned null".into());
        }
        // SAFETY: `base` points to the library's static `OrtApiBase`; `GetVersionString` returns a
        // NUL-terminated string owned by the library.
        let text = unsafe {
            let ptr = ((*base).get_version_string)();
            if ptr.is_null() {
                return Err("GetVersionString returned null".into());
            }
            CStr::from_ptr(ptr)
                .to_str()
                .map_err(|_| "GetVersionString is not UTF-8".to_owned())?
                .to_owned()
        };
        OrtVersion::parse(&text)
            .ok_or_else(|| format!("unrecognised ONNX Runtime version {text:?}"))
    })
}

/// Off macOS the guard never reads a version (it only applies to Apple arm64).
#[cfg(not(target_os = "macos"))]
fn probe_runtime_version(_path: &Path) -> Result<(OrtVersion, RuntimeLibrary), String> {
    Err("reading the ONNX Runtime version is only implemented on macOS".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeError;
    use std::path::PathBuf;

    const AFFECTED: Host = Host {
        apple_arm64: true,
        sme2: true,
    };
    const NO_SME2: Host = Host {
        apple_arm64: true,
        sme2: false,
    };
    const OTHER_OS: Host = Host {
        apple_arm64: false,
        sme2: false,
    };

    fn v(text: &str) -> Option<OrtVersion> {
        OrtVersion::parse(text)
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(
            v("1.26.0"),
            Some(OrtVersion {
                major: 1,
                minor: 26,
                patch: 0
            })
        );
        assert_eq!(v("1.27"), v("1.27.0"));
        assert_eq!(v("1.28.0-dev"), v("1.28.0"));
        assert_eq!(v(" 1.30.0+abc "), v("1.30.0"));
        for bad in ["", "1", "x.y.z", "1.2.3.4", "1..0", "-1.2.3", "1.27.x"] {
            assert_eq!(v(bad), None, "{bad:?}");
        }
        assert!(v("1.26.0") < Some(MIN_SAFE_ORT_VERSION));
        assert!(v("1.24.4") < v("1.26.0"));
        assert!(v("1.26.9") < v("1.27.0"));
        assert!(v("1.27.0") < v("1.27.1"));
        assert!(v("1.9.0") < v("1.27.0"));
        assert!(v("2.0.0") > v("1.30.0"));
        assert_eq!(MIN_SAFE_ORT_VERSION.to_string(), "1.27.0");
    }

    #[test]
    fn verdict_matrix() {
        for host in [NO_SME2, OTHER_OS] {
            for version in [v("1.24.4"), v("1.27.0"), None] {
                for allow in [false, true] {
                    assert_eq!(assess(host, version, allow), Verdict::NotApplicable);
                }
            }
        }
        assert_eq!(assess(AFFECTED, v("1.26.0"), false), Verdict::Refuse);
        assert_eq!(assess(AFFECTED, v("1.24.4"), false), Verdict::Refuse);
        assert_eq!(assess(AFFECTED, v("1.26.0"), true), Verdict::ForceDisable);
        assert_eq!(assess(AFFECTED, v("1.27.0"), false), Verdict::Safe);
        assert_eq!(assess(AFFECTED, v("1.28.3"), true), Verdict::Safe);
        assert_eq!(assess(AFFECTED, v("1.30.0"), false), Verdict::Safe);
        assert_eq!(assess(AFFECTED, None, false), Verdict::Refuse);
        assert_eq!(assess(AFFECTED, None, true), Verdict::ForceDisable);
    }

    #[test]
    fn host_applies_only_on_apple_arm64_with_sme2() {
        assert!(AFFECTED.affected());
        assert!(!NO_SME2.affected());
        assert!(!OTHER_OS.affected());
        // sme2 without apple_arm64 is not a reachable probe result, and still does not apply.
        assert!(!Host {
            apple_arm64: false,
            sme2: true
        }
        .affected());
    }

    #[test]
    fn bind_refuses_a_known_bad_runtime_naming_version_feature_and_issue() {
        let path = PathBuf::from("/app/runtime/libonnxruntime.dylib");
        let error = check_bind_with(AFFECTED, &path, false, |_| {
            Ok(OrtVersion::parse("1.26.0").unwrap())
        })
        .unwrap_err();
        match &error {
            RuntimeError::KnownBadRuntime {
                path: refused,
                version,
                host_feature,
                issue,
            } => {
                assert_eq!(refused, &path);
                assert_eq!(version, "1.26.0");
                assert_eq!(*host_feature, "hw.optional.arm.FEAT_SME2");
                assert_eq!(*issue, "bogusyogi/rightkit#889");
            }
            other => panic!("unexpected {other:?}"),
        }
        let text = error.to_string();
        for needle in [
            "1.26.0",
            "hw.optional.arm.FEAT_SME2",
            "rightkit#889",
            "1.27.0",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
    }

    #[test]
    fn bind_with_opt_out_allows_and_forces_the_session_entry() {
        let path = PathBuf::from("/app/runtime/libonnxruntime.dylib");
        let forced = check_bind_with(AFFECTED, &path, true, |_| {
            Ok(OrtVersion::parse("1.26.0").unwrap())
        });
        assert!(forced.unwrap());
    }

    #[test]
    fn bind_accepts_a_fixed_runtime_without_forcing() {
        let path = PathBuf::from("/app/runtime/libonnxruntime.dylib");
        for version in ["1.27.0", "1.28.3", "1.30.0"] {
            let forced = check_bind_with(AFFECTED, &path, false, |_| {
                Ok(OrtVersion::parse(version).unwrap())
            });
            assert!(!forced.unwrap(), "{version}");
        }
    }

    #[test]
    fn bind_does_not_read_the_library_on_unaffected_hosts() {
        let path = PathBuf::from("/app/runtime/libonnxruntime.dylib");
        for host in [NO_SME2, OTHER_OS] {
            let forced = check_bind_with(host, &path, false, |_| panic!("must not read"));
            assert!(!forced.unwrap());
        }
    }

    #[test]
    fn unknown_versions_fail_closed_or_force_mitigation() {
        let path = Path::new("/app/runtime/libonnxruntime.dylib");
        for text in ["garbage", "1.26.0.dev20261022"] {
            for allow in [false, true] {
                let result = check_bind_with(AFFECTED, path, allow, |_| {
                    OrtVersion::parse(text).ok_or_else(|| "unknown".into())
                });
                if allow {
                    assert!(result.unwrap());
                } else {
                    assert!(matches!(result, Err(RuntimeError::KnownBadRuntime { .. })));
                }
            }
        }
    }

    #[test]
    fn probe_ownership_follows_binding_outcome() {
        use std::cell::Cell;
        struct Handle<'a>(&'a Cell<usize>);
        impl Drop for Handle<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let closed = Cell::new(0);
        let path = Path::new("runtime");
        for error in [
            "missing OrtGetApiBase",
            "null API base",
            "null version",
            "invalid UTF-8",
            "unparseable version",
        ] {
            let before = closed.get();
            let probe = inspect_library(Handle(&closed), || Err(error.into()));
            assert!(probe.is_err());
            assert_eq!(closed.get(), before + 1, "{error}");
            assert!(check_probe::<Handle<'_>>(AFFECTED, path, false, probe).is_err());
        }
        closed.set(0);
        assert!(check_probe(
            AFFECTED,
            path,
            false,
            Ok((v("1.26.0").unwrap(), Handle(&closed)))
        )
        .is_err());
        assert_eq!(closed.get(), 1);
        // Accepted probe, but a later binding conflict: dropping must close it too.
        drop(
            check_probe(
                AFFECTED,
                path,
                false,
                Ok((v("1.30.0").unwrap(), Handle(&closed))),
            )
            .unwrap(),
        );
        assert_eq!(closed.get(), 2);
        check_probe(
            AFFECTED,
            path,
            false,
            Ok((v("1.30.0").unwrap(), Handle(&closed))),
        )
        .unwrap()
        .commit();
        assert_eq!(closed.get(), 2);
    }

    #[test]
    fn session_entry_rules() {
        // Default: nothing on any host.
        assert!(!session_disables(false, true, false));
        assert!(!session_disables(false, false, false));
        // Explicit with_kleidiai(false): on Arm only.
        assert!(session_disables(true, true, false));
        assert!(!session_disables(true, false, false));
        // Forced by the opt-out bind: always, whatever the caller asks.
        assert!(session_disables(false, true, true));
        assert!(session_disables(false, false, true));
    }

    #[test]
    fn config_key_is_the_upstream_entry() {
        assert_eq!(CONFIG_KEY_DISABLE, "mlas.disable_kleidiai");
    }
}
