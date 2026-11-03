//! Local Blender rendering through the independently installed RenderRight worker.
//!
//! The job runner owns the Blender process for the duration of one attempt.
//! On Windows, RightKit contains the owned child in a kill-on-close Job Object.
//! The runner lock records the durable owner lease; recovery replays stale attempts.

use super::{
    Durations, GenRequest, Kind, ModelInfo, Output, PollOutcome, Provider, ProviderError,
    SubmitOutcome,
};
use crate::host::ffmpeg::FfmpegBinary;
use rightkit_child_process::{OwnedCommand, RunOptions};
use serde_json::{json, Map, Value};
use std::fs;
#[cfg_attr(not(any(target_os = "macos", windows)), allow(unused_imports))]
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const ID: &str = "local-blender";
const MAX_DIMENSION: u64 = 4096;
const MAX_PIXELS: u64 = 16_777_216;
const MAX_FRAMES: u64 = 1200;
const MAX_SAMPLES: u64 = 512;
const RUN_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

static MODELS: [ModelInfo; 5] = [
    ModelInfo {
        provider: ID,
        model: "blender-hero",
        kind: Kind::Image,
        label: "Blender hero render (local, free)",
        est_usd: 0.0,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: ID,
        model: "blender-turntable",
        kind: Kind::Video,
        label: "Blender turntable (local, free)",
        est_usd: 0.0,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: ID,
        model: "blender-exploded",
        kind: Kind::Video,
        label: "Blender exploded view (local, free)",
        est_usd: 0.0,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: ID,
        model: "blender-film",
        kind: Kind::Video,
        label: "Blender product film (local, free)",
        est_usd: 0.0,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: ID,
        model: "blender-probe",
        kind: Kind::Image,
        label: "Blender render probe (local, free)",
        est_usd: 0.0,
        durations: Durations::NONE,
    },
];

#[derive(Clone, Debug)]
struct Config {
    blender: PathBuf,
    worker: PathBuf,
    assets: Option<PathBuf>,
    jobs: PathBuf,
    probe_cache: PathBuf,
}

/// Environment variable the RenderRight worker reads for the external assets folder.
pub const ASSETS_ENV: &str = "RIGHTKIT_BLENDER_ASSETS_DIR";
/// Folder name under the app data root that is used when the variable is unset.
const ASSETS_DEFAULT_DIR: &str = "blender-assets";

/// Resolve the external Blender assets folder.
///
/// Order: `RIGHTKIT_BLENDER_ASSETS_DIR` when non-empty (passed through verbatim, so a missing
/// folder fails in the worker with its own error naming the variable); otherwise
/// `<app data root>/blender-assets` when that directory exists; otherwise `None`.
/// No settings source exists for the other Blender paths, so none is added here.
pub fn resolve_assets_dir(env: Option<std::ffi::OsString>, data_root: &Path) -> Option<PathBuf> {
    if let Some(value) = env.filter(|v| !v.to_string_lossy().trim().is_empty()) {
        return Some(PathBuf::from(value));
    }
    let default = data_root.join(ASSETS_DEFAULT_DIR);
    default.is_dir().then_some(default)
}

/// The assets folder this process would hand to Blender, for `doctor` reporting.
pub fn resolved_assets_dir() -> Option<PathBuf> {
    resolve_assets_dir(std::env::var_os(ASSETS_ENV), &crate::host::app_data_root())
}

#[derive(Clone, Debug)]
struct WorkerInstall {
    root: PathBuf,
    entry: PathBuf,
    version: String,
    recipe_hash: String,
    recipe_manifest: Value,
}

pub struct LocalBlender {
    config: Config,
    probes: Mutex<std::collections::HashSet<String>>,
    active: std::sync::Arc<Mutex<std::collections::HashSet<String>>>,
    owner_token: String,
}

fn default_blender() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(root) = super::orthic_root() {
        let version_root = root.join("shared/tools/blender/5.2.2");
        let preferred = version_root.join("blender-5.2.2-windows-x64/blender.exe");
        if preferred.is_file() {
            return Some(preferred);
        }
        if let Ok(entries) = fs::read_dir(version_root) {
            let mut candidates: Vec<_> = entries
                .flatten()
                .map(|entry| entry.path().join("blender.exe"))
                .filter(|p| p.is_file())
                .collect();
            candidates.sort();
            if let Some(path) = candidates.into_iter().next() {
                return Some(path);
            }
        }
    }
    #[cfg(windows)]
    let candidates = [
        PathBuf::from(r"C:\Program Files\Blender Foundation\Blender 5.2\blender.exe"),
        PathBuf::from(r"C:\Program Files\Blender Foundation\Blender 4.5\blender.exe"),
    ];
    #[cfg(target_os = "macos")]
    let candidates = [PathBuf::from(
        "/Applications/Blender.app/Contents/MacOS/Blender",
    )];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates = [
        PathBuf::from("/usr/bin/blender"),
        PathBuf::from("/usr/local/bin/blender"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn default_worker() -> PathBuf {
    let root = super::orthic_root().unwrap_or_else(|| std::env::temp_dir().join("Orthic Labs"));
    let mut candidates = Vec::new();
    for tools_root in [root.join("shared/tools"), root.join("tools")] {
        let direct = tools_root.join("renderright");
        candidates.push(direct.clone());
        if let Ok(entries) = fs::read_dir(&direct) {
            candidates.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir()),
            );
        }
    }
    candidates
        .into_iter()
        .find(|path| path.join("worker-manifest.json").is_file())
        .unwrap_or_else(|| root.join("shared/tools/renderright"))
}

#[cfg(windows)]
fn ordinary_path(path: &Path) -> PathBuf {
    let display = path.as_os_str().to_string_lossy();
    if let Some(rest) = display.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = display.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

#[cfg(not(windows))]
fn ordinary_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

impl LocalBlender {
    pub fn new() -> Self {
        let data = crate::host::app_data_root();
        let blender = std::env::var_os("RIGHTKIT_BLENDER_PATH")
            .map(PathBuf::from)
            .or_else(default_blender)
            .unwrap_or_else(|| PathBuf::from("blender"));
        let worker = std::env::var_os("RIGHTKIT_BLENDER_WORKER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_worker);
        let assets = resolve_assets_dir(std::env::var_os(ASSETS_ENV), &data);
        Self {
            config: Config {
                blender,
                worker,
                assets,
                jobs: data.join("blender-jobs"),
                probe_cache: data.join("blender-probe-cache.json"),
            },
            probes: Mutex::new(std::collections::HashSet::new()),
            active: std::sync::Arc::new(Mutex::new(std::collections::HashSet::new())),
            owner_token: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn worker_install(&self) -> Result<WorkerInstall, ProviderError> {
        let root = self.config.worker.canonicalize().map_err(|e| {
            unavailable(format!(
                "RenderRight worker directory is unavailable at {}: {e}",
                self.config.worker.display()
            ))
        })?;
        let manifest_path = root.join("worker-manifest.json");
        let manifest: Value = read_json(&manifest_path)
            .map_err(|e| unavailable(format!("invalid worker manifest: {e}")))?;
        if manifest["schema"].as_u64() != Some(1) || manifest["name"] != "renderright" {
            return Err(unavailable("unsupported RenderRight worker manifest"));
        }
        let entry_name = manifest["entry"]
            .as_str()
            .filter(|v| *v == "render_job.py")
            .ok_or_else(|| unavailable("worker manifest has an unsupported entry"))?;
        let recipes_rel = manifest["recipes"]
            .as_str()
            .filter(|v| *v == "recipes/manifest.json")
            .ok_or_else(|| unavailable("worker manifest has an unsupported recipes path"))?;
        let entry = root.join(entry_name);
        let recipes = root.join(recipes_rel);
        if !entry.is_file() || !recipes.is_file() {
            return Err(unavailable(
                "RenderRight entry or recipe manifest is missing",
            ));
        }
        let recipe_doc: Value = read_json(&recipes)
            .map_err(|e| unavailable(format!("invalid recipe manifest: {e}")))?;
        let recipes_map = recipe_doc
            .get("recipes")
            .and_then(Value::as_object)
            .ok_or_else(|| unavailable("recipe manifest has no recipes object"))?;
        for name in ["hero", "turntable", "exploded", "film", "probe"] {
            if recipes_map.get(name).is_none() {
                return Err(unavailable(format!("recipe manifest is missing {name}")));
            }
        }
        let version = manifest["version"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| unavailable("worker manifest has no version"))?
            .to_owned();
        let recipe_hash = hash_file(&recipes)
            .map_err(|e| unavailable(format!("could not hash recipe manifest: {e}")))?;
        Ok(WorkerInstall {
            root,
            entry,
            version,
            recipe_hash,
            recipe_manifest: recipe_doc,
        })
    }

    fn blender_path(&self) -> Result<PathBuf, ProviderError> {
        let path = if self.config.blender.components().count() == 1 {
            std::env::var_os("PATH")
                .and_then(|p| {
                    std::env::split_paths(&p)
                        .map(|d| d.join(&self.config.blender))
                        .find(|p| p.is_file())
                })
                .unwrap_or_else(|| self.config.blender.clone())
        } else {
            self.config.blender.clone()
        };
        path.canonicalize().map_err(|e| {
            unavailable(format!(
                "Blender executable is unavailable at {}: {e}",
                path.display()
            ))
        })
    }

    /// The owner's ffmpeg handle; the only source. Development builds reach `RIGHTKIT_FFMPEG` / PATH through the
    /// owner's `dev-overrides` path inside it, release builds never do.
    fn ffmpeg_handle(&self) -> Result<crate::host::ffmpeg::FfmpegHandle, ProviderError> {
        crate::host::ffmpeg::resolve_ffmpeg()
            .map_err(|e| unavailable(format!("FFmpeg is required for Blender video models but no verified ffmpeg was found (bundled; RIGHTKIT_FFMPEG or PATH in development builds): {e}")))
    }

    fn checked(&self, req: &GenRequest) -> Result<(&'static str, Kind), ProviderError> {
        let (recipe, kind) = match req.model.as_str() {
            "blender-hero" => ("hero", Kind::Image),
            "blender-turntable" => ("turntable", Kind::Video),
            "blender-exploded" => ("exploded", Kind::Video),
            "blender-film" => ("film", Kind::Video),
            "blender-probe" => ("probe", Kind::Image),
            _ => return Err(invalid(format!("unknown Blender model {}", req.model))),
        };
        if req.provider != ID || req.kind != kind {
            return Err(invalid(
                "request provider or kind does not match Blender model",
            ));
        }
        if !req.params.is_null() && !req.params.is_object() {
            return Err(invalid("params must be an object"));
        }
        if req.params.get("source").is_some() && req.params.get("source_path").is_some() {
            return Err(invalid("supply only params.source"));
        }
        if req
            .params
            .get("source")
            .is_some_and(|v| v.as_str().is_none())
            || req
                .params
                .get("source_path")
                .is_some_and(|v| v.as_str().is_none())
        {
            return Err(invalid("params.source must be a path string"));
        }
        let source = req
            .params
            .get("source")
            .or_else(|| req.params.get("source_path"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if recipe == "probe" {
            if !source.is_empty() {
                return Err(invalid("probe does not accept a source"));
            }
        } else if source.is_empty()
            || !Path::new(source).is_absolute()
            || !source.to_ascii_lowercase().ends_with(".glb")
            || !Path::new(source).is_file()
        {
            return Err(invalid(
                "render requires params.source as an absolute readable .glb path",
            ));
        }
        let known = [
            "source",
            "source_path",
            "source_sha256",
            "recipe_sha256",
            "mechanism",
            "mechanism_json",
            "mechanism_sha256",
            "worker_version",
            "blender_version",
            "recipe_version",
            "recipe",
            "width",
            "height",
            "samples",
            "frame_count",
            "fps",
            "device",
            "transparent",
            "material_recipe",
            "env",
            "prompt",
            "shot_id",
            "camera",
            "shot_start_frame",
            "source_frame_count",
            "product_spec",
        ];
        if let Some(p) = req.params.as_object() {
            if let Some(k) = p.keys().find(|k| !known.contains(&k.as_str())) {
                return Err(invalid(format!("unknown Blender parameter {k}")));
            }
        }
        Ok((recipe, kind))
    }

    fn job_json(
        &self,
        req: &GenRequest,
        job_id: &str,
        recipe: &str,
        kind: Kind,
        worker: &WorkerInstall,
        dir: &Path,
    ) -> Result<Value, ProviderError> {
        let params = req.params.as_object().cloned().unwrap_or_default();
        let render = recipe != "probe";
        let mut normalized = Map::new();
        if render && params.contains_key("product_spec") {
            let raw_path = params["product_spec"]
                .as_str()
                .ok_or_else(|| invalid("params.product_spec must be an absolute JSON path"))?;
            let path = PathBuf::from(raw_path);
            if !path.is_absolute() || !path.is_file() {
                return Err(invalid(
                    "params.product_spec must be an absolute readable JSON path",
                ));
            }
            let path = path
                .canonicalize()
                .map_err(|e| invalid(format!("could not read product spec: {e}")))?;
            read_json(&path)
                .map_err(|e| invalid(format!("product spec must be valid JSON: {e}")))?;
            normalized.insert(
                "product_spec".into(),
                json!(ordinary_path(&path).to_string_lossy()),
            );
        } else if !render
            && ["product_spec", "material_recipe"]
                .iter()
                .any(|key| params.contains_key(*key))
        {
            return Err(invalid("blender-probe is source-free and does not accept product review or material parameters"));
        }
        let get_u = |key: &str, default: u64, min: u64, max: u64| -> Result<u64, ProviderError> {
            match params.get(key) {
                None => Ok(default),
                Some(v) => v
                    .as_u64()
                    .filter(|n| (min..=max).contains(n))
                    .ok_or_else(|| {
                        invalid(format!(
                            "params.{key} must be an integer from {min} through {max}"
                        ))
                    }),
            }
        };
        let defaults = &worker.recipe_manifest["recipes"][recipe]["defaults"];
        let default_u = |key: &str, fallback: u64| defaults[key].as_u64().unwrap_or(fallback);
        let (dw, dh, ds, df) = (
            default_u("width", 1280),
            default_u("height", 720),
            default_u("samples", 32),
            default_u("frame_count", 120),
        );
        let width = get_u("width", dw, 16, MAX_DIMENSION)?;
        let height = get_u("height", dh, 16, MAX_DIMENSION)?;
        if width * height > MAX_PIXELS {
            return Err(invalid("render pixel count exceeds 16777216"));
        }
        let samples = get_u("samples", ds, 1, MAX_SAMPLES)?;
        let frame_count = get_u("frame_count", df, 1, MAX_FRAMES)?;
        let fps = match params.get("fps") {
            None => defaults
                .get("fps")
                .cloned()
                .unwrap_or_else(|| json!({"num":30,"den":1})),
            Some(v)
                if v["num"].as_u64().is_some_and(|n| (1..=240000).contains(&n))
                    && v["den"].as_u64().is_some_and(|n| (1..=240000).contains(&n))
                    && v["num"]
                        .as_u64()
                        .zip(v["den"].as_u64())
                        .is_some_and(|(n, d)| n >= d && n <= d.saturating_mul(240)) =>
            {
                json!({"num":v["num"],"den":v["den"]})
            }
            Some(_) => {
                return Err(invalid(
                    "params.fps must contain positive integer num and den",
                ))
            }
        };
        let device = params
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("auto");
        if !matches!(device, "auto" | "cpu") {
            return Err(invalid("params.device must be auto or cpu"));
        }
        let transparent = params
            .get("transparent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if params.get("transparent").is_some() && params["transparent"].as_bool().is_none() {
            return Err(invalid("params.transparent must be boolean"));
        }
        normalized.insert("width".into(), json!(width));
        normalized.insert("height".into(), json!(height));
        normalized.insert("samples".into(), json!(samples));
        normalized.insert("frame_count".into(), json!(frame_count));
        normalized.insert("fps".into(), fps);
        normalized.insert("device".into(), json!(device));
        normalized.insert("transparent".into(), json!(transparent));
        if params
            .get("recipe")
            .is_some_and(|v| v.as_str() != Some(recipe))
        {
            return Err(invalid(
                "params.recipe must match the selected Blender model",
            ));
        }
        normalized.insert("recipe".into(), json!(recipe));
        if let Some(v) = params.get("material_recipe") {
            let valid = v.is_string()
                || v.as_object().is_some_and(|recipe| {
                    recipe.len() == 1
                        && recipe
                            .get("materials")
                            .and_then(Value::as_object)
                            .is_some_and(|materials| {
                                !materials.is_empty()
                                    && materials.iter().all(|(name, preset)| {
                                        !name.trim().is_empty()
                                            && preset.as_str().is_some_and(|s| !s.trim().is_empty())
                                    })
                            })
                });
            if !valid {
                return Err(invalid("params.material_recipe must be a preset string or {materials:{material_name:preset}}"));
            }
            normalized.insert("material_recipe".into(), v.clone());
        }
        if let Some(v) = params.get("env") {
            if !v.is_string() {
                return Err(invalid("params.env must be a string"));
            }
            normalized.insert("env".into(), v.clone());
        }
        let prompt = match params.get("prompt") {
            Some(v) => v
                .as_str()
                .ok_or_else(|| invalid("params.prompt must be a string"))?,
            None => req.prompt.as_str(),
        };
        if prompt.chars().count() > 4000 {
            return Err(invalid("params.prompt must be at most 4000 characters"));
        }
        normalized.insert("prompt".into(), json!(prompt));
        if let Some(v) = params.get("shot_id") {
            let shot_id = v
                .as_str()
                .filter(|s| matches!(*s, "establish" | "top_detail" | "profile" | "hero"))
                .ok_or_else(|| {
                    invalid("params.shot_id must name an installed Blender film shot")
                })?;
            if recipe != "film" {
                return Err(invalid("params.shot_id is supported only by blender-film"));
            }
            normalized.insert("shot_id".into(), json!(shot_id));
        }
        let shot_start = params.get("shot_start_frame");
        let source_count = params.get("source_frame_count");
        if shot_start.is_some() || source_count.is_some() {
            if recipe != "film" || params.get("shot_id").is_none() {
                return Err(invalid(
                    "shot source range requires blender-film params.shot_id",
                ));
            }
            let start = shot_start
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("params.shot_start_frame must be a nonnegative integer"))?;
            let count = source_count
                .and_then(Value::as_u64)
                .filter(|n| (1..=MAX_FRAMES).contains(n))
                .ok_or_else(|| invalid("params.source_frame_count must be from 1 through 1200"))?;
            if start.saturating_add(frame_count) > count {
                return Err(invalid(
                    "shot_start_frame plus frame_count must fit source_frame_count",
                ));
            }
            normalized.insert("shot_start_frame".into(), json!(start));
            normalized.insert("source_frame_count".into(), json!(count));
        }
        if let Some(v) = params.get("camera") {
            let camera = v
                .as_object()
                .ok_or_else(|| invalid("params.camera must be an object"))?;
            let fields = ["lens_mm", "azimuth_deg", "elevation_deg", "distance_scale"];
            if camera.keys().any(|key| !fields.contains(&key.as_str())) {
                return Err(invalid("params.camera supports lens_mm, azimuth_deg, elevation_deg, and distance_scale only"));
            }
            let mut normalized_camera = Map::new();
            for name in fields {
                if let Some(value) = camera.get(name) {
                    let (min, max) = match name {
                        "lens_mm" => (15.0, 200.0),
                        "azimuth_deg" => (-360.0, 360.0),
                        "elevation_deg" => (-89.0, 89.0),
                        _ => (0.25, 4.0),
                    };
                    let number = value
                        .as_f64()
                        .filter(|n| *n >= min && *n <= max)
                        .ok_or_else(|| {
                            invalid(format!(
                                "params.camera.{name} must be from {min} through {max}"
                            ))
                        })?;
                    normalized_camera.insert(name.into(), json!(number));
                }
            }
            normalized.insert("camera".into(), Value::Object(normalized_camera));
        }
        let mechanism = params
            .get("mechanism_json")
            .or_else(|| params.get("mechanism"));
        if params.get("mechanism_json").is_some() && params.get("mechanism").is_some() {
            return Err(invalid("supply only params.mechanism_json"));
        }
        if let Some(mech) = mechanism.and_then(Value::as_str) {
            let path = PathBuf::from(mech);
            if !path.is_absolute() || !path.is_file() {
                return Err(invalid(
                    "params.mechanism_json must be an absolute readable JSON file",
                ));
            }
            let path = path.canonicalize().map_err(|e| invalid(e.to_string()))?;
            read_json(&path).map_err(|e| invalid(format!("mechanism JSON is invalid: {e}")))?;
            let actual = hash_file(&path)
                .map_err(|e| invalid(format!("could not hash mechanism JSON: {e}")))?;
            check_hash_param(&params, "mechanism_sha256", &actual)?;
            normalized.insert(
                "mechanism_json".into(),
                json!(ordinary_path(&path).to_string_lossy()),
            );
            normalized.insert("mechanism_sha256".into(), json!(actual));
        } else if mechanism.is_some() {
            return Err(invalid("params.mechanism_json must be a path string"));
        }
        if params.get("mechanism_sha256").is_some() && mechanism.is_none() {
            return Err(invalid(
                "params.mechanism_sha256 requires params.mechanism_json",
            ));
        }
        if recipe == "exploded" && mechanism.is_none() {
            return Err(invalid("blender-exploded requires params.mechanism_json"));
        }
        let source = params
            .get("source")
            .or_else(|| params.get("source_path"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let source = source
            .map(|p| {
                p.canonicalize()
                    .map(|p| ordinary_path(&p).to_string_lossy().into_owned())
                    .map_err(|e| invalid(format!("source path: {e}")))
            })
            .transpose()?;
        let source_hash = source
            .as_deref()
            .map(|p| {
                hash_file(Path::new(p)).map_err(|e| invalid(format!("could not hash source: {e}")))
            })
            .transpose()?;
        if let Some(hash) = &source_hash {
            check_hash_param(&params, "source_sha256", hash)?;
            normalized.insert("source_sha256".into(), json!(hash));
        } else if params.get("source_sha256").is_some() {
            return Err(invalid("params.source_sha256 requires a source"));
        }
        let recipe_hash = worker.recipe_hash.as_str();
        check_hash_param(&params, "recipe_sha256", recipe_hash)?;
        normalized.insert("recipe_sha256".into(), json!(recipe_hash));
        for (key, actual) in [
            ("worker_version", worker.version.as_str()),
            (
                "recipe_version",
                worker.recipe_manifest["recipes"][recipe]["version"]
                    .as_str()
                    .unwrap_or(""),
            ),
        ] {
            if let Some(requested) = params.get(key) {
                if requested.as_str() != Some(actual) {
                    return Err(invalid(format!(
                        "params.{key} is incompatible with installed worker"
                    )));
                }
            }
            normalized.insert(key.into(), json!(actual));
        }
        if params
            .get("blender_version")
            .is_some_and(|v| !v.as_str().is_some_and(|s| !s.trim().is_empty()))
        {
            return Err(invalid("params.blender_version must be a nonempty string"));
        }
        let installed_blender = read_json(&self.config.probe_cache)
            .ok()
            .and_then(|v| v["blender_version"].as_str().map(str::to_owned))
            .ok_or_else(|| unavailable("Blender version has not been probed"))?;
        if params
            .get("blender_version")
            .and_then(Value::as_str)
            .is_some_and(|v| v.trim() != installed_blender.trim())
        {
            return Err(invalid(
                "params.blender_version is incompatible with installed Blender",
            ));
        }
        normalized.insert("blender_version".into(), json!(installed_blender.trim()));
        Ok(
            json!({"schema":1,"job_id":job_id,"kind":if kind == Kind::Image {"image"} else {"video"},"source":source,"recipe":recipe,"output_dir":ordinary_path(dir).to_string_lossy(),"params":normalized}),
        )
    }

    fn run(
        &self,
        req: &GenRequest,
        job_id: &str,
        attempt: i64,
    ) -> Result<SubmitOutcome, ProviderError> {
        if job_id.is_empty()
            || job_id.len() > 128
            || !job_id.as_bytes()[0].is_ascii_alphanumeric()
            || !job_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(invalid("job id is not safe for worker paths"));
        }
        let (recipe, kind) = self.checked(req)?;
        if attempt < 1 {
            return Err(invalid("attempt must be positive"));
        }
        let blender = self.blender_path()?;
        let worker = self.worker_install()?;
        let dir = self.config.jobs.join(job_id);
        fs::create_dir_all(&dir)
            .map_err(|e| unavailable(format!("could not create Blender job directory: {e}")))?;
        let job_file = dir.join("job.json");
        let state_file = dir.join("provider-state.json");
        let provider_id = format!("{job_id}:attempt:{attempt}");
        if self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&provider_id)
        {
            return Ok(SubmitOutcome::Accepted {
                provider_job_id: provider_id,
            });
        }
        if let Ok(state) = read_json(&state_file) {
            if state["attempt"].as_i64() == Some(attempt)
                && matches!(
                    state["state"].as_str(),
                    Some(
                        "exited"
                            | "failed"
                            | "cancelled"
                            | "timeout"
                            | "observer_error"
                            | "interrupted"
                    )
                )
            {
                // Recover the narrow crash window after the process exit receipt
                // committed but before the engine persisted provider_job_id.
                return Ok(SubmitOutcome::Accepted {
                    provider_job_id: provider_id,
                });
            }
            if state["state"] == "running" {
                // The exclusive engine job lock is held by this runner. A
                // persisted owner from an earlier lock holder is interrupted.
                let interrupted = json!({"schema":1,"job_id":job_id,"attempt":attempt,"state":"interrupted","message":"previous job runner ended before recording Blender exit"});
                atomic_json(&state_file, &interrupted).map_err(|e| {
                    unavailable(format!("could not record interrupted Blender owner: {e}"))
                })?;
                return Ok(SubmitOutcome::Accepted {
                    provider_job_id: provider_id,
                });
            }
        }
        if kind == Kind::Video {
            let _ = self.ffmpeg_handle()?;
        }
        self.test_key("")?;
        let job = self.job_json(req, job_id, recipe, kind, &worker, &dir)?;
        let job_bytes = serde_json::to_vec_pretty(&job).map_err(|e| unavailable(e.to_string()))?;
        let input_hash = sha256_bytes(&job_bytes);
        rightkit_fs::atomic_write(&job_file, &job_bytes)
            .map_err(|e| unavailable(format!("could not write Blender job: {e}")))?;
        // A prior attempt's files remain useful checkpoints, but its manifest
        // cannot certify this attempt's process exit.
        let _ = fs::remove_file(dir.join("result-manifest.json"));
        for stale in ["encode-request.json", "encode-result.json"] {
            let _ = fs::remove_file(dir.join(stale));
        }
        let owner = json!({"schema":1,"job_id":job_id,"attempt":attempt,"state":"running","owner_token":self.owner_token,"owner_pid":std::process::id(),"started_unix_ms":unix_ms(),"input_sha256":input_hash,"blender":blender,"worker_version":worker.version});
        atomic_json(&state_file, &owner)
            .map_err(|e| unavailable(format!("could not persist Blender owner state: {e}")))?;
        // Both streams go through the owner's scrubbing, rotating text log (`rightkit_logs::RotatingTextWriter`).
        let stderr_log = std::sync::Arc::new(
            rightkit_logs::RotatingTextWriter::new(
                dir.join(format!("blender-attempt-{attempt}.stderr.log")),
                4 << 20,
            )
            .with_generations(2),
        );
        let stdout_log = std::sync::Arc::new(
            rightkit_logs::RotatingTextWriter::new(
                dir.join(format!("blender-attempt-{attempt}.stdout.log")),
                4 << 20,
            )
            .with_generations(2),
        );
        let device = job["params"]["device"].as_str().unwrap_or("auto");
        let mut command = OwnedCommand::new(ordinary_path(&blender));
        command
            .command_mut()
            .args(worker_args(
                &ordinary_path(&worker.entry),
                &ordinary_path(&job_file),
                device,
                cfg!(target_os = "macos"),
            ))
            .current_dir(ordinary_path(&worker.root))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The worker never launches ffmpeg: it asks the host (`encode-request.json`) and the monitor runs the owner handle.
        let encoder = if kind == Kind::Video {
            Some(ordinary_path(self.ffmpeg_handle()?.ffmpeg_path()))
        } else {
            None
        };
        if let Some(assets) = &self.config.assets {
            command.command_mut().env(ASSETS_ENV, ordinary_path(assets));
        }
        let mut child = command.spawn().map_err(|e| { let _ = atomic_json(&state_file, &json!({"schema":1,"job_id":job_id,"attempt":attempt,"state":"spawn_failed","message":e.to_string()})); unavailable(format!("could not launch Blender at {}: {e}", blender.display())) })?;
        let owner = json!({"schema":1,"job_id":job_id,"attempt":attempt,"state":"running","owner_token":self.owner_token,"owner_pid":std::process::id(),"child_pid":child.id(),"child_created_unix_ms":child.creation_time().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis()),"started_unix_ms":unix_ms(),"lease_until_unix_ms":unix_ms()+30_000,"input_sha256":input_hash,"blender":blender,"worker_version":worker.version});
        atomic_json(&state_file, &owner)
            .map_err(|e| unavailable(format!("could not persist Blender child identity: {e}")))?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| unavailable("Blender stdout pipe unavailable"))?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| unavailable("Blender stderr pipe unavailable"))?;
        // `capture_stderr` copies stderr line by line, scrubbed, into the rotating log; the thread ends when Blender closes it.
        let stderr_capture = rightkit_logs::capture_stderr(stderr, stderr_log.clone(), "")
            .map_err(|e| unavailable(format!("could not capture Blender stderr: {e}")))?;
        let (event_tx, event_rx) = std::sync::mpsc::channel::<Value>();
        // Owner drain (`StderrTail::capture` reads any stream): each stdout line goes to the private attempt log
        // and, when it is a worker `event` object, to the monitor. The drain keeps reading after a failed write.
        let log = stdout_log;
        let reader = rightkit_child_process::StderrTail::capture(
            stdout,
            64 * 1024,
            |line| Some(line.to_string()),
            move |line| {
                let _ = log.write_line(&rightkit_logs::scrub_text(line));
                if let Ok(value) = serde_json::from_str::<Value>(line) {
                    if value.get("event").and_then(Value::as_str).is_some() {
                        let _ = event_tx.send(value);
                    }
                }
            },
        );
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider_id.clone());
        let active = self.active.clone();
        let state_file_monitor = state_file.clone();
        let job_id_owned = job_id.to_owned();
        let owner_token = self.owner_token.clone();
        std::thread::spawn(move || {
            monitor_blender(
                child,
                reader,
                event_rx,
                active,
                provider_id,
                state_file_monitor,
                job_id_owned,
                attempt,
                input_hash,
                owner_token,
                dir.join(format!("blender-attempt-{attempt}.stderr.log")),
                stderr_capture,
                dir,
                encoder,
            )
        });
        Ok(SubmitOutcome::Accepted {
            provider_job_id: format!("{job_id}:attempt:{attempt}"),
        })
    }
}

impl Default for LocalBlender {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for LocalBlender {
    fn id(&self) -> &'static str {
        ID
    }
    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }
    /// `checked`: every recipe but the probe needs `params.source`, an absolute readable `.glb`; no param named
    /// aspect, resolution, duration or seed is known (`known` refuses it).
    fn capabilities(&self, model: &str) -> crate::capabilities::Capabilities {
        if model == "blender-probe" {
            crate::capabilities::Capabilities::default().mode("none")
        } else {
            crate::capabilities::Capabilities::default()
                .mode("model_file")
                .input("model", crate::capabilities::io("source", "path", true))
        }
    }
    fn needs_key(&self) -> bool {
        false
    }
    fn submit_is_idempotent(&self) -> bool {
        true
    }
    fn runtime_is_external(&self) -> bool {
        true
    }
    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.checked(req)?;
        Ok(0.0)
    }
    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        Some(
            self.blender_path()
                .and_then(|_| self.worker_install().map(|_| ())),
        )
    }
    fn test_key(&self, _key: &str) -> Result<(), ProviderError> {
        let blender = self.blender_path()?;
        let worker = self.worker_install()?;
        let identity = format!(
            "{}:{}:{}:{}:{}",
            blender.display(),
            file_identity(&blender),
            worker.version,
            file_identity(&worker.entry),
            worker.recipe_hash
        );
        if self
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&identity)
            || read_json(&self.config.probe_cache)
                .ok()
                .and_then(|v| v["identity"].as_str().map(str::to_owned))
                .as_deref()
                == Some(&identity)
        {
            return Ok(());
        }
        let probe_logs = self
            .config
            .probe_cache
            .parent()
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(probe_logs).map_err(|e| {
            unavailable(format!("could not create Blender probe log directory: {e}"))
        })?;
        let blender_version = probe(&blender, &["--version"], probe_logs, "blender-version")?;
        let worker_output = probe(
            &blender,
            &[
                "-b",
                "--factory-startup",
                "--python-exit-code",
                "1",
                "--python",
                worker.entry.to_str().unwrap_or("render_job.py"),
                "--",
                "--version",
            ],
            probe_logs,
            "worker-version",
        )?;
        let blender_version = normalized_blender_version(&blender_version)
            .ok_or_else(|| unavailable("Blender version probe returned unexpected output"))?;
        if !worker_output.contains(&worker.version) {
            return Err(unavailable(
                "RenderRight version probe returned unexpected output",
            ));
        }
        if let Some(parent) = self.config.probe_cache.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| unavailable(format!("could not create probe cache directory: {e}")))?;
        }
        atomic_json(&self.config.probe_cache, &json!({"identity":identity,"blender_version":blender_version,"worker_version":worker.version,"recipe_sha256":worker.recipe_hash,"verified_unix_ms":unix_ms()})).map_err(|e| unavailable(format!("could not cache Blender version probe: {e}")))?;
        self.probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(identity);
        Ok(())
    }
    fn submit(&self, req: &GenRequest, _key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.run(req, &format!("manual-{}", uuid::Uuid::new_v4()), 1)
    }
    fn submit_job(
        &self,
        req: &GenRequest,
        _key: &str,
        job_id: &str,
        attempt: i64,
        _idempotency_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        self.run(req, job_id, attempt)
    }
    fn poll(
        &self,
        _req: &GenRequest,
        provider_id: &str,
        _key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        let (job_id, attempt) = parse_provider_id(provider_id)
            .ok_or_else(|| invalid("invalid local Blender provider job id"))?;
        let dir = self.config.jobs.join(&job_id);
        let state: Value = match read_json(&dir.join("provider-state.json")) {
            Ok(state) => state,
            Err(_) => {
                return Ok(PollOutcome::Failed {
                    message: "Blender owner receipt is missing; replaying verified checkpoints"
                        .into(),
                })
            }
        };
        if state["job_id"].as_str() != Some(job_id.as_str())
            || state["attempt"].as_i64() != Some(attempt)
        {
            return Ok(PollOutcome::Failed {
                message: "Blender owner receipt belongs to another attempt".into(),
            });
        }
        match state["state"].as_str().unwrap_or("") {
            "running" => {
                let lease = state["lease_until_unix_ms"].as_u64().unwrap_or(0) as u128;
                if state["owner_pid"].as_u64() == Some(std::process::id() as u64)
                    && state["owner_token"].as_str() == Some(self.owner_token.as_str())
                    && lease >= unix_ms()
                {
                    Ok(PollOutcome::Running)
                } else {
                    Ok(PollOutcome::Failed { message: "Blender job runner lost its owned process; replaying verified checkpoints".into() })
                }
            }
            "interrupted" => Ok(PollOutcome::Failed {
                message: state["message"]
                    .as_str()
                    .unwrap_or("Blender owner was interrupted")
                    .to_owned(),
            }),
            "cancelled" => Err(ProviderError::new(
                "cancelled",
                "Blender render was cancelled",
                false,
            )),
            "timeout" => Ok(PollOutcome::Failed {
                message: "Blender render exceeded its two-hour limit".into(),
            }),
            "failed" | "observer_error" => Ok(PollOutcome::Failed {
                message: state["message"]
                    .as_str()
                    .unwrap_or("Blender process failed")
                    .to_owned(),
            }),
            "exited"
                if state["success"] == Value::Bool(true)
                    && state["exit_code"].as_i64() == Some(0) =>
            {
                let job = read_json(&dir.join("job.json")).map_err(|e| {
                    unavailable(format!("could not read submitted Blender job: {e}"))
                })?;
                let input_hash = state["input_sha256"]
                    .as_str()
                    .ok_or_else(|| unavailable("Blender exit receipt has no input identity"))?;
                let worker = self.worker_install()?;
                match verify_manifest(&dir, &job_id, &job, input_hash, &worker)
                    .and_then(|manifest| manifest_outputs(&dir, &manifest))
                {
                    Ok(outputs) => Ok(PollOutcome::Completed { outputs }),
                    Err(error) if !error.retryable => Err(error),
                    Err(error) => Ok(PollOutcome::Failed {
                        message: error.message,
                    }),
                }
            }
            "exited" => Ok(PollOutcome::Failed {
                message: "Blender exit receipt did not prove exit code zero".into(),
            }),
            _ => Ok(PollOutcome::Failed {
                message: "Blender owner receipt has an unknown state".into(),
            }),
        }
    }
    fn cancel_job(
        &self,
        _req: &GenRequest,
        _id: Option<&str>,
        job_id: &str,
    ) -> Result<(), ProviderError> {
        if job_id.is_empty()
            || job_id.len() > 128
            || !job_id.as_bytes()[0].is_ascii_alphanumeric()
            || !job_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(invalid("job id is not safe for cancellation"));
        }
        let dir = self.config.jobs.join(job_id);
        if dir.exists() {
            atomic_json(
                &dir.join("cancel.json"),
                &json!({"job_id":job_id,"requested_unix_ms":unix_ms()}),
            )
            .map_err(|e| {
                unavailable(format!(
                    "could not persist Blender cancellation request: {e}"
                ))
            })?;
        }
        Ok(())
    }
    fn progress_message(&self, _req: &GenRequest, id: &str) -> Option<String> {
        let (job_id, _) = parse_provider_id(id)?;
        let dir = self.config.jobs.join(&job_id);
        if let Ok(progress) = read_json(&dir.join("render-progress.json")) {
            if progress["job_id"].as_str() == Some(job_id.as_str()) {
                if let Some(message) = progress["message"]
                    .as_str()
                    .or_else(|| progress["state"].as_str())
                {
                    return Some(message.to_owned());
                }
            }
        }
        read_json(&dir.join("provider-state.json"))
            .ok()?
            .get("last_event")?
            .get("message")?
            .as_str()
            .map(str::to_owned)
    }
    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        let _ = output;
        Err(invalid(
            "local Blender path outputs are adopted directly by the library",
        ))
    }
}

fn monitor_blender(
    mut child: rightkit_child_process::OwnedChild,
    mut reader: rightkit_child_process::StderrTail,
    event_rx: std::sync::mpsc::Receiver<Value>,
    active: std::sync::Arc<Mutex<std::collections::HashSet<String>>>,
    provider_id: String,
    state_path: PathBuf,
    job_id: String,
    attempt: i64,
    input_hash: String,
    owner_token: String,
    stderr_log: PathBuf,
    stderr_capture: rightkit_logs::StderrCapture,
    job_dir: PathBuf,
    encoder: Option<PathBuf>,
) {
    let started = Instant::now();
    // Lease renewal runs on its own thread, independent of everything this loop can block on (the job-store
    // open inside `cancelled`, an encode step, `wait_timeout`, a slow disk): the loop only publishes the latest
    // worker event, and the thread renews the 30 s lease every 10 s until the loop ends.
    let lease_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shared_event: std::sync::Arc<Mutex<Option<Value>>> = std::sync::Arc::new(Mutex::new(None));
    let lease_thread = {
        let (stop, event) = (lease_stop.clone(), shared_event.clone());
        let (state_path, job_id, input_hash, owner_token) = (
            state_path.clone(),
            job_id.clone(),
            input_hash.clone(),
            owner_token.clone(),
        );
        let child_pid = child.id();
        let child_created = child
            .creation_time()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis());
        std::thread::spawn(move || {
            let mut last = Instant::now();
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(100));
                if last.elapsed() < Duration::from_secs(10) {
                    continue;
                }
                let mut state = json!({"schema":1,"job_id":job_id,"attempt":attempt,"state":"running","owner_token":owner_token,"owner_pid":std::process::id(),"child_pid":child_pid,"child_created_unix_ms":child_created,"started_unix_ms":unix_ms().saturating_sub(started.elapsed().as_millis()),"lease_until_unix_ms":unix_ms()+30_000,"input_sha256":input_hash});
                if let Some(event) = event.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    state["last_event"] = event.clone();
                }
                if !stop.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = atomic_json(&state_path, &state);
                }
                last = Instant::now();
            }
        })
    };
    let mut last_event: Option<Value> = None;
    let mut encode: Option<EncodeRun> = None;
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(unused_mut))]
    let mut result: Option<(String, Option<i32>, bool, Option<String>)>;
    loop {
        for event in event_rx.try_iter() {
            last_event = Some(event);
        }
        if let Some(event) = &last_event {
            *shared_event.lock().unwrap_or_else(|e| e.into_inner()) = Some(event.clone());
        }
        if let Some(ffmpeg) = &encoder {
            encode_tick(&mut encode, &job_dir, ffmpeg, &job_id);
        }
        if cancelled(&job_id) {
            let _ = child.terminate_tree_bounded(Duration::from_secs(10));
            result = Some((
                "cancelled".into(),
                None,
                false,
                Some("Blender render was cancelled".into()),
            ));
            break;
        }
        if started.elapsed() >= RUN_TIMEOUT {
            let _ = child.terminate_tree_bounded(Duration::from_secs(10));
            result = Some((
                "timeout".into(),
                None,
                false,
                Some("Blender render exceeded its two-hour limit".into()),
            ));
            break;
        }
        // Owner wait: blocks up to one poll interval and returns the moment the leader exits.
        match child.wait_timeout(POLL_INTERVAL) {
            Ok(Some(status)) => {
                // Blender has exited, so its stderr pipe is closed: wait for the capture thread to write the last lines.
                stderr_capture.join();
                let stderr = if status.success() {
                    None
                } else {
                    read_log_tail(&stderr_log)
                };
                result = Some((
                    if status.success() { "exited" } else { "failed" }.into(),
                    status.code(),
                    status.success(),
                    (!status.success()).then(|| {
                        format!(
                            "Blender exited with {status}{}",
                            stderr.map(|s| format!(": {s}")).unwrap_or_default()
                        )
                    }),
                ));
                break;
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.terminate_tree_bounded(Duration::from_secs(10));
                result = Some((
                    "observer_error".into(),
                    None,
                    false,
                    Some(format!("could not observe Blender process: {e}")),
                ));
                break;
            }
        }
    }
    lease_stop.store(true, std::sync::atomic::Ordering::Release);
    let _ = lease_thread.join();
    // Blender is gone (exit, cancel, timeout): an encode still running has no worker left to read its answer.
    if let Some(mut run) = encode.take() {
        run.abort();
        write_encode_result(&job_dir, Err("cancelled".into()));
    }
    let _ = reader.finish_within(Duration::from_secs(2));
    let (state, exit_code, success, message) = result.unwrap_or_else(|| {
        (
            "observer_error".into(),
            None,
            false,
            Some("Blender monitor ended without an exit result".into()),
        )
    });
    let mut receipt = json!({"schema":1,"job_id":job_id,"attempt":attempt,"state":state,"owner_token":owner_token,"owner_pid":std::process::id(),"exit_code":exit_code,"success":success,"finished_unix_ms":unix_ms(),"input_sha256":input_hash});
    if let Some(message) = message {
        receipt["message"] = json!(message);
    }
    if let Some(event) = last_event {
        receipt["last_event"] = event;
    }
    let _ = atomic_json(&state_path, &receipt);
    active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&provider_id);
}

/// One owner-spawned ffmpeg run answering the worker's `encode-request.json`. It never blocks the monitor: the
/// monitor calls [`encode_tick`] once per poll, so cancel, lease renewal and Blender's own exit keep being observed
/// while the encode runs.
struct EncodeRun {
    child: rightkit_child_process::OwnedChild,
    tail: Option<rightkit_child_process::StderrTail>,
    started: Instant,
}

impl EncodeRun {
    /// `None`: the worker has asked for nothing (or the question was answered). `Some(Err)`: the request is invalid or
    /// ffmpeg did not start. `Some(Ok)`: running.
    fn start(job_dir: &Path, ffmpeg: &Path) -> Option<Result<EncodeRun, String>> {
        let request = read_json(&job_dir.join("encode-request.json")).ok()?;
        if job_dir.join("encode-result.json").exists() {
            return None;
        }
        Some((|| {
            let (num, den) = (
                request["fps"]["num"].as_u64().filter(|n| *n > 0),
                request["fps"]["den"].as_u64().filter(|n| *n > 0),
            );
            let (Some(num), Some(den)) = (num, den) else {
                return Err("encode request has no valid fps".to_string());
            };
            let frames = request["frame_count"]
                .as_u64()
                .filter(|n| (1..=1_000_000).contains(n))
                .ok_or("encode request has no valid frame_count")?;
            let transparent = request["transparent"].as_bool().unwrap_or(false);
            let ext = if transparent { "mov" } else { "mp4" };
            let mut args: Vec<std::ffi::OsString> =
                ["-hide_banner", "-loglevel", "error", "-y", "-framerate"]
                    .iter()
                    .map(Into::into)
                    .collect();
            args.push(format!("{num}/{den}").into());
            args.extend(
                ["-start_number", "0", "-i"]
                    .iter()
                    .map(std::ffi::OsString::from),
            );
            args.push(
                ordinary_path(&job_dir.join("frames").join("frame_%06d.png")).into_os_string(),
            );
            let codec: &[&str] = if transparent {
                &[
                    "-c:v",
                    "prores_ks",
                    "-profile:v",
                    "4",
                    "-pix_fmt",
                    "yuva444p10le",
                ]
            } else {
                &[
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-movflags",
                    "+faststart",
                ]
            };
            args.extend(codec.iter().map(std::ffi::OsString::from));
            args.extend(["-frames:v".into(), frames.to_string().into()]);
            args.push(
                ordinary_path(&job_dir.join(format!("movie.renderright-tmp.{ext}")))
                    .into_os_string(),
            );
            let mut command = OwnedCommand::new(ffmpeg);
            command
                .command_mut()
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let mut child = command
                .spawn()
                .map_err(|e| format!("could not run ffmpeg: {e}"))?;
            let tail = child
                .take_stderr()
                .map(|stderr| rightkit_child_process::StderrTail::plain(stderr, 8 * 1024));
            Ok(EncodeRun {
                child,
                tail,
                started: Instant::now(),
            })
        })())
    }

    /// One non-blocking look: `None` while running, `Some(result)` when it ended (exit, cancel, deadline).
    fn step(&mut self, job_id: &str) -> Option<Result<(), String>> {
        if cancelled(job_id) {
            self.abort();
            return Some(Err("cancelled".into()));
        }
        if self.started.elapsed() >= Duration::from_secs(600) {
            self.abort();
            return Some(Err("ffmpeg encode timed out".into()));
        }
        match self.child.wait_timeout(Duration::from_millis(1)) {
            Ok(None) => None,
            Ok(Some(status)) => {
                let text = self
                    .tail
                    .as_mut()
                    .map(|t| t.finish_within(Duration::from_secs(2)))
                    .unwrap_or_default();
                Some(if status.success() {
                    Ok(())
                } else {
                    let skip = text.chars().count().saturating_sub(2000);
                    Err(text.chars().skip(skip).collect())
                })
            }
            Err(e) => {
                self.abort();
                Some(Err(format!("could not observe ffmpeg: {e}")))
            }
        }
    }

    /// Terminates the whole ffmpeg tree through the owner (bounded).
    fn abort(&mut self) {
        let _ = self.child.terminate_tree_bounded(Duration::from_secs(10));
    }
}

fn write_encode_result(job_dir: &Path, answer: Result<(), String>) {
    let value = match answer {
        Ok(()) => json!({"schema":1,"ok":true}),
        Err(message) => {
            json!({"schema":1,"ok":false,"cancelled":message == "cancelled","message":message})
        }
    };
    let _ = atomic_json(&job_dir.join("encode-result.json"), &value);
}

/// Advances the encode by one non-blocking step (start it when the worker asked, else check the running one).
fn encode_tick(run: &mut Option<EncodeRun>, job_dir: &Path, ffmpeg: &Path, job_id: &str) {
    if let Some(active) = run.as_mut() {
        if let Some(done) = active.step(job_id) {
            *run = None;
            write_encode_result(job_dir, done);
        }
        return;
    }
    match EncodeRun::start(job_dir, ffmpeg) {
        None => {}
        Some(Ok(started)) => *run = Some(started),
        Some(Err(message)) => write_encode_result(job_dir, Err(message)),
    }
}

/// Blender argv for one render attempt. The Cycles device override follows `--`, the form the
/// Blender manual documents for `--cycles-device` (`blender -b file.blend -- --cycles-device OPTIX`).
/// On macOS, `device=auto` selects Metal (`METAL`, Apple Silicon); `cpu` and other hosts keep the
/// Blender preference and the worker's own choice.
fn worker_args(
    entry: &Path,
    job_file: &Path,
    device: &str,
    metal_host: bool,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = [
        "-b",
        "--factory-startup",
        "--python-exit-code",
        "1",
        "--python",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .collect();
    args.push(entry.as_os_str().to_owned());
    args.push("--".into());
    args.push(job_file.as_os_str().to_owned());
    if metal_host && device == "auto" {
        args.push("--cycles-device".into());
        args.push("METAL".into());
    }
    args
}

fn parse_provider_id(provider_id: &str) -> Option<(String, i64)> {
    let (job_id, attempt) = provider_id.rsplit_once(":attempt:")?;
    if job_id.is_empty()
        || job_id.len() > 128
        || !job_id.as_bytes()[0].is_ascii_alphanumeric()
        || !job_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return None;
    }
    Some((job_id.to_owned(), attempt.parse().ok()?))
}

fn verify_manifest(
    dir: &Path,
    id: &str,
    job: &Value,
    input_hash: &str,
    worker: &WorkerInstall,
) -> Result<Value, ProviderError> {
    let actual_input_hash = hash_file(&dir.join("job.json"))
        .map_err(|e| unavailable(format!("could not verify submitted job identity: {e}")))?;
    if actual_input_hash != input_hash {
        return Err(unavailable(
            "submitted job JSON does not match the Blender exit receipt",
        ));
    }
    let manifest: Value = read_json(&dir.join("result-manifest.json")).map_err(|e| {
        unavailable(format!(
            "Blender exited successfully but result manifest is invalid: {e}"
        ))
    })?;
    if manifest["schema"].as_u64() != Some(1) || manifest["job_id"].as_str() != Some(id) {
        return Err(unavailable("result manifest job identity does not match"));
    }
    if manifest["worker"]["name"].as_str() != Some("renderright")
        || manifest["worker"]["version"].as_str() != Some(worker.version.as_str())
    {
        return Err(unavailable(
            "result manifest worker identity does not match",
        ));
    }
    if manifest["recipe"]["id"].as_str() != job["recipe"].as_str()
        || manifest["recipe"]["version"].as_str()
            != worker.recipe_manifest["recipes"][job["recipe"].as_str().unwrap_or("")]["version"]
                .as_str()
        || manifest["recipe"]["sha256"].as_str() != Some(&worker.recipe_hash)
    {
        return Err(unavailable("result manifest recipe does not match request"));
    }
    let expected_blender = job["params"]["blender_version"]
        .as_str()
        .ok_or_else(|| unavailable("submitted job has no Blender version expectation"))?;
    if manifest["blender_version"]
        .as_str()
        .and_then(normalized_blender_version)
        .as_deref()
        != Some(expected_blender.trim())
    {
        return Err(unavailable(
            "result manifest Blender version does not match submitted job",
        ));
    }
    let source = job["source"].as_str().unwrap_or("");
    if !source.is_empty() {
        let stored_hash = job["params"]["source_sha256"]
            .as_str()
            .ok_or_else(|| unavailable("submitted job has no normalized source hash"))?;
        let current_hash = hash_file(Path::new(source))
            .map_err(|e| unavailable(format!("could not verify source identity: {e}")))?;
        if current_hash != stored_hash {
            return Err(unavailable(
                "source changed after the Blender job was submitted",
            ));
        }
        if manifest["source"]["path"].as_str().is_none_or(|p| {
            Path::new(p).canonicalize().ok().as_deref()
                != Path::new(source).canonicalize().ok().as_deref()
        }) || manifest["source"]["sha256"].as_str() != Some(stored_hash)
        {
            return Err(unavailable(
                "result manifest source identity does not match",
            ));
        }
    }
    if manifest["job_fingerprint"]
        .as_str()
        .is_none_or(|s| s.is_empty())
    {
        return Err(unavailable("result manifest has no job fingerprint"));
    }
    let progress = dir.join("render-progress.json");
    if !progress.is_file() {
        return Err(unavailable("render progress receipt is missing"));
    }
    let p: Value =
        read_json(&progress).map_err(|e| unavailable(format!("invalid render progress: {e}")))?;
    if p["state"].as_str() != Some("complete") {
        return Err(unavailable("render progress did not reach complete state"));
    }
    if p["job_id"].as_str() != Some(id)
        || p["job_fingerprint"].as_str() != manifest["job_fingerprint"].as_str()
    {
        return Err(unavailable(
            "progress and result manifest identities do not match",
        ));
    }
    if job["recipe"].as_str() != Some("probe") {
        verify_product_review(dir, job, &manifest)?;
    }
    Ok(manifest)
}

fn verify_product_review(dir: &Path, job: &Value, manifest: &Value) -> Result<(), ProviderError> {
    let path = job["params"]["product_spec"].as_str().ok_or_else(|| {
        review_required("render completed, but product review is missing; output cannot be adopted")
    })?;
    let spec: Value = read_json(Path::new(path))
        .map_err(|e| review_required(format!("current product spec is unavailable: {e}")))?;
    let summary = &manifest["product_review"];
    if summary.is_null() || !summary.is_object() {
        return Err(review_required(
            "result manifest has no product review summary",
        ));
    }
    let owner = spec["owner"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| review_required("product spec owner is missing"))?;
    if spec["schema"].as_u64() != Some(1)
        || spec["product_id"].as_str().is_none_or(str::is_empty)
        || spec["variant"].as_str().is_none_or(str::is_empty)
    {
        return Err(review_required(
            "product spec identity or schema is invalid",
        ));
    }
    if spec["confirmed_by"].as_str() != Some(owner) {
        return Err(review_required(
            "product spec is not confirmed by its owner",
        ));
    }
    // `confirmed` and `approved` are booleans any caller can write into the spec file. They count only
    // with the engine's approval receipts (`studio.blender.spec.approve` writes them after consuming
    // an app-minted token for exactly this spec); see `spec_receipts_ok`.
    spec_receipts_ok(&crate::host::app_data_root(), &spec).map_err(review_required)?;
    let source_hash = job["params"]["source_sha256"]
        .as_str()
        .ok_or_else(|| review_required("submitted job has no source identity"))?;
    if summary["product_id"] != spec["product_id"]
        || summary["variant"] != spec["variant"]
        || summary["confirmed"] != spec["confirmed"]
        || summary["source_sha256"].as_str() != Some(source_hash)
    {
        return Err(review_required(
            "result product review identity does not match current spec and source",
        ));
    }
    let actual_tags = summary["finish_tags"]
        .as_object()
        .ok_or_else(|| review_required("product review finish_tags is missing"))?;
    let mut expected_tags = std::collections::BTreeMap::<String, String>::new();
    let parts = spec["parts"]
        .as_object()
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| review_required("product spec parts are missing"))?;
    let finishes = spec["finishes"]
        .as_object()
        .ok_or_else(|| review_required("product spec finishes are missing"))?;
    for (part_name, part) in parts {
        let slots = part["slots"]
            .as_object()
            .filter(|slots| !slots.is_empty())
            .ok_or_else(|| {
                review_required(format!("product spec part {part_name} has no finish slots"))
            })?;
        for (slot_name, slot_finish) in slots {
            let finish_id = slot_finish
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| {
                    review_required(format!(
                        "product spec finish slot {part_name}.{slot_name} has no valid finish id"
                    ))
                })?;
            let finish = finishes.get(finish_id).ok_or_else(|| {
                review_required(format!(
                    "product spec references unknown finish {finish_id}"
                ))
            })?;
            let version = finish["version"]
                .as_str()
                .filter(|version| !version.trim().is_empty())
                .ok_or_else(|| {
                    review_required(format!("finish {finish_id} has no valid version"))
                })?;
            expected_tags.insert(finish_id.to_owned(), version.to_owned());
            if spec["confirmed"] != Value::Bool(true)
                || finish["approved"] != Value::Bool(true)
                || finish["approved_by"].as_str() != Some(owner)
                || actual_tags.get(finish_id).and_then(Value::as_str) != Some(version)
            {
                return Err(review_required(format!(
                    "finish {finish_id} is not currently owner-approved for adoption"
                )));
            }
        }
    }
    if expected_tags.is_empty() || actual_tags.is_empty() {
        return Err(review_required("product review has no used finish tags"));
    }
    let expected_value =
        serde_json::to_value(expected_tags).map_err(|e| review_required(e.to_string()))?;
    if summary["finish_tags"] != expected_value
        || summary["reviewed"] != Value::Bool(true)
        || summary["approved_by"].as_str() != Some(owner)
    {
        return Err(review_required(
            "product review summary is draft or does not match current approvals",
        ));
    }
    let shot_artifact = manifest["artifacts"]
        .as_array()
        .and_then(|items| items.iter().find(|a| a["role"] == "shot_manifest"))
        .ok_or_else(|| review_required("result manifest has no shot manifest"))?;
    let shot_manifest = shot_artifact["path"]
        .as_str()
        .ok_or_else(|| review_required("shot manifest path is missing"))?;
    let rel = Path::new(shot_manifest);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(review_required(
            "shot manifest path escapes worker output directory",
        ));
    }
    let root = dir
        .canonicalize()
        .map_err(|e| review_required(e.to_string()))?;
    let shot_path = dir
        .join(rel)
        .canonicalize()
        .map_err(|e| review_required(format!("shot manifest is unavailable: {e}")))?;
    if !shot_path.starts_with(&root) || !shot_path.is_file() {
        return Err(review_required(
            "shot manifest is outside worker output directory",
        ));
    }
    let shot_hash = shot_artifact["sha256"]
        .as_str()
        .ok_or_else(|| review_required("shot manifest hash is missing"))?;
    let shot_bytes = shot_artifact["bytes"]
        .as_u64()
        .ok_or_else(|| review_required("shot manifest byte count is missing"))?;
    if hash_file(&shot_path).map_err(|e| review_required(e.to_string()))? != shot_hash
        || fs::metadata(&shot_path)
            .map_err(|e| review_required(e.to_string()))?
            .len()
            != shot_bytes
    {
        return Err(review_required(
            "shot manifest hash or size does not match result manifest",
        ));
    }
    let shot: Value = read_json(&shot_path)
        .map_err(|e| review_required(format!("shot manifest is invalid: {e}")))?;
    if shot["product_review"] != *summary {
        return Err(review_required(
            "shot manifest product review does not match result manifest",
        ));
    }
    Ok(())
}

/// Why the spec's confirmation does not carry the app's approval, or `Ok` when the confirmation and
/// every finish's approval each name a challenge the engine issued and a token consumed for this
/// spec's current approval inputs (parts, slots, finish ids and versions, owner). The hash ignores
/// the booleans and records the approval sets, so a hand-set flag adds nothing and an edit to the
/// spec after approval invalidates it.
fn spec_receipts_ok(data_dir: &Path, spec: &Value) -> Result<(), String> {
    use crate::host::approval;
    let hash = approval::blender_draft_id(spec);
    let valid = |record: &Value| {
        approval::receipt_valid(data_dir, approval::BLENDER_KIND, &hash, &hash, record)
    };
    if !valid(&spec["confirmation"]) {
        return Err("product spec confirmation has no approval from the app for its current contents; approve it in the app".into());
    }
    let finishes = spec["finishes"]
        .as_object()
        .ok_or("product spec finishes are missing")?;
    for (id, finish) in finishes {
        if !valid(&finish["approval"]) {
            return Err(format!("finish {id} has no approval from the app for the current spec; approve it in the app"));
        }
    }
    Ok(())
}

fn manifest_outputs(dir: &Path, manifest: &Value) -> Result<Vec<Output>, ProviderError> {
    let artifacts = manifest["artifacts"]
        .as_array()
        .ok_or_else(|| unavailable("result manifest has no artifact list"))?;
    let mut outputs = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for artifact in artifacts {
        let role = artifact["role"].as_str().unwrap_or("");
        if !matches!(role, "movie" | "hero" | "shot_manifest" | "probe") {
            continue;
        }
        let artifact_id = artifact["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| unavailable("artifact id is missing"))?;
        if !ids.insert(artifact_id) {
            return Err(unavailable("result manifest repeats a final artifact id"));
        }
        let rel = artifact["path"]
            .as_str()
            .ok_or_else(|| unavailable("artifact path is missing"))?;
        let relative = Path::new(rel);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(unavailable("artifact path escapes worker output directory"));
        }
        let root = dir.canonicalize().map_err(|e| unavailable(e.to_string()))?;
        let claimed_path = dir.join(relative);
        if let Some(parent_path) = claimed_path.parent() {
            match parent_path.canonicalize() {
                Ok(parent) if parent.starts_with(&root) => {}
                Ok(_) => {
                    return Err(unavailable(
                        "artifact parent is outside worker output directory",
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(unavailable(format!(
                        "could not resolve artifact parent: {e}"
                    )))
                }
            }
        }
        let path = match claimed_path.canonicalize() {
            Ok(path) if path.starts_with(&root) && path.is_file() => path,
            Ok(_) => {
                return Err(unavailable(
                    "artifact path is outside worker output directory",
                ))
            }
            // Descriptor adoption may already have moved a verified file into
            // the library; retain deterministic path for its receipt lookup.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => claimed_path,
            Err(e) => return Err(unavailable(format!("could not resolve artifact path: {e}"))),
        };
        let sha = artifact["sha256"]
            .as_str()
            .ok_or_else(|| unavailable("artifact hash is missing"))?;
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(unavailable("artifact hash is not SHA-256"));
        }
        let bytes = artifact["bytes"]
            .as_u64()
            .ok_or_else(|| unavailable("artifact byte count is missing"))?;
        if bytes == 0 {
            return Err(unavailable("artifact is empty"));
        }
        if path.is_file() {
            let actual = hash_file(&path).map_err(|e| unavailable(e.to_string()))?;
            let len = fs::metadata(&path)
                .map_err(|e| unavailable(e.to_string()))?
                .len();
            if actual != sha || len != bytes {
                return Err(unavailable("artifact hash or size does not match manifest"));
            }
        }
        let mime = artifact["mime"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| unavailable("artifact mime is missing"))?;
        outputs.push(Output::Path {
            path,
            mime: mime.into(),
            sha256: sha.into(),
            bytes,
        });
    }
    if outputs.is_empty() {
        return Err(unavailable("result manifest has no final media artifacts"));
    }
    Ok(outputs)
}

fn cancelled(job_id: &str) -> bool {
    if gen_job_cancelled(job_id) {
        return true;
    }
    crate::host::app_data_root()
        .join("blender-jobs")
        .join(job_id)
        .join("cancel.json")
        .is_file()
}

fn gen_job_cancelled(job_id: &str) -> bool {
    let ctx = crate::Context::production();
    crate::store::open(&ctx)
        .ok()
        .and_then(|db| crate::store::get_job(&db, job_id).ok())
        .is_some_and(|job| job.status == "cancelled")
}

fn probe(
    blender: &Path,
    args: &[&str],
    log_dir: &Path,
    label: &str,
) -> Result<String, ProviderError> {
    let out = rightkit_child_process::run(
        ordinary_path(blender),
        args,
        &RunOptions::new(Duration::from_secs(30)),
    )
    .map_err(|e| unavailable(format!("could not run Blender version probe: {e}")))?;
    if out.timed_out {
        return Err(unavailable("Blender version probe timed out"));
    }
    let text = String::from_utf8_lossy(&out.stdout.bytes).into_owned();
    let stderr_text = String::from_utf8_lossy(&out.stderr.bytes).into_owned();
    rightkit_fs::atomic_write(
        &log_dir.join(format!("{label}.stderr.log")),
        stderr_text.as_bytes(),
    )
    .map_err(|e| unavailable(format!("could not persist Blender probe stderr: {e}")))?;
    rightkit_fs::atomic_write(
        &log_dir.join(format!("{label}.stdout.log")),
        text.as_bytes(),
    )
    .map_err(|e| unavailable(format!("could not persist Blender probe output: {e}")))?;
    if out.status.success() {
        return Ok(text);
    }
    Err(unavailable(format!(
        "Blender version probe failed: {text} {}",
        stderr_text.trim()
    )))
}

fn file_identity(path: &Path) -> String {
    fs::metadata(path)
        .map(|m| {
            format!(
                "{}:{}",
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            )
        })
        .unwrap_or_default()
}
fn read_log_tail(path: &Path) -> Option<String> {
    let tail = rightkit_logs::tail_bytes(path, 8192).ok()?;
    let text = String::from_utf8_lossy(&tail).trim().to_owned();
    (!text.is_empty()).then_some(text)
}
fn normalized_blender_version(output: &str) -> Option<String> {
    let first = output.lines().next()?.trim();
    let version = first.strip_prefix("Blender ").unwrap_or(first).trim();
    let mut fields = version.split_whitespace();
    let core = fields.next()?;
    let components: Vec<_> = core.split('.').collect();
    if components.len() != 3
        || components
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }

    let suffix = fields.collect::<Vec<_>>();
    if !matches!(
        suffix.as_slice(),
        [] | ["LTS"] | ["Alpha"] | ["Beta"] | ["Candidate"]
    ) {
        return None;
    }
    Some(
        std::iter::once(core)
            .chain(suffix)
            .collect::<Vec<_>>()
            .join(" "),
    )
}
fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}
fn hash_file(path: &Path) -> std::io::Result<String> {
    rightkit_fs::digest::sha256_file(path)
}
fn sha256_bytes(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}
fn atomic_json(path: &Path, value: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    rightkit_fs::atomic_write(path, &bytes).map_err(|e| std::io::Error::other(e.to_string()))
}
fn check_hash_param(
    params: &Map<String, Value>,
    name: &str,
    actual: &str,
) -> Result<(), ProviderError> {
    if let Some(value) = params.get(name) {
        if value.as_str() != Some(actual) {
            return Err(invalid(format!(
                "params.{name} does not match installed/source content"
            )));
        }
    }
    Ok(())
}
fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new("invalid_request", message, false)
}
fn unavailable(message: impl Into<String>) -> ProviderError {
    ProviderError::new("not_installed", message, true)
}

fn review_required(message: impl Into<String>) -> ProviderError {
    ProviderError::new("product_review_required", message, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(args: &[std::ffi::OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn macos_auto_device_selects_metal_after_job_separator() {
        let args = text(&worker_args(
            Path::new("/w/render_job.py"),
            Path::new("/j/job.json"),
            "auto",
            true,
        ));
        assert_eq!(
            args,
            [
                "-b",
                "--factory-startup",
                "--python-exit-code",
                "1",
                "--python",
                "/w/render_job.py",
                "--",
                "/j/job.json",
                "--cycles-device",
                "METAL"
            ]
        );
    }

    #[test]
    fn macos_cpu_device_keeps_cpu_render_without_metal() {
        let args = text(&worker_args(
            Path::new("/w/render_job.py"),
            Path::new("/j/job.json"),
            "cpu",
            true,
        ));
        assert!(!args.iter().any(|a| a == "--cycles-device" || a == "METAL"));
        assert_eq!(args.last().map(String::as_str), Some("/j/job.json"));
    }

    #[test]
    fn non_macos_hosts_get_no_metal_override() {
        let args = text(&worker_args(
            Path::new("/w/render_job.py"),
            Path::new("/j/job.json"),
            "auto",
            false,
        ));
        assert!(!args.iter().any(|a| a == "--cycles-device" || a == "METAL"));
        assert_eq!(args.len(), 8);
    }
}

#[cfg(test)]
mod approval_gate_tests {
    use super::*;
    use crate::host::approval;

    fn temp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-blender-gate-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spec() -> Value {
        json!({
            "schema": 1, "product_id": "p", "variant": "v", "owner": "adrian",
            "confirmed": true, "confirmed_by": "adrian",
            "parts": {"body": {"slots": {"main": "matte"}}},
            "finishes": {"matte": {"version": "1", "approved": true, "approved_by": "adrian"}},
        })
    }

    /// What the engine's approve writes: an issued, consumed challenge and the stamped records.
    fn approve(dir: &Path, mut spec: Value) -> Value {
        let hash = approval::blender_draft_id(&spec);
        let nonce = "0123456789abcdef0123456789abcdef";
        let file = approval::challenge_file(dir, approval::BLENDER_KIND, &hash, nonce).unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            json!({"nonce": nonce, "input_hash": hash}).to_string(),
        )
        .unwrap();
        fs::write(format!("{}.used", file.display()), "t").unwrap();
        let record = json!({"kind": approval::BLENDER_KIND, "nonce": nonce, "input_hash": hash, "channel": "sidecar_secret", "approved_at": "t"});
        spec["confirmation"] = record.clone();
        spec["finishes"]["matte"]["approval"] = record;
        spec
    }

    #[test]
    fn caller_set_booleans_do_not_confirm_a_finish() {
        let dir = temp("flags");
        let err = spec_receipts_ok(&dir, &spec()).unwrap_err();
        assert!(err.contains("no approval from the app"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_app_approved_spec_is_accepted_and_an_edit_or_a_copied_record_is_not() {
        let dir = temp("ok");
        let approved = approve(&dir, spec());
        spec_receipts_ok(&dir, &approved).unwrap();

        // Editing a finish after approval invalidates it (the draft id moves).
        let mut edited = approved.clone();
        edited["finishes"]["matte"]["version"] = json!("2");
        assert!(spec_receipts_ok(&dir, &edited).is_err());

        // A record copied onto a different spec names another hash.
        let mut other = spec();
        other["parts"]["body"]["slots"]["main"] = json!("gloss");
        other["finishes"]["gloss"] = json!({"version": "1"});
        other["confirmation"] = approved["confirmation"].clone();
        other["finishes"]["matte"]["approval"] = approved["confirmation"].clone();
        other["finishes"]["gloss"]["approval"] = approved["confirmation"].clone();
        assert!(spec_receipts_ok(&dir, &other).is_err());

        // A finish added without its own approval record is refused even if the confirmation is valid.
        let mut extra = approved.clone();
        extra["finishes"]["matte"]
            .as_object_mut()
            .unwrap()
            .remove("approval");
        assert!(spec_receipts_ok(&dir, &extra).is_err());

        // An unconsumed challenge (no `.used` marker) approves nothing.
        let unused = temp("unused");
        let forged = approve(&unused, spec());
        for entry in fs::read_dir(unused.join("approvals").join(approval::BLENDER_KIND))
            .unwrap()
            .flatten()
        {
            if entry.path().extension().is_some_and(|e| e == "used") {
                fs::remove_file(entry.path()).unwrap();
            }
        }
        assert!(spec_receipts_ok(&unused, &forged).is_err());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&unused);
    }
}
