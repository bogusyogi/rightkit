//! the host app environment, bundle/data-root & model adapters for RightKit ORT.
//! RightKit owns platform filenames, runtime inspection, selection & setup.
//! Call [`configure_ort_dylib`] before sessions or worker threads start.

use crate::{CandidateSource, RuntimeCandidate};
use std::path::PathBuf;
use std::sync::Once;

pub use crate::runtime_filename as ort_dylib_filename;

static ORT_DYLIB_INIT: Once = Once::new();
static EMBED_MODEL_INIT: Once = Once::new();

/// Set `ORT_DYLIB_PATH` to a known-good DirectML `onnxruntime.dll` if not
/// already configured by the user.
///
/// Call this once at process startup **before** any `ort` session is built or
/// worker threads are spawned. It mutates process-global environment state.
pub fn configure_ort_dylib() {
    ORT_DYLIB_INIT.call_once(|| {
        if crate::configured_runtime().is_some() {
            return;
        }
        // Missing runtime remains nonfatal here; serve validates it for fastembed.
        let _ = crate::configure_runtime(ort_runtime_candidates());
    });
}

/// Set `RIGHTKIT_EMBED_MODEL_DIR` to the installed `embed-model` resource directory
/// (RightKit's installed-bundle layout) if not already configured, so the CLI daemon finds
/// the model offline without requiring the parent to export the env.
pub fn configure_embed_model_dir() {
    EMBED_MODEL_INIT.call_once(|| {
        if env_path("RIGHTKIT_EMBED_MODEL_DIR").is_some() {
            return;
        }
        let installed = std::env::current_exe()
            .ok()
            .and_then(|exe| crate::installed_resource_dir(&exe))
            .map(|dir| dir.join("embed-model"));
        if let Some(path) = installed.filter(|p| p.is_dir()) {
            std::env::set_var("RIGHTKIT_EMBED_MODEL_DIR", path);
        }
    });
}

/// All locations probed for the ONNX Runtime dylib, in priority order.
///
/// This is a public iterator so downstream code (e.g. the Doctor endpoint in
/// `commands.rs`) can check for existence without calling the side-effecting
/// `configure_ort_dylib`.
pub fn ort_dylib_candidates() -> Vec<PathBuf> {
    ort_runtime_candidates()
        .into_iter()
        .map(|candidate| candidate.path)
        .collect()
}

/// Product-owned candidate precedence, adapted to RightKit's absolute paths.
pub fn ort_runtime_candidates() -> Vec<RuntimeCandidate> {
    let mut candidates = Vec::new();
    // RightKit's ordered search: a non-empty `ORT_DYLIB_PATH`, `<exe dir>/runtime`, `<exe dir>`.
    // The app-data directories stay below, after the product's own bundle layouts.
    candidates.extend(crate::discovery_candidates(std::iter::empty()));
    // Installed bundle layout: `<resources>/runtime/<runtime file>`.
    if let Some(candidate) = std::env::current_exe()
        .ok()
        .and_then(|exe| crate::installed_runtime_candidate(&exe, "runtime"))
    {
        candidates.push(candidate);
    }
    // Exe-adjacent locations: covers `cargo run` dev binaries, CLI `host app.exe`
    // next to its resources, and Tauri's `resources/runtime/` layout. Must precede
    // generic data-dir probing so the bundled DirectML build is preferred over the
    // Windows-ML system DLL (`System32\onnxruntime.dll` hangs at session init).
    candidates.extend(
        exe_candidates()
            .into_iter()
            .map(|path| RuntimeCandidate::new(path, CandidateSource::Bundled)),
    );
    // Per-user data dir: `LOCALAPPDATA/the host app/runtime` on Windows, `~/Library/...
    //` on macOS, `~/.local/share/the host app/runtime` on Linux.
    if let Some(dir) = user_data_dir() {
        candidates.push(RuntimeCandidate::new(
            dir.join("runtime").join(ort_dylib_filename()),
            CandidateSource::AppData,
        ));
    }
    candidates.push(RuntimeCandidate::new(
        ort_dylib_filename(),
        CandidateSource::Bundled,
    ));
    // The bare runtime filename is relative to cwd; RightKit takes absolute paths.
    let cwd = std::env::current_dir().ok();
    candidates
        .into_iter()
        .map(|mut candidate| {
            if candidate.path.is_relative() {
                if let Some(dir) = cwd.as_ref() {
                    candidate.path = dir.join(candidate.path);
                }
            }
            candidate
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn user_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })
    }
    .map(|base| base.join("Agent"))
}

fn exe_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let filename = ort_dylib_filename();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // (`<exe dir>` and `<exe dir>/runtime` come from `crate::discovery_candidates`.)
            // Tauri bundle: resources are beside exe or in `resources/`
            out.push(dir.join("resources").join("runtime").join(filename));
            // Installed app: `host app.exe` at `<app>/host app.exe`, resources at `<app>/resources/runtime/`
            // or sidecar at `<app>/host_engine/host app.exe`, with runtime
            // at `<app>/runtime/`; macOS also uses `<app>/../Resources/runtime/`.
            if let Some(parent) = dir.parent() {
                out.push(parent.join("runtime").join(filename));
                out.push(parent.join("Resources").join("runtime").join(filename));
                out.push(parent.join("resources").join("runtime").join(filename));
            }
            // Dev layout: `engine/target/debug/host app` -> repo root `host-app/src-tauri/resources/runtime/`
            // Walk up to find repo root via `engine/Cargo.toml`
            let mut probe = dir.to_path_buf();
            for _ in 0..6 {
                let candidate = probe
                    .join("apps")
                    .join("rightkit-agent-tauri")
                    .join("src-tauri")
                    .join("resources")
                    .join("runtime")
                    .join(filename);
                out.push(candidate);
                if probe.join("engine").join("Cargo.toml").exists() {
                    break;
                }
                if let Some(p) = probe.parent() {
                    probe = p.to_path_buf();
                } else {
                    break;
                }
            }
        }
    }
    out
}
