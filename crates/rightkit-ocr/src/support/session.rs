use rightkit_onnx::ort::session::{builder::GraphOptimizationLevel, Session};
use rightkit_onnx::{ProviderDiagnostic, ProviderKind};
use std::path::Path;

/// Build an ort session. Windows tries DirectML first, falling back to CPU;
/// other platforms (macOS) run on CPU directly. OCR is light, so CPU is fine.
///
/// DirectML requires sequential execution + memory pattern disabled.
pub(crate) fn build_session(path: &Path) -> Result<(Session, Vec<ProviderDiagnostic>), String> {
    let mut diagnostics = Vec::new();
    #[cfg(windows)]
    {
        match build_directml_session(path) {
            Ok(s) => {
                diagnostics.push(ProviderDiagnostic {
                    provider: ProviderKind::DirectMl,
                    available: true,
                    detail: "session created".to_string(),
                });
                return Ok((s, diagnostics));
            }
            Err(e) => {
                diagnostics.push(ProviderDiagnostic {
                    provider: ProviderKind::DirectMl,
                    available: false,
                    detail: e,
                });
            }
        }
    }

    let session = Session::builder()
        .map_err(|e| e.to_string())?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())?
        .commit_from_file(path)
        .map_err(|e| e.to_string())?;
    diagnostics.push(ProviderDiagnostic {
        provider: ProviderKind::Cpu,
        available: true,
        detail: "session created".to_string(),
    });
    Ok((session, diagnostics))
}

#[cfg(windows)]
fn build_directml_session(path: &Path) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| e.to_string())?
        .with_execution_providers([rightkit_onnx::ort::ep::DirectML::default()
            .build()
            .error_on_failure()])
        .map_err(|e| e.to_string())?
        .with_parallel_execution(false)
        .map_err(|e| e.to_string())?
        .with_memory_pattern(false)
        .map_err(|e| e.to_string())?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())?
        .commit_from_file(path)
        .map_err(|e| e.to_string())
}
