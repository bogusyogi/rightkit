//! Execution-provider availability probe against the loaded runtime.

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
    let mut out = vec![ProviderDiagnostic {
        provider: ProviderKind::Cpu,
        available: true,
        detail: "always compiled in".into(),
    }];
    let coreml = ort::ep::CoreML::default();
    let directml = ort::ep::DirectML::default();
    for (kind, supported, available) in [
        (
            ProviderKind::CoreMl,
            coreml.supported_by_platform(),
            guarded(|| coreml.is_available()),
        ),
        (
            ProviderKind::DirectMl,
            directml.supported_by_platform(),
            guarded(|| directml.is_available()),
        ),
    ] {
        let (available, detail) = match (supported, available) {
            (false, _) => (false, "not supported on this OS".to_string()),
            (true, Ok(true)) => (true, "compiled into the loaded runtime".to_string()),
            (true, Ok(false)) => (false, "not compiled into the loaded runtime".to_string()),
            (true, Err(e)) => (false, format!("probe failed: {e}")),
        };
        out.push(ProviderDiagnostic {
            provider: kind,
            available,
            detail,
        });
    }
    out
}

fn guarded(f: impl FnOnce() -> ort::Result<bool> + std::panic::UnwindSafe) -> Result<bool, String> {
    match std::panic::catch_unwind(f) {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(_) => Err("ONNX Runtime library could not be loaded".into()),
    }
}
