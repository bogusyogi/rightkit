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
        diagnose(ProviderKind::DirectMl, directml_probe()),
    ]
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
