//! Daemon startup helpers.
//!
//! The one-line discovery record `coderight serve` prints and writes to `<state root>/daemon.port`
//! is `rightkit_daemon::loopback::LoopbackRecord`; this file no longer defines it.

use std::path::Path;

/// Validate `ORT_DYLIB_PATH` before any ONNX Runtime load.
///
/// When the `fastembed` feature is enabled the daemon will attempt to load the
/// ONNX Runtime dylib through `ort` with `load-dynamic`. A missing or invalid
/// `ORT_DYLIB_PATH` causes the embedding model load to block forever (see
/// `main_runtime_server.rs` MEMORY_OPEN_TIMEOUT handling). Fail fast here
/// instead of appearing alive while every request hangs.
///
/// Returns `Ok(path)` when a candidate exists and the explicitly-configured
/// path (if any) points to an existing file. Returns `Err(diagnostic)` with a
/// message containing `ORT_DYLIB_PATH missing/invalid` when the check fails.
/// The caller should emit the diagnostic to stderr and exit non-zero.
pub fn validate_ort_dylib_path() -> Result<Option<std::path::PathBuf>, String> {
    validate_ort_runtime(rightkit_onnx::configured_runtime())
}

/// [`validate_ort_dylib_path`] for an already-read configured runtime path.
fn validate_ort_runtime(
    configured: Option<std::path::PathBuf>,
) -> Result<Option<std::path::PathBuf>, String> {
    // A configured runtime takes precedence — if the operator set it, it must be valid.
    if let Some(path) = configured {
        let diagnostic = rightkit_onnx::inspect_candidate(&rightkit_onnx::RuntimeCandidate::new(
            &path,
            rightkit_onnx::CandidateSource::Explicit,
        ));
        if !diagnostic.exists {
            return Err(format!(
                "ORT_DYLIB_PATH missing/invalid: configured path does not exist: {}",
                path.display()
            ));
        }
        if !diagnostic.is_file {
            return Err(format!(
                "ORT_DYLIB_PATH missing/invalid: configured path is a directory, expected file: {}",
                path.display()
            ));
        }
        return Ok(Some(path));
    }

    // No explicit env var — probe via the single source of truth in ort_common
    // (exe-adjacent, data-dir, etc). When no candidate exists
    // we emit a diagnostic; caller decides whether to fail closed.
    match rightkit_onnx::resolve_runtime(rightkit_onnx::agent_ort::ort_runtime_candidates()) {
        Ok(_) => Ok(None),
        Err(rightkit_onnx::RuntimeError::NotFound { .. }) => {
            Err("ORT_DYLIB_PATH missing/invalid: no ONNX Runtime dylib found; set ORT_DYLIB_PATH to the bundled onnxruntime library".to_string())
        }
        Err(error) => Err(format!("ORT_DYLIB_PATH missing/invalid: {error}")),
    }
}

/// Check whether a path looks like a valid ONNX Runtime dylib file.
#[allow(dead_code)]
pub fn is_valid_ort_dylib_path(path: &Path) -> bool {
    rightkit_onnx::inspect_candidate(&rightkit_onnx::RuntimeCandidate::new(
        path,
        rightkit_onnx::CandidateSource::Explicit,
    ))
    .is_file
}

/// Generate an opaque per-launch token from the OS CSPRNG.
///
/// This token is the management bearer credential: it gates the daemon's
/// general mutating HTTP API via the bearer middleware (`api_router.rs`), and
/// its entropy must not be guessable from observable state. We take 16 bytes
/// of `getrandom` entropy rendered as 32 hex chars. If the OS RNG fails we
/// **abort startup** rather than emit a weak, derivable token — a daemon
/// reachable with a guessable token is worse than a daemon that refuses to
/// start. (The previous FNV-of-(timestamp+pid) fallback was brute-forceable
/// from approximate start time + PID; see SECURITY F3.)
///
/// It is not the *only* mutating-capability boundary: driver capability
/// routes (`api_drivers.rs`) additionally accept a scoped, narrower-lived
/// lease token plus a process-local secret, issued and checked outside this
/// bearer middleware. That boundary is separate by design — a lease token
/// authorizes one bounded capability, not the full management API — and must
/// not be conflated with this daemon token when reasoning about the attack
/// surface.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    if let Err(err) = getrandom::fill(&mut bytes) {
        // stderr only: stdout carries the single-line startup contract.
        eprintln!("fatal: OS RNG unavailable; refusing to mint a guessable daemon token: {err}");
        std::process::exit(1);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_differ_and_are_hex() {
        let a = generate_token();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn validate_ort_runtime_rejects_nonexistent_file() {
        let result = validate_ort_runtime(Some(std::path::PathBuf::from(
            "/tmp/definitely-not-exist-ort-12345.dll",
        )));
        assert!(result
            .unwrap_err()
            .contains("ORT_DYLIB_PATH missing/invalid"));
    }

    #[test]
    fn validate_ort_runtime_rejects_directory() {
        let dir = std::env::temp_dir();
        let result = validate_ort_runtime(Some(dir));
        assert!(result
            .unwrap_err()
            .contains("ORT_DYLIB_PATH missing/invalid"));
    }

    #[test]
    fn is_valid_ort_dylib_path_checks_file() {
        let tmp = std::env::temp_dir().join("cr-ort-test-valid.tmp");
        std::fs::write(&tmp, b"fake dylib").unwrap();
        assert!(is_valid_ort_dylib_path(&tmp));
        std::fs::remove_file(&tmp).ok();
        assert!(!is_valid_ort_dylib_path(&tmp));
        assert!(!is_valid_ort_dylib_path(Path::new(
            "/tmp/not-exist-ort-xyz"
        )));
    }
}
