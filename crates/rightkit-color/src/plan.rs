//! The render-path API: [`ColorPlan`] -> [`build_fragment`] -> [`ColorFragment`].
//!
//! A fragment is one linear `-vf` chain (no split/blend, no labels) ordered
//! SDR conversion -> exposure / white balance -> shot match -> user grade -> bounded LUT ->
//! bt709 tagging. It may reference generated `.cube` files (written under `BuildEnv::work_dir`, named by content
//! hash so retries are idempotent) which the caller keeps next to the render.

use std::path::{Path, PathBuf};

use rightkit_ffmpeg::FfmpegCommand;
use rightkit_fs::atomic_write;
use serde::{Deserialize, Serialize};

use crate::{
    apple_log, clamp_or,
    correct::{ColorCorrection, CorrectionLimits, CorrectionOverrides},
    cube::Cube3d,
    grade::Grade,
    run_media_capture,
    sample::FrameStats,
    sdr::SdrConversion,
    shot_match::ShotMatch,
    space::{probe_color_tags, ColorSpaceKind},
    ColorError,
};

/// Tags every converted render must carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedTags {
    pub color_primaries: &'static str,
    pub color_transfer: &'static str,
    pub color_matrix: &'static str,
}

pub const EXPECTED_SDR_TAGS: ExpectedTags = ExpectedTags {
    color_primaries: "bt709",
    color_transfer: "bt709",
    color_matrix: "bt709",
};

/// A user LUT with a bounded intensity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LutRef {
    pub path: PathBuf,
    /// 0..=1; clamped.
    pub intensity: f64,
}

/// Automatic exposure / white balance request; resolved with [`ColorPlan::resolve_auto`].
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoSpec {
    pub limits: CorrectionLimits,
    pub overrides: CorrectionOverrides,
}

const PIX_FMTS: [&str; 5] = [
    "yuv420p",
    "yuv422p",
    "yuv420p10le",
    "yuv422p10le",
    "yuv444p",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorPlan {
    pub sdr: SdrConversion,
    /// Fixed correction (manual preset or the result of [`ColorPlan::resolve_auto`]).
    pub correction: Option<ColorCorrection>,
    /// When set, [`ColorPlan::resolve_auto`] fills `correction` from frame statistics.
    pub auto: Option<AutoSpec>,
    pub shot_match: Option<ShotMatch>,
    /// The user's curves / wheels / HSL / saturation grade, after shot match and before the LUT.
    pub grade: Option<Grade>,
    pub lut: Option<LutRef>,
    /// Output pixel format of the fragment (whitelisted).
    pub pix_fmt: String,
}

impl Default for ColorPlan {
    fn default() -> Self {
        ColorPlan {
            sdr: SdrConversion::default(),
            correction: None,
            auto: None,
            shot_match: None,
            grade: None,
            lut: None,
            pix_fmt: "yuv420p".into(),
        }
    }
}

impl ColorPlan {
    /// Fill `correction` from `stats` if the plan asks for automatic correction.
    pub fn resolve_auto(&mut self, stats: &FrameStats) {
        if let Some(a) = &self.auto {
            self.correction = Some(crate::auto_correction(stats, &a.limits, &a.overrides));
        }
    }
}

/// Where generated files go.
#[derive(Debug, Clone, Copy)]
pub struct BuildEnv<'a> {
    pub work_dir: &'a Path,
    /// `false` (dry run) builds the same filter text and file names but writes
    /// nothing: the render never runs, so the LUT need not exist yet.
    pub write: bool,
}

impl<'a> BuildEnv<'a> {
    pub fn new(work_dir: &'a Path) -> Self {
        Self {
            work_dir,
            write: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColorFragment {
    /// Comma-joined filter chain; empty only if nothing needs doing and tags are already right
    /// (never: tagging is always appended).
    pub filter: String,
    /// Files the filter references (generated or user-supplied LUT copies).
    pub files: Vec<PathBuf>,
    /// FFmpeg filter names that must exist (see [`FilterCapabilities::check`]).
    pub requires: Vec<&'static str>,
    pub expected: ExpectedTags,
    /// Encoder flags that write the bt709 tags into the stream (`-color_primaries ...`).
    pub encoder_args: Vec<String>,
}

/// Escape a path for use inside `file='...'` of a filter option (Windows drive colons and
/// backslashes included). Quotes and control characters are rejected.
pub fn escape_filter_path(path: &Path) -> Result<String, ColorError> {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.chars().any(|c| c == '\'' || c.is_control()) {
        return Err(ColorError::Invalid(format!(
            "unsupported character in path {s:?}"
        )));
    }
    Ok(s.replace(':', "\\:"))
}

fn write_cube(env: &BuildEnv<'_>, name: &str, cube: &Cube3d) -> Result<PathBuf, ColorError> {
    let path = env
        .work_dir
        .join(format!("{name}-{}.cube", cube.fingerprint()));
    if !env.write {
        return Ok(path);
    }
    std::fs::create_dir_all(env.work_dir)?;
    if !path.is_file() {
        atomic_write(&path, cube.to_text().as_bytes()).map_err(std::io::Error::from)?;
    }
    Ok(path)
}

/// The programme-level mastering stage: the bounded user / preset LUT, applied once to the whole
/// composed programme after every per-source stage and all compositing. `setparams` is not part
/// of it; the caller appends the bt709 tags after this stage.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterStage {
    pub filter: String,
    pub files: Vec<PathBuf>,
    pub requires: Vec<&'static str>,
}

/// The master stage for `plan.lut`, or `None` when there is no LUT or its intensity is zero.
/// Fails on a malformed user LUT or an unsupported `pix_fmt`, like [`build_fragment`].
pub fn build_master_stage(
    plan: &ColorPlan,
    env: &BuildEnv<'_>,
) -> Result<Option<MasterStage>, ColorError> {
    let pix_fmt = PIX_FMTS
        .iter()
        .find(|p| **p == plan.pix_fmt)
        .ok_or_else(|| ColorError::Invalid(format!("unsupported pix_fmt {}", plan.pix_fmt)))?;
    let Some(lut) = &plan.lut else {
        return Ok(None);
    };
    Ok(
        lut_stage(lut, pix_fmt, env)?.map(|(filter, path)| MasterStage {
            filter,
            files: vec![path],
            requires: vec!["lut3d"],
        }),
    )
}

/// The `lut3d` filter text and the written `.cube` path for a LUT of non-zero intensity.
fn lut_stage(
    lut: &LutRef,
    pix_fmt: &str,
    env: &BuildEnv<'_>,
) -> Result<Option<(String, PathBuf)>, ColorError> {
    let intensity = clamp_or(lut.intensity, 0.0, 1.0, 0.0);
    if intensity <= 0.0 {
        return Ok(None);
    }
    let path = lut.path.display().to_string();
    let text = std::fs::read_to_string(&lut.path).map_err(|reason| ColorError::LutUnreadable {
        path: path.clone(),
        reason,
    })?;
    let cube = Cube3d::parse(&text)
        .map_err(|reason| ColorError::LutInvalid {
            path: path.clone(),
            reason,
        })?
        .mixed(intensity);
    let file = write_cube(env, "user-lut", &cube)?;
    let filter = format!(
        "lut3d=file='{}':interp=tetrahedral,format={pix_fmt}",
        escape_filter_path(&file)?
    );
    Ok(Some((filter, file)))
}

/// Assemble the fragment for a source of `kind`. Fails on a malformed user LUT.
/// `plan.lut` is applied here only for single-clip callers; a programme render sets it to `None`
/// and applies [`build_master_stage`] after compositing instead.
pub fn build_fragment(
    plan: &ColorPlan,
    kind: ColorSpaceKind,
    env: &BuildEnv<'_>,
) -> Result<ColorFragment, ColorError> {
    let pix_fmt = PIX_FMTS
        .iter()
        .find(|p| **p == plan.pix_fmt)
        .ok_or_else(|| ColorError::Invalid(format!("unsupported pix_fmt {}", plan.pix_fmt)))?;
    let mut stages: Vec<String> = Vec::new();
    let mut files = Vec::new();
    let mut requires: Vec<&'static str> = Vec::new();

    match kind {
        ColorSpaceKind::Rec2020Pq | ColorSpaceKind::Rec2020Hlg => {
            if let Some(f) = plan.sdr.hdr_fragment(kind, pix_fmt) {
                stages.push(f);
            }
            requires.extend(["zscale", "tonemap", "format"]);
        }
        ColorSpaceKind::AppleLog => {
            let stops = plan.sdr.clamped().apple_log_exposure_stops;
            let path = write_cube(
                env,
                "apple-log-rec709",
                &apple_log::rec709_lut(apple_log::DEFAULT_SIZE, stops),
            )?;
            files.push(path.clone());
            stages.push(format!(
                "scale=in_color_matrix=bt2020:in_range=tv:out_range=pc,format=gbrp16le,\
                 lut3d=file='{}':interp=tetrahedral,\
                 scale=out_color_matrix=bt709:out_range=tv,format={pix_fmt}",
                escape_filter_path(&path)?
            ));
            requires.extend(["scale", "lut3d"]);
        }
        ColorSpaceKind::Rec709 | ColorSpaceKind::Unknown => {
            stages.push(format!("format={pix_fmt}"));
        }
    }

    if let Some(c) = plan.correction.as_ref().and_then(ColorCorrection::filter) {
        if c.contains("colortemperature") {
            requires.push("colortemperature");
        }
        requires.push("colorchannelmixer");
        stages.push(c);
    }
    if let Some(m) = plan.shot_match.as_ref().and_then(ShotMatch::filter) {
        requires.push(if m.starts_with("lutrgb") {
            "lutrgb"
        } else {
            "lutyuv"
        });
        stages.push(m);
        stages.push(format!("format={pix_fmt}"));
    }
    if let Some(grade) = &plan.grade {
        grade.validate()?;
        if let Some(g) = grade.filter() {
            requires.extend(grade.requires());
            stages.push(g);
            stages.push(format!("format={pix_fmt}"));
        }
    }
    // The master LUT is programme-level: a caller that renders a programme takes it out of the
    // source plan (`plan.lut = None`) and applies [`build_master_stage`] once after compositing.
    if let Some(lut) = &plan.lut {
        if let Some((stage, path)) = lut_stage(lut, pix_fmt, env)? {
            stages.push(stage);
            files.push(path);
            requires.push("lut3d");
        }
    }
    stages.push("setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv".into());
    requires.push("setparams");
    requires.sort_unstable();
    requires.dedup();

    Ok(ColorFragment {
        filter: stages.join(","),
        files,
        requires,
        expected: EXPECTED_SDR_TAGS,
        encoder_args: [
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
        ]
        .map(String::from)
        .to_vec(),
    })
}

/// Filters available in an FFmpeg build (`ffmpeg -filters`).
#[derive(Debug, Clone, Default)]
pub struct FilterCapabilities {
    names: Vec<String>,
}

impl FilterCapabilities {
    pub fn from_filters_output(text: &str) -> Self {
        let names = text
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let flags = it.next()?;
                let name = it.next()?;
                ((2..=3).contains(&flags.len())
                    && flags.chars().all(|c| c == '.' || c.is_ascii_uppercase()))
                .then(|| name.to_string())
            })
            .collect();
        FilterCapabilities { names }
    }

    pub fn probe(ffmpeg: &Path) -> Result<Self, ColorError> {
        let mut cmd = FfmpegCommand::new();
        cmd.texts(["-hide_banner", "-filters"]);
        let out = run_media_capture(ffmpeg, &cmd)?;
        if !out.status.success() {
            return Err(ColorError::Process(out.stderr.trim().to_string()));
        }
        Ok(Self::from_filters_output(&String::from_utf8_lossy(
            &out.stdout,
        )))
    }

    /// [`probe`](Self::probe), remembered per binary path for the life of the process (the
    /// bundled ffmpeg does not change under a running engine). Failures are not remembered.
    pub fn probe_cached(ffmpeg: &Path) -> Result<Self, ColorError> {
        static CACHE: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, FilterCapabilities>>,
        > = std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        if let Some(hit) = cache.lock().ok().and_then(|c| c.get(ffmpeg).cloned()) {
            return Ok(hit);
        }
        let caps = Self::probe(ffmpeg)?;
        if let Ok(mut c) = cache.lock() {
            c.insert(ffmpeg.to_path_buf(), caps.clone());
        }
        Ok(caps)
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// Whether PQ and HLG sources can be converted to SDR with this build.
    pub fn hdr_to_sdr_available(&self) -> bool {
        self.has("zscale") && self.has("tonemap")
    }

    /// Error naming every filter the fragment needs that this build lacks.
    pub fn check(&self, fragment: &ColorFragment) -> Result<(), ColorError> {
        let missing: Vec<&str> = fragment
            .requires
            .iter()
            .copied()
            .filter(|f| !self.has(f))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(ColorError::MissingFilters(missing.join(", ")))
        }
    }
}

/// Confirm a render's colour tags match `expected` (a silently wrong transfer is a defect).
pub fn verify_output_tags(
    ffprobe: &Path,
    output: &Path,
    expected: &ExpectedTags,
) -> Result<(), ColorError> {
    let t = probe_color_tags(ffprobe, output)?;
    let ok = t.color_primaries.as_deref() == Some(expected.color_primaries)
        && t.color_transfer.as_deref() == Some(expected.color_transfer)
        && t.color_matrix.as_deref() == Some(expected.color_matrix);
    if ok {
        Ok(())
    } else {
        Err(ColorError::TagMismatch {
            expected: format!(
                "{}/{}/{}",
                expected.color_primaries, expected.color_transfer, expected.color_matrix
            ),
            actual: format!(
                "{:?}/{:?}/{:?}",
                t.color_primaries, t.color_transfer, t.color_matrix
            ),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub label: String,
    pub description: String,
    pub plan: ColorPlan,
}

#[derive(Deserialize)]
struct PresetFile {
    schema: u32,
    presets: Vec<Preset>,
}

const PRESETS_JSON: &str = include_str!("../presets/color-presets.json");

/// The shipped presets.
pub fn presets() -> Vec<Preset> {
    match serde_json::from_str::<PresetFile>(PRESETS_JSON) {
        Ok(f) if f.schema == 1 => f.presets,
        _ => Vec::new(),
    }
}

pub fn preset(id: &str) -> Option<Preset> {
    presets().into_iter().find(|p| p.id == id)
}
