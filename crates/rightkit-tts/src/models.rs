//! Model files, manifest verification, and where things live.
//!
//! Models live in `<data>/models/qts/` for now (later the Orthic Labs shared
//! folder; set `RIGHTKIT_TTS_MODEL_DIR` to point elsewhere). Files are
//! manifest-verified by exact size always and by SHA-256 once per file
//! version (cached in `.verified.json`). Callers that own their data root
//! pass it to the `*_in` functions; the no-argument forms resolve it from the
//! environment and the per-user base.
use crate::protocol::Mode;
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

/// One file of a 1.7B mode export, named as `rightkit-qts`'s exporter writes it (q8_0).
/// `bytes` and `sha256` stay `None` until an export is pinned; an unpinned file is not
/// verified here, and the worker's path resolution reports it as `model_missing`.
pub struct ModeManifestFile {
    pub name: &'static str,
    pub bytes: Option<u64>,
    pub sha256: Option<&'static str>,
}

/// The 1.7B export for one mode. Base has no entry: it uses [`MANIFEST`].
pub struct ModeManifest {
    pub mode: Mode,
    pub files: &'static [ModeManifestFile],
}

const CUSTOM_VOICE_FILES: [ModeManifestFile; 3] = [
    ModeManifestFile {
        name: "qwen3-tts-1.7b-customvoice-talker-q8_0.gguf",
        bytes: None,
        sha256: None,
    },
    ModeManifestFile {
        name: "qwen3-tts-1.7b-customvoice-code-predictor-q8_0.gguf",
        bytes: None,
        sha256: None,
    },
    ModeManifestFile {
        name: "qwen3-tts-1.7b-customvoice-tokenizer.gguf",
        bytes: None,
        sha256: None,
    },
];

const VOICE_DESIGN_FILES: [ModeManifestFile; 3] = [
    ModeManifestFile {
        name: "qwen3-tts-1.7b-voicedesign-talker-q8_0.gguf",
        bytes: None,
        sha256: None,
    },
    ModeManifestFile {
        name: "qwen3-tts-1.7b-voicedesign-code-predictor-q8_0.gguf",
        bytes: None,
        sha256: None,
    },
    ModeManifestFile {
        name: "qwen3-tts-1.7b-voicedesign-tokenizer.gguf",
        bytes: None,
        sha256: None,
    },
];

/// 1.7B mode exports. The shared vocoder stays in [`MANIFEST`]. No entry is pinned yet: the
/// exports are not in the workspace, so their sizes and hashes must come from a real export.
pub const MODE_MANIFESTS: [ModeManifest; 2] = [
    ModeManifest {
        mode: Mode::CustomVoice,
        files: &CUSTOM_VOICE_FILES,
    },
    ModeManifest {
        mode: Mode::VoiceDesign,
        files: &VOICE_DESIGN_FILES,
    },
];

/// The manifest entry for a 1.7B mode; `None` for Base.
pub fn mode_manifest(mode: Mode) -> Option<&'static ModeManifest> {
    MODE_MANIFESTS.iter().find(|m| m.mode == mode)
}

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

/// The data root: the caller's `data_root` when given (must be absolute), else the environment
/// and per-user base of [`try_data_root`].
pub fn data_root_in(data_root: Option<&Path>) -> Result<PathBuf, DataRootError> {
    match data_root {
        Some(root) if root.is_absolute() => Ok(root.to_path_buf()),
        Some(root) => Err(DataRootError(format!("{} is not absolute", root.display()))),
        None => try_data_root(),
    }
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

/// Model folder: `RIGHTKIT_TTS_MODEL_DIR` (legacy `GENRIGHT_QTS_MODEL_DIR`) when set, else
/// `<data_root>/models/qts` with the data root resolved by [`data_root_in`].
pub fn model_dir_in(data_root: Option<&Path>) -> Result<PathBuf, DataRootError> {
    match env_nonempty("RIGHTKIT_TTS_MODEL_DIR").or_else(|| env_nonempty("GENRIGHT_QTS_MODEL_DIR"))
    {
        Some(p) => Ok(p),
        None => Ok(data_root_in(data_root)?.join("models").join("qts")),
    }
}

/// Fail-closed [`model_dir`]: `RIGHTKIT_TTS_MODEL_DIR` (legacy `GENRIGHT_QTS_MODEL_DIR`) or
/// `<data>/models/qts`, erroring when the data root cannot be named.
pub fn try_model_dir() -> Result<PathBuf, DataRootError> {
    model_dir_in(None)
}

/// Voice library folder under the caller's data root (see [`data_root_in`]).
pub fn try_voices_dir_in(data_root: Option<&Path>) -> Result<PathBuf, DataRootError> {
    Ok(data_root_in(data_root)?.join("voices").join("local"))
}

/// Fail-closed [`voices_dir`].
pub fn try_voices_dir() -> Result<PathBuf, DataRootError> {
    try_voices_dir_in(None)
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
    base_files_present(dir, &dir.join(MANIFEST[1].name))
}

fn sized_file(path: &Path, bytes: u64) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() == bytes)
        .unwrap_or(false)
}

/// The vocoder passes when it is a regular file, and, at its default place in the model folder,
/// also has the manifest's size.
fn vocoder_ok(dir: &Path, vocoder: &Path) -> bool {
    let default = dir.join(MANIFEST[1].name);
    if vocoder == default {
        sized_file(vocoder, MANIFEST[1].bytes)
    } else {
        vocoder.is_file()
    }
}

/// The Base checkpoint in `dir` has the manifest's exact size and the vocoder at `vocoder` is
/// present; where the vocoder lives is the caller's choice, so only a vocoder at its default
/// place is size-pinned.
pub fn base_files_present(dir: &Path, vocoder: &Path) -> bool {
    sized_file(&dir.join(MANIFEST[0].name), MANIFEST[0].bytes) && vocoder_ok(dir, vocoder)
}

/// GGUF files a 1.7B mode needs in `dir`, named as `rightkit-qts` resolves them: a bundled
/// `qwen3-tts-1.7b-<mode>-<quant>.gguf` if one exists, else the per-component files
/// `-talker-`, `-code-predictor-` and `-tokenizer`. Quantizations are tried in qts order
/// (f16, q8_0, q6_k, q5_k, q4_k); a component with none present is reported under its f16 name,
/// so the result lists exactly what to supply. Empty for Base. The vocoder is not included.
pub fn mode_model_files(dir: &Path, mode: Mode) -> Vec<PathBuf> {
    const QUANTS: [&str; 5] = ["f16", "q8_0", "q6_k", "q5_k", "q4_k"];
    let stem = match mode {
        Mode::Base => return Vec::new(),
        Mode::CustomVoice => "qwen3-tts-1.7b-customvoice",
        Mode::VoiceDesign => "qwen3-tts-1.7b-voicedesign",
    };
    if let Some(bundled) = QUANTS
        .iter()
        .map(|q| dir.join(format!("{stem}-{q}.gguf")))
        .find(|p| p.is_file())
    {
        return vec![bundled];
    }
    let component = |name: &str| {
        let candidates: Vec<PathBuf> = QUANTS
            .iter()
            .map(|q| dir.join(format!("{stem}-{name}-{q}.gguf")))
            .collect();
        candidates
            .iter()
            .find(|p| p.is_file())
            .unwrap_or(&candidates[0])
            .clone()
    };
    vec![
        component("talker"),
        component("code-predictor"),
        dir.join(format!("{stem}-tokenizer.gguf")),
    ]
}

/// Pre-spawn presence check for one mode: the paths that are missing (or, for Base, not the
/// manifest size). Empty means the mode can start. Base needs its checkpoint in `dir` and the
/// vocoder; the 1.7B modes need [`mode_model_files`] and the vocoder.
pub fn missing_for_mode(dir: &Path, vocoder: &Path, mode: Mode) -> Vec<PathBuf> {
    if mode == Mode::Base {
        let main = dir.join(MANIFEST[0].name);
        let mut missing = Vec::new();
        if !sized_file(&main, MANIFEST[0].bytes) {
            missing.push(main);
        }
        if !vocoder_ok(dir, vocoder) {
            missing.push(vocoder.to_path_buf());
        }
        return missing;
    }
    mode_model_files(dir, mode)
        .into_iter()
        .chain(std::iter::once(vocoder.to_path_buf()))
        .filter(|p| !p.is_file())
        .collect()
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

/// One file to check: `bytes` and `sha256` are each optional; an entry with neither is skipped by
/// the callers that pass it.
struct Expect<'a> {
    name: &'a str,
    bytes: Option<u64>,
    sha256: Option<&'a str>,
}

/// Verify `expects` under `dir`. A file missing from `dir` is an error. Sizes are always checked
/// when pinned; a SHA-256 pass runs for any pinned file whose (size, mtime) is not in the cache,
/// and records it on success. `full = true` ignores the cache. Cache entries for other files are
/// kept. The cache file is written only when its content changes.
fn verify_files(dir: &Path, expects: &[Expect<'_>], full: bool) -> Result<(), String> {
    let cache_path = dir.join(".verified.json");
    let stored = std::fs::read_to_string(&cache_path).ok();
    let cache: VerifiedCache = stored
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let mut out = VerifiedCache::default();
    for f in expects {
        let p = dir.join(f.name);
        let meta =
            std::fs::metadata(&p).map_err(|_| format!("missing model file {}", p.display()))?;
        if let Some(bytes) = f.bytes {
            if meta.len() != bytes {
                return Err(format!(
                    "{} has {} bytes, expected {}",
                    f.name,
                    meta.len(),
                    bytes
                ));
            }
        }
        let Some(sha256) = f.sha256 else {
            continue;
        };
        let entry = VerifiedEntry {
            name: f.name.into(),
            bytes: meta.len(),
            mtime_ns: mtime_ns(&p),
        };
        if full || !cache.files.contains(&entry) {
            let got = sha256_file(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            if got != sha256 {
                return Err(format!("{} failed its SHA-256 check", f.name));
            }
        }
        out.files.push(entry);
    }
    out.files.extend(
        cache
            .files
            .into_iter()
            .filter(|old| !expects.iter().any(|f| f.name == old.name)),
    );
    // A check that learned nothing writes nothing: the cache file is rewritten only when its
    // content changes, so verifying an already-verified folder does not touch it.
    if let Ok(s) = serde_json::to_string(&out) {
        if stored.as_deref() != Some(s.as_str()) {
            let _ = std::fs::write(&cache_path, s);
        }
    }
    Ok(())
}

/// Verify the Base (0.6B) folder against [`MANIFEST`].
pub fn verify(dir: &Path, full: bool) -> Result<(), String> {
    let expects: Vec<Expect<'_>> = MANIFEST
        .iter()
        .map(|f| Expect {
            name: f.name,
            bytes: Some(f.bytes),
            sha256: Some(f.sha256),
        })
        .collect();
    verify_files(dir, &expects, full)
}

/// Verify the Base checkpoint in `dir` and the vocoder at `vocoder` against [`MANIFEST`]. At the
/// default vocoder place this is [`verify`]; a vocoder elsewhere is hashed in its own folder.
pub fn verify_base(dir: &Path, vocoder: &Path, full: bool) -> Result<(), String> {
    if vocoder == dir.join(MANIFEST[1].name) {
        return verify(dir, full);
    }
    verify_files(
        dir,
        &[Expect {
            name: MANIFEST[0].name,
            bytes: Some(MANIFEST[0].bytes),
            sha256: Some(MANIFEST[0].sha256),
        }],
        full,
    )?;
    let (Some(parent), Some(name)) = (
        vocoder.parent(),
        vocoder.file_name().and_then(|n| n.to_str()),
    ) else {
        return Err(format!(
            "vocoder path {} has no file name",
            vocoder.display()
        ));
    };
    verify_files(
        parent,
        &[Expect {
            name,
            bytes: Some(MANIFEST[1].bytes),
            sha256: Some(MANIFEST[1].sha256),
        }],
        full,
    )
}

/// Verify the pinned files of a 1.7B mode export. Unpinned entries are skipped: their presence is
/// checked by the worker through `rightkit-qts` path resolution, which also accepts other
/// quantizations. Base has no mode manifest and always passes.
pub fn verify_mode(dir: &Path, mode: Mode, full: bool) -> Result<(), String> {
    let Some(manifest) = mode_manifest(mode) else {
        return Ok(());
    };
    let expects: Vec<Expect<'_>> = manifest
        .files
        .iter()
        .filter(|f| f.bytes.is_some() || f.sha256.is_some())
        .map(|f| Expect {
            name: f.name,
            bytes: f.bytes,
            sha256: f.sha256,
        })
        .collect();
    verify_files(dir, &expects, full)
}

/// Bind the ONNX Runtime a worker process loads. Release builds never inherit `ORT_DYLIB_PATH`;
/// debug builds keep it as a developer override. An explicit runtime sets `ORT_DYLIB_PATH` and its
/// directory goes first on `PATH`, so the DLLs beside it load on Windows; that holds in every build
/// and whatever the environment held. Call single-threaded, before the engine loads.
pub fn bind_ort_runtime(explicit: Option<&Path>) {
    if !cfg!(debug_assertions) {
        std::env::remove_var("ORT_DYLIB_PATH");
    }
    if let Some(path) = explicit {
        std::env::set_var("ORT_DYLIB_PATH", path);
        rightkit_ort::prepend_runtime_dir_to_path(path);
    }
}

pub fn runtime_lib_name() -> &'static str {
    rightkit_ort::runtime_filename()
}

/// Developer override for ONNX Runtime: `ORT_DYLIB_PATH` is read only in debug builds, the same
/// gate `rightkit-ort` applies to its developer override. Release builds never consult it.
fn developer_env_runtime() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        env_nonempty("ORT_DYLIB_PATH")
    } else {
        None
    }
}

/// First regular, non-system ONNX Runtime among `developer` then `runtime/` and the exe directory
/// beside the running exe, then `<dir>/<runtime file>` for each absolute directory in `dirs`.
/// Reads no environment itself. Windows-ML copies under `System32` are skipped.
fn select_runtime(developer: Option<PathBuf>, dirs: Vec<PathBuf>) -> Option<PathBuf> {
    let name = rightkit_ort::runtime_filename();
    let mut candidates: Vec<PathBuf> = developer.into_iter().collect();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(exe_dir.join("runtime").join(name));
        candidates.push(exe_dir.join(name));
    }
    candidates.extend(
        dirs.into_iter()
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name)),
    );
    candidates
        .into_iter()
        .find(|path| path.is_file() && !rightkit_ort::is_system_runtime(path))
}

/// Find ONNX Runtime for a worker of `model_dir` under the caller's data root: the directory
/// beside the model folder's parent `runtime/`, then `<data>/runtime/qts`. Only a debug build
/// honours `ORT_DYLIB_PATH` (see [`developer_env_runtime`]).
pub fn find_onnxruntime_in(data_root: Option<&Path>, model_dir: Option<&Path>) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = model_dir.and_then(Path::parent) {
        dirs.push(parent.join("runtime"));
    }
    if let Ok(root) = data_root_in(data_root) {
        dirs.push(root.join("runtime").join("qts"));
    }
    select_runtime(developer_env_runtime(), dirs)
}

/// Find ONNX Runtime (>= 1.24, `api-24`) for the default model folder and data root. See
/// [`find_onnxruntime_in`].
pub fn find_onnxruntime() -> Option<PathBuf> {
    let model_dir = try_model_dir().ok();
    find_onnxruntime_in(None, model_dir.as_deref())
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

    #[test]
    fn mode_manifest_names_match_the_exporter_for_each_1_7b_mode() {
        assert!(mode_manifest(Mode::Base).is_none());
        for mode in [Mode::CustomVoice, Mode::VoiceDesign] {
            let manifest = mode_manifest(mode).expect("1.7B mode entry");
            let stem = format!("qwen3-tts-1.7b-{}", mode_stem(mode));
            assert_eq!(manifest.files.len(), 3);
            assert!(manifest.files.iter().all(|f| f.name.starts_with(&stem)));
            assert!(manifest.files.iter().all(|f| f.name.ends_with(".gguf")));
            // Unpinned entries stay unverified until a real export is measured.
            assert!(manifest
                .files
                .iter()
                .all(|f| f.bytes.is_none() && f.sha256.is_none()));
        }
    }

    fn mode_stem(mode: Mode) -> &'static str {
        match mode {
            Mode::CustomVoice => "customvoice",
            Mode::VoiceDesign => "voicedesign",
            Mode::Base => "base",
        }
    }

    #[test]
    fn caller_data_root_must_be_absolute_and_is_used_verbatim() {
        let root = std::env::temp_dir().join("gqts-caller-root");
        assert_eq!(data_root_in(Some(root.as_path())).unwrap(), root);
        assert!(data_root_in(Some(Path::new("relative/root")))
            .unwrap_err()
            .0
            .contains("not absolute"));
        assert_eq!(
            try_voices_dir_in(Some(root.as_path())).unwrap(),
            root.join("voices").join("local")
        );
    }

    #[test]
    fn select_runtime_ignores_the_environment_and_skips_system_copies() {
        let dir = std::env::temp_dir().join(format!("gqts-ort-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(rightkit_ort::runtime_filename());
        std::fs::write(&file, b"x").unwrap();
        // Explicit inputs only: no developer override, one installed directory.
        assert_eq!(
            select_runtime(None, vec![dir.clone()]).as_deref(),
            Some(file.as_path())
        );
        // A developer override that points at a missing file is not selected.
        assert_eq!(
            select_runtime(Some(dir.join("missing")), vec![]).as_deref(),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inherited_ort_dylib_path_is_honoured_only_in_debug_builds() {
        let dir = std::env::temp_dir().join(format!("gqts-ort-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let injected = dir.join("injected-runtime");
        std::fs::write(&injected, b"x").unwrap();
        std::env::set_var("ORT_DYLIB_PATH", &injected);
        // Only the default-data-root search runs here; the injected file is in no searched dir.
        let found = find_onnxruntime_in(None, None);
        std::env::remove_var("ORT_DYLIB_PATH");
        if cfg!(debug_assertions) {
            assert_eq!(found.as_deref(), Some(injected.as_path()));
        } else {
            assert_ne!(found.as_deref(), Some(injected.as_path()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
