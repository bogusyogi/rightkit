//! Model files, manifest verification, and where things live.
//!
//! Models live in `<data>/models/qts/` for now (later the Orthic Labs shared
//! folder; set `RIGHTKIT_TTS_MODEL_DIR` to point elsewhere). Files are
//! manifest-verified by exact size always and by SHA-256 once per file
//! version (cached in `.verified.json`).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MODEL_ID: &str = "qwen3-tts-0.6b-q8_0";

pub struct ManifestFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// Hashes recorded by VoiceRight ADR-4 for the Hugging Face artifacts
/// `dsh0416/Qwen3-TTS-12Hz-0.6B-Base-QTS` (the vocoder hash is the HF
/// X-Linked-ETag; upstream's own SHA256SUMS line for it is stale).
pub const MANIFEST: [ManifestFile; 2] = [
    ManifestFile {
        name: "qwen3-tts-0.6b-q8_0.gguf",
        bytes: 1_325_190_016,
        sha256: "101b17e49b8042ffb07b189e4cbdc18797c879c2a2868bc72306c44ba2bd4ebf",
    },
    ManifestFile {
        name: "qwen3-tts-vocoder.onnx",
        bytes: 459_514_304,
        sha256: "b219a025f6d43a4c0f16764fd038ad159650863e7565b9f0b2f24cea5625077e",
    },
];

pub const HF_REPO: &str = "dsh0416/Qwen3-TTS-12Hz-0.6B-Base-QTS";
/// Total download size of the two model files.
pub const DOWNLOAD_BYTES: u64 = 1_325_190_016 + 459_514_304;

/// The data root could not be named: no override, no per-user base, or a relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRootError(pub String);

impl std::fmt::Display for DataRootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot resolve the RightKit data root: {}", self.0)
    }
}

impl std::error::Error for DataRootError {}

fn env_nonempty(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The RightKit data root, failing closed: `RIGHTKIT_DATA_ROOT` (legacy `GENRIGHT_DATA_ROOT`) or
/// the per-user base (`%LOCALAPPDATA%` / `$HOME`) plus `Orthic Labs/RightKit`. An unset or
/// relative base is an error; this never guesses a temp directory.
pub fn try_data_root() -> Result<PathBuf, DataRootError> {
    let root = if let Some(root) =
        env_nonempty("RIGHTKIT_DATA_ROOT").or_else(|| env_nonempty("GENRIGHT_DATA_ROOT"))
    {
        root
    } else {
        let var = if cfg!(windows) {
            "LOCALAPPDATA"
        } else {
            "HOME"
        };
        env_nonempty(var)
            .ok_or_else(|| DataRootError(format!("{var} is not set")))?
            .join("Orthic Labs")
            .join("RightKit")
    };
    if !root.is_absolute() {
        return Err(DataRootError(format!("{} is not absolute", root.display())));
    }
    Ok(root)
}

/// As [`try_data_root`], but falls back to the OS temp directory when no base is set, and accepts
/// a relative override. Kept for compatibility; prefer [`try_data_root`], which fails closed.
pub fn data_root() -> PathBuf {
    if let Some(root) = std::env::var_os("RIGHTKIT_DATA_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    if let Some(root) = std::env::var_os("GENRIGHT_DATA_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .unwrap_or_else(std::env::temp_dir)
    .join("Orthic Labs")
    .join("RightKit")
}

/// Fail-closed [`model_dir`]: `RIGHTKIT_TTS_MODEL_DIR` (legacy `GENRIGHT_QTS_MODEL_DIR`) or
/// `<data>/models/qts`, erroring when the data root cannot be named.
pub fn try_model_dir() -> Result<PathBuf, DataRootError> {
    match env_nonempty("RIGHTKIT_TTS_MODEL_DIR").or_else(|| env_nonempty("GENRIGHT_QTS_MODEL_DIR"))
    {
        Some(p) => Ok(p),
        None => Ok(try_data_root()?.join("models").join("qts")),
    }
}

/// Fail-closed [`voices_dir`].
pub fn try_voices_dir() -> Result<PathBuf, DataRootError> {
    Ok(try_data_root()?.join("voices").join("local"))
}

/// `RIGHTKIT_TTS_MODEL_DIR` (legacy `GENRIGHT_QTS_MODEL_DIR`) or `<data>/models/qts`. Falls back
/// like [`data_root`]; prefer [`try_model_dir`].
pub fn model_dir() -> PathBuf {
    match std::env::var_os("RIGHTKIT_TTS_MODEL_DIR")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("GENRIGHT_QTS_MODEL_DIR").filter(|v| !v.is_empty()))
    {
        Some(p) => PathBuf::from(p),
        None => data_root().join("models").join("qts"),
    }
}

/// Local voice records (x-vector prompts) live here.
pub fn voices_dir() -> PathBuf {
    data_root().join("voices").join("local")
}

/// Both files exist with the manifest's exact size (cheap; no hashing).
pub fn files_present(dir: &Path) -> bool {
    MANIFEST.iter().all(|f| {
        std::fs::metadata(dir.join(f.name))
            .map(|m| m.is_file() && m.len() == f.bytes)
            .unwrap_or(false)
    })
}

#[derive(Serialize, Deserialize, Default)]
struct VerifiedCache {
    files: Vec<VerifiedEntry>,
}
#[derive(Serialize, Deserialize, PartialEq)]
struct VerifiedEntry {
    name: String,
    bytes: u64,
    mtime_ns: u128,
}

fn mtime_ns(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    rightkit_fs::digest::sha256_file(path)
}

/// Verify the model folder against the manifest. Sizes are always checked; a
/// SHA-256 pass runs for any file whose (size, mtime) is not in the cache and
/// records it on success. `full = true` ignores the cache.
pub fn verify(dir: &Path, full: bool) -> Result<(), String> {
    let cache_path = dir.join(".verified.json");
    let cache: VerifiedCache = std::fs::read_to_string(&cache_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut out = VerifiedCache::default();
    for f in &MANIFEST {
        let p = dir.join(f.name);
        let meta =
            std::fs::metadata(&p).map_err(|_| format!("missing model file {}", p.display()))?;
        if meta.len() != f.bytes {
            return Err(format!(
                "{} has {} bytes, expected {}",
                f.name,
                meta.len(),
                f.bytes
            ));
        }
        let entry = VerifiedEntry {
            name: f.name.into(),
            bytes: f.bytes,
            mtime_ns: mtime_ns(&p),
        };
        if full || !cache.files.contains(&entry) {
            let got = sha256_file(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            if got != f.sha256 {
                return Err(format!("{} failed its SHA-256 check", f.name));
            }
        }
        out.files.push(entry);
    }
    if let Ok(s) = serde_json::to_string(&out) {
        let _ = std::fs::write(&cache_path, s);
    }
    Ok(())
}

pub fn runtime_lib_name() -> &'static str {
    rightkit_ort::runtime_filename()
}

/// Find ONNX Runtime (>= 1.24, `api-24`) through `rightkit_ort::discover_runtime`:
/// `ORT_DYLIB_PATH`, then `runtime/` next to the running exe, next to the exe,
/// `<model_dir>/../runtime`, `<data>/runtime/qts`.
pub fn find_onnxruntime() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = try_model_dir().ok().as_deref().and_then(Path::parent) {
        dirs.push(parent.join("runtime"));
    }
    if let Ok(root) = try_data_root() {
        dirs.push(root.join("runtime").join("qts"));
    }
    rightkit_ort::discover_runtime(dirs)
}

/// Backend the bundled worker binary supports: Metal on macOS, CPU elsewhere.
/// The staging script builds `--features vulkan` only when a Vulkan worker is
/// actually shipped; until then "vulkan" here would reject a working CPU build.
pub fn default_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "metal"
    } else if cfg!(feature = "vulkan") {
        "vulkan"
    } else {
        "cpu"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_wrong_size_files_are_rejected() {
        let d = std::env::temp_dir().join(format!("gqts-models-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(!files_present(&d));
        assert!(verify(&d, false).is_err());
        std::fs::write(d.join(MANIFEST[0].name), b"short").unwrap();
        std::fs::write(d.join(MANIFEST[1].name), b"short").unwrap();
        assert!(!files_present(&d));
        assert!(verify(&d, false).unwrap_err().contains("expected"));
        let _ = std::fs::remove_file(d.join(MANIFEST[0].name));
        let _ = std::fs::remove_file(d.join(MANIFEST[1].name));
    }

    #[test]
    fn sha256_of_known_bytes() {
        let p = std::env::temp_dir().join(format!("gqts-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(p);
    }
}
