//! Execution-provider availability probe against the loaded runtime.

#[cfg(any(target_vendor = "apple", target_os = "windows"))]
use ort::ep::ExecutionProvider as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    DirectMl,
    CoreMl,
    Cpu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDiagnostic {
    pub provider: ProviderKind,
    pub available: bool,
    pub detail: String,
}

/// Ask the loaded runtime which execution providers it was built with and
/// whether this OS can host them. Does not create a session. A provider is
/// `available` only when the platform supports it AND the library was compiled
/// with it ("compiled in" is not "usable for this model"; sessions use
/// `error_on_failure`, so a failed registration is reported, never hidden).
///
/// DirectML needs more than the runtime: its own library, `DirectML.dll`, beside
/// `onnxruntime.dll`. A runtime built with DirectML says "compiled in" without it, and
/// Windows then offers its own older `DirectML.dll` from the system directory, which the
/// pinned runtime is not qualified against. So DirectML is `available` only when the
/// `DirectML.dll` beside the configured runtime ([`crate::configured_runtime`]) loads; the
/// detail says "compiled in, library missing" otherwise.
///
/// The runtime must already be configured (`configure_runtime` or
/// `init_environment`); a missing library is reported instead of panicking.
pub fn probe_providers() -> Vec<ProviderDiagnostic> {
    vec![
        ProviderDiagnostic {
            provider: ProviderKind::Cpu,
            available: true,
            detail: "always compiled in".into(),
        },
        diagnose(ProviderKind::CoreMl, coreml_probe()),
        directml_diagnostic(),
    ]
}

/// What was found of DirectML's own library beside the configured runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
enum ProviderLibrary {
    /// It is there and the loader accepts it.
    Loads,
    /// The runtime's directory has no such file.
    Missing,
    /// It is there and does not load.
    Unloadable(String),
    /// No runtime path is recorded for this process, so there is no directory to look in.
    RuntimeUnknown,
}

/// DirectML is usable when it is compiled into the runtime AND its library loads.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn directml_verdict(probe: Probe, library: impl FnOnce() -> ProviderLibrary) -> ProviderDiagnostic {
    let compiled = diagnose(ProviderKind::DirectMl, probe);
    if !compiled.available {
        return compiled;
    }
    let (available, detail) = match library() {
        ProviderLibrary::Loads => (
            true,
            "compiled into the loaded runtime; DirectML.dll beside it loads".to_string(),
        ),
        ProviderLibrary::Missing => (
            false,
            "compiled in, library missing: no DirectML.dll beside the runtime".to_string(),
        ),
        ProviderLibrary::Unloadable(why) => (
            false,
            format!("compiled in, but the DirectML.dll beside the runtime does not load: {why}"),
        ),
        ProviderLibrary::RuntimeUnknown => (
            false,
            "compiled in, but no runtime path is configured, so DirectML.dll could not be checked"
                .to_string(),
        ),
    };
    ProviderDiagnostic {
        provider: ProviderKind::DirectMl,
        available,
        detail,
    }
}

#[cfg(target_os = "windows")]
fn directml_diagnostic() -> ProviderDiagnostic {
    directml_verdict(directml_probe(), directml_library)
}

#[cfg(not(target_os = "windows"))]
fn directml_diagnostic() -> ProviderDiagnostic {
    diagnose(ProviderKind::DirectMl, directml_probe())
}

/// Look for `DirectML.dll` in the directory of the configured runtime and ask the loader to map
/// it, searching only that directory and the system directory for what it imports.
#[cfg(target_os = "windows")]
fn directml_library() -> ProviderLibrary {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }

    let Some(directory) = crate::configured_runtime()
        .and_then(|runtime| runtime.parent().map(std::path::Path::to_path_buf))
    else {
        return ProviderLibrary::RuntimeUnknown;
    };
    let library = directory.join("DirectML.dll");
    if !library.is_file() {
        return ProviderLibrary::Missing;
    }
    let Ok(library) = library.canonicalize() else {
        return ProviderLibrary::Missing;
    };
    let name: Vec<u16> = library.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `name` is an absolute, NUL-terminated path that outlives the call; the file
    // argument is reserved and null.
    let module = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return ProviderLibrary::Unloadable(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: `module` is the handle the load returned; it is released once. The runtime
    // takes its own reference when it registers the provider.
    unsafe { FreeLibrary(module) };
    ProviderLibrary::Loads
}

/// `None`: this OS cannot host the provider (ort rc.13 compiles each EP only on
/// the OS whose Cargo feature rightkit-ort enables for it).
type Probe = Option<Result<bool, String>>;

fn diagnose(provider: ProviderKind, probe: Probe) -> ProviderDiagnostic {
    let (available, detail) = match probe {
        None => (false, "not supported on this OS".to_string()),
        Some(Ok(true)) => (true, "compiled into the loaded runtime".to_string()),
        Some(Ok(false)) => (false, "not compiled into the loaded runtime".to_string()),
        Some(Err(e)) => (false, format!("probe failed: {e}")),
    };
    ProviderDiagnostic {
        provider,
        available,
        detail,
    }
}

#[cfg(target_vendor = "apple")]
fn coreml_probe() -> Probe {
    Some(guarded(|| ort::ep::CoreML::default().is_available()))
}

#[cfg(not(target_vendor = "apple"))]
fn coreml_probe() -> Probe {
    None
}

#[cfg(target_os = "windows")]
fn directml_probe() -> Probe {
    Some(guarded(|| ort::ep::DirectML::default().is_available()))
}

#[cfg(not(target_os = "windows"))]
fn directml_probe() -> Probe {
    None
}

#[cfg(any(target_vendor = "apple", target_os = "windows"))]
fn guarded(f: impl FnOnce() -> ort::Result<bool> + std::panic::UnwindSafe) -> Result<bool, String> {
    match std::panic::catch_unwind(f) {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(_) => Err("ONNX Runtime library could not be loaded".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(probe: Probe, library: ProviderLibrary) -> (bool, String) {
        let diagnostic = directml_verdict(probe, || library);
        assert_eq!(diagnostic.provider, ProviderKind::DirectMl);
        (diagnostic.available, diagnostic.detail)
    }

    #[test]
    fn directml_is_usable_only_when_compiled_in_and_its_library_loads() {
        assert_eq!(
            verdict(Some(Ok(true)), ProviderLibrary::Loads),
            (
                true,
                "compiled into the loaded runtime; DirectML.dll beside it loads".into()
            )
        );
        // Compiled in is not usable: the library decides.
        let (available, detail) = verdict(Some(Ok(true)), ProviderLibrary::Missing);
        assert!(!available);
        assert!(
            detail.starts_with("compiled in, library missing"),
            "{detail}"
        );
        let (available, detail) = verdict(
            Some(Ok(true)),
            ProviderLibrary::Unloadable("The specified module could not be found.".into()),
        );
        assert!(!available);
        assert!(
            detail.contains("does not load: The specified module could not be found."),
            "{detail}"
        );
        let (available, detail) = verdict(Some(Ok(true)), ProviderLibrary::RuntimeUnknown);
        assert!(!available);
        assert!(detail.contains("no runtime path is configured"), "{detail}");
    }

    #[test]
    fn the_library_is_not_looked_for_when_the_runtime_lacks_the_provider() {
        let never = || -> ProviderLibrary { panic!("the library must not be checked") };
        for (probe, detail) in [
            (None, "not supported on this OS"),
            (Some(Ok(false)), "not compiled into the loaded runtime"),
            (Some(Err("boom".to_string())), "probe failed: boom"),
        ] {
            let diagnostic = directml_verdict(probe, never);
            assert!(!diagnostic.available);
            assert_eq!(diagnostic.detail, detail);
        }
    }
}
