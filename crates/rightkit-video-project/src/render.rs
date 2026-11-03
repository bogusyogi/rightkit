//! Render orchestration: compile the plan, reverify every source against its
//! registered BLAKE3, build the typed FFmpeg command, run it through the media
//! worker into a sweepable RightKit temp file, then publish on success.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_ffmpeg::{self as rk, FfmpegArg, FfmpegCommand};
use rightkit_timeline::{
    build_ffmpeg_measure, build_ffmpeg_render_styled, compile, AudioFinish, Encoder, FitPolicy,
    Preset, RenderPlan, RenderRequest, RenderStyle, SourceId, TextFile, Ticks, Timeline,
    TitleAssets, VideoSegment,
};

use crate::error::ProjectError;
use crate::loudness::{self, LoudnessReport, LoudnessSetting};
use crate::project::{Project, RENDERS_DIR};
use crate::source::verify_record;
use crate::tools::MediaTools;
use crate::worker::{run_ffmpeg, ProgressEvent, RunOptions, RunReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderChoice {
    Auto,
    Explicit(Encoder),
}

impl EncoderChoice {
    pub fn parse(text: &str) -> Result<Self, ProjectError> {
        Ok(match text {
            "auto" => EncoderChoice::Auto,
            "videotoolbox" => EncoderChoice::Explicit(Encoder::VideoToolbox),
            "nvenc" => EncoderChoice::Explicit(Encoder::Nvenc),
            "mf" => EncoderChoice::Explicit(Encoder::MediaFoundation),
            other => {
                return Err(ProjectError::BadInput(format!(
                    "unknown encoder {other:?}: expected auto, videotoolbox, nvenc or mf"
                )))
            }
        })
    }
}

pub fn parse_preset(text: &str) -> Result<Preset, ProjectError> {
    match text {
        "youtube" => Ok(Preset::YouTube),
        "reels" => Ok(Preset::Reels),
        "tiktok" => Ok(Preset::TikTok),
        "square" => Ok(Preset::Square),
        other => Err(ProjectError::BadInput(format!(
            "unknown preset {other:?}: expected youtube, reels, tiktok or square"
        ))),
    }
}

/// Platform safe-zone id matching a render preset (styled captions keep the
/// box clear of each platform's UI rails).
pub fn caption_zone_id(preset: Preset) -> &'static str {
    match preset {
        Preset::YouTube => "youtube",
        Preset::Reels => "instagram-reels",
        Preset::TikTok => "tiktok",
        Preset::Square => "generic-square",
    }
}

pub fn encoder_name(e: Encoder) -> &'static str {
    match e {
        Encoder::VideoToolbox => "videotoolbox",
        Encoder::Nvenc => "nvenc",
        Encoder::MediaFoundation => "mf",
    }
}

/// Output file stem: letters, digits, `.`, `_`, `-`; no leading dot; `.mp4` is implied.
pub fn sanitize_out_name(name: &str) -> Result<String, ProjectError> {
    let stem = name.strip_suffix(".mp4").unwrap_or(name);
    let ok = !stem.is_empty()
        && stem.len() <= 100
        && !stem.starts_with('.')
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(stem.to_string())
    } else {
        Err(ProjectError::BadInput(format!(
            "invalid output name {name:?}: use letters, digits, '.', '_' or '-'"
        )))
    }
}

/// Resolves the encoder against what the bundled ffmpeg lists (see [`rightkit_ffmpeg::encoders`]):
/// `auto` takes the platform order; an explicit choice must be listed.
pub fn resolve_encoder(
    choice: EncoderChoice,
    tools: &MediaTools,
    os: &str,
) -> Result<Encoder, ProjectError> {
    let binary = tools.ffmpeg_handle()?;
    let inventory = rk::EncoderInventory::probe(&binary, crate::tools::PROBE_TIMEOUT)
        .map_err(|e| ProjectError::EncoderUnavailable(e.to_string()))?;
    let selected = match choice {
        EncoderChoice::Explicit(encoder) => {
            let name = match encoder {
                Encoder::VideoToolbox => "h264_videotoolbox",
                Encoder::Nvenc => "h264_nvenc",
                Encoder::MediaFoundation => "h264_mf",
            };
            inventory.has(name).then_some(encoder)
        }
        EncoderChoice::Auto => match rk::pick_h264(&inventory, os, &mut |name| {
            rk::encoders::hardware_usable(
                &binary,
                name,
                crate::tools::PROBE_TIMEOUT,
                &rk::CancellationToken::new(),
            )
        }) {
            Some("h264_videotoolbox") => Some(Encoder::VideoToolbox),
            Some("h264_nvenc") => Some(Encoder::Nvenc),
            Some("h264_mf") => Some(Encoder::MediaFoundation),
            _ => None,
        },
    };
    selected.ok_or_else(|| {
        ProjectError::EncoderUnavailable(format!(
            "no supported delivery encoder for {os} and {choice:?}"
        ))
    })
}

/// What a render was produced from; recorded in the render record so retention and
/// review can bind the delivered file to an exact revision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderOrigin {
    /// The project's main timeline.
    Main,
    /// A saved variant's own timeline (shorts, alternate cuts). The main project
    /// head at render time is recorded for traceability.
    Variant {
        name: String,
        project_head_hash: String,
    },
}

pub struct RenderOptions {
    pub preset: Preset,
    pub encoder: EncoderChoice,
    /// File stem under `renders/`; defaults to `<preset>-<timeline hash prefix>`.
    pub out_name: Option<String>,
    pub job: String,
    pub timeout: Duration,
    pub cancel: Arc<AtomicBool>,
    /// Title font (`<resource_dir>/runtime/fonts/JetBrainsMono-Regular.ttf`); only needed
    /// when the timeline has titles.
    pub font: Option<PathBuf>,
    /// Fit of a source to the preset frame; `None` uses the preset's default
    /// (Reels centre-crops, YouTube letterboxes).
    pub fit: Option<FitPolicy>,
    /// Final loudness; the default normalises to -14 LUFS / -1.5 dBTP in two passes.
    pub loudness: LoudnessSetting,
    /// Cues to burn into the composed output (drawtext, after titles). Empty = no burn-in.
    /// Requires `font`; cues must already be mapped to output-timeline milliseconds.
    pub captions: Vec<rightkit_captions::Cue>,
    /// Caption style id from `StyleCatalog` ("classic-box", "karaoke-pop", ...).
    /// Set → cues are rebuilt with the style's segmentation and drawn through the
    /// styled path (box, outline, karaoke); unset → the plain boxed look.
    pub caption_style: Option<String>,
    /// Normalized output-timeline words `captions` cue indices refer to; required
    /// by the styled path (per-word karaoke/emphasis timing).
    pub caption_words: Vec<rightkit_captions::TimedWord>,
    /// Per-source filters applied inside each clip chain before fit/composition
    /// (colour normalisation of mixed HDR/SDR sources). Keyed by source id.
    pub video_pre: BTreeMap<SourceId, Vec<String>>,
    /// Finishing filters on the composed video after captions (output tagging only;
    /// pixel-affecting colour belongs in `video_pre` so captions are not recoloured).
    pub video_post: Vec<String>,
    /// Finishing filters on the composed mix BEFORE loudness mastering
    /// (dialogue cleanup/compression chain).
    pub audio_post: Vec<String>,
    /// Output-millisecond windows of programme dialogue; every `Music`-role
    /// layer is attenuated inside them (recipe Music stage ducking).
    pub music_duck_windows: Vec<(u64, u64)>,
    /// Extra encoder/muxer args appended before the output (e.g. colour tags).
    pub encoder_args: Vec<String>,
    /// What is being rendered (main timeline or a saved variant); recorded.
    pub origin: RenderOrigin,
}

/// Everything decided before ffmpeg runs.
pub struct Prepared {
    pub plan: RenderPlan,
    pub command: FfmpegCommand,
    pub encoder: Encoder,
    pub preset: Preset,
    pub final_path: PathBuf,
    pub partial_path: PathBuf,
    pub head_hash: String,
    pub cursor: usize,
    pub total_ms: i64,
    pub verified_sources: usize,
    /// Title text files to write before running `command` (content-addressed).
    pub text_files: Vec<TextFile>,
    pub fit: FitPolicy,
    pub loudness: LoudnessSetting,
    /// Verified input path per source, kept so the normalising command can be rebuilt
    /// once the measurement pass has run. `command` itself has no loudness pass.
    inputs: BTreeMap<SourceId, PathBuf>,
    font: Option<PathBuf>,
    text_dir: PathBuf,
    /// Caption drawtext filters applied on the composed output; kept so the loudness
    /// second-pass rebuild keeps burning them.
    captions: Vec<String>,
    /// Per-input finishing chains, indexed by plan input (rebuilt with the loudness
    /// second pass).
    video_pre: Vec<Vec<String>>,
    /// Post-compose finishing (output tags / dialogue cleanup / encoder tags), rebuilt
    /// with the loudness second pass.
    video_post: Vec<String>,
    audio_post: Vec<String>,
    encoder_args: Vec<String>,
    /// What was rendered (main or a named variant); recorded in the render record.
    pub origin: RenderOrigin,
}

/// Builds the render command (with the explicit `mp4` muxer for the partial name).
fn build_command(
    req: &RenderRequest<'_>,
    font: &Option<PathBuf>,
    text_dir: &Path,
    style: &RenderStyle,
) -> Result<(FfmpegCommand, Vec<TextFile>), ProjectError> {
    let plan = req.plan;
    let assets = match (font, plan.overlays.is_empty()) {
        (_, true) => None,
        (Some(font), false) if font.is_file() => Some(TitleAssets { font, text_dir }),
        (_, false) => {
            return Err(ProjectError::MediaToolMissing(
                "the title font is missing from the engine bundle (runtime/fonts)".into(),
            ))
        }
    };
    let built = build_ffmpeg_render_styled(req, assets.as_ref(), style)?;
    let mut command = FfmpegCommand::new();
    // The partial name has no media extension, so name the muxer explicitly (before the output).
    for arg in &built.command.args()[..built.command.args().len() - 1] {
        match arg {
            FfmpegArg::Text(text) => {
                command.text(text);
            }
            FfmpegArg::Path(path) => {
                command.path(path);
            }
        }
    }
    command.texts(["-f", "mp4"]).path(req.output);
    Ok((command, built.text_files))
}

/// A renderable revision: the project timeline itself, or a derived snapshot such
/// as a saved variant's own store. `dir` is the project directory (`renders/`
/// lives under it); `head_hash`/`cursor` identify the rendered revision in the
/// render record.
pub struct RenderTarget<'a> {
    pub dir: &'a Path,
    pub timeline: &'a Timeline,
    pub head_hash: String,
    pub cursor: usize,
}

/// Compiles, reverifies sources, picks the encoder, and builds the command.
/// Reads only; nothing is written.
pub fn prepare(
    project: &Project,
    tools: &MediaTools,
    opts: &RenderOptions,
    os: &str,
) -> Result<Prepared, ProjectError> {
    prepare_with_room_tone(project, tools, opts, os, None)
}

/// [`prepare`] with the optional room-tone bed. With `analysis` set, each source's room tone
/// is captured once (cached under that directory by source id), the programme's cut gaps are
/// filled from it, and the bed WAV is written there and attached to the plan. `None` keeps
/// the plan's gaps silent and writes nothing, so dry runs pass `None`.
pub fn prepare_with_room_tone(
    project: &Project,
    tools: &MediaTools,
    opts: &RenderOptions,
    os: &str,
    analysis: Option<&Path>,
) -> Result<Prepared, ProjectError> {
    prepare_inner(
        &RenderTarget {
            dir: &project.dir,
            timeline: project.timeline(),
            head_hash: project.store.head_hash(),
            cursor: project.store.cursor(),
        },
        tools,
        opts,
        os,
        &|id| {
            let record = project
                .source_record(id)
                .ok_or_else(|| ProjectError::SourceNotFound(format!("{id} (not registered)")))?;
            verify_record(project, record)
        },
        analysis,
    )
}

/// [`prepare`] against an explicit [`RenderTarget`]: `verify` resolves each plan
/// input's source id to its BLAKE3-reverified media path. Nothing is written.
pub fn prepare_with(
    target: &RenderTarget<'_>,
    tools: &MediaTools,
    opts: &RenderOptions,
    os: &str,
    verify: &dyn Fn(&SourceId) -> Result<PathBuf, ProjectError>,
) -> Result<Prepared, ProjectError> {
    prepare_inner(target, tools, opts, os, verify, None)
}

/// [`prepare_with`] with the optional room-tone bed, for variant and shorts renders that
/// work from a [`RenderTarget`] rather than a whole project. `analysis` has the same meaning
/// as in [`prepare_with_room_tone`]: `None` writes nothing and keeps the gaps silent.
pub fn prepare_with_target_room_tone(
    target: &RenderTarget<'_>,
    tools: &MediaTools,
    opts: &RenderOptions,
    os: &str,
    verify: &dyn Fn(&SourceId) -> Result<PathBuf, ProjectError>,
    analysis: Option<&Path>,
) -> Result<Prepared, ProjectError> {
    prepare_inner(target, tools, opts, os, verify, analysis)
}

fn prepare_inner(
    target: &RenderTarget<'_>,
    tools: &MediaTools,
    opts: &RenderOptions,
    os: &str,
    verify: &dyn Fn(&SourceId) -> Result<PathBuf, ProjectError>,
    analysis: Option<&Path>,
) -> Result<Prepared, ProjectError> {
    // Mic mappings are source-level and a short clip can legitimately have no
    // overlap with its microphone file. Reverify both sides before compiling
    // so an unattached or out-of-range microphone cannot bypass render gates.
    for attachment in &target.timeline.mic_attachments {
        verify(&attachment.camera)?;
        verify(&attachment.microphone)?;
    }
    let mut plan = compile(target.timeline)?;
    if let Some(analysis) = analysis {
        attach_room_tone(&mut plan, tools, analysis, verify)?;
    }
    // Corrected cues compile as positioned text overlays on the same snapshot.
    // The engine omits corrected cues from `opts.captions` before this point
    // (keyed by stable cue id, see the engine's caption track); only the overlays
    // of corrections survive an empty caption list.
    let corrections = &target.timeline.corrections.captions;
    if opts.captions.is_empty() {
        plan.overlays
            .retain(|o| match o.id.strip_prefix("caption:") {
                Some(key) => corrections.contains_key(key),
                None => true,
            });
    }
    let base_captions = opts.captions.clone();
    let base_words = opts.caption_words.clone();
    // R4: a music bed ducks under programme dialogue — each window lowers the
    // bed level while speech plays (in addition to the narration ducking the
    // plan itself derives).
    if !opts.music_duck_windows.is_empty() {
        for layer in plan
            .audio
            .layers
            .iter_mut()
            .filter(|l| l.role == rightkit_timeline::plan::LayerRole::Music)
        {
            for (a, b) in &opts.music_duck_windows {
                let (w0, w1) = (Ticks::from_millis(*a as i64), Ticks::from_millis(*b as i64));
                let l_end = Ticks(layer.start.0 + layer.duration.0);
                if w0 < l_end && layer.start < w1 {
                    layer.windows.push(rightkit_timeline::plan::DuckWindow {
                        start: w0.max(layer.start),
                        end: w1.min(l_end),
                        attenuation: rightkit_timeline::plan::Attenuation::Duck {
                            amount: rightkit_timeline::MilliDb::from_db(8.0),
                        },
                    });
                }
            }
        }
    }
    let head_hash = target.head_hash.clone();
    let stem = match &opts.out_name {
        Some(n) => sanitize_out_name(n)?,
        None => format!(
            "{}-{}",
            opts.preset.name(),
            &head_hash[..12.min(head_hash.len())]
        ),
    };
    let renders = target.dir.join(RENDERS_DIR);
    let final_path = renders.join(format!("{stem}.mp4"));
    let partial_path = crate::fsutil::ffmpeg_temp_path(&final_path)?;

    // Reverify every source this plan reads against its registered BLAKE3.
    let mut inputs: BTreeMap<SourceId, PathBuf> = BTreeMap::new();
    for input in &plan.inputs {
        inputs.insert(input.source.clone(), verify(&input.source)?);
    }
    // Per-source finishing chains, aligned to plan input order.
    let video_pre: Vec<Vec<String>> = plan
        .inputs
        .iter()
        .map(|i| opts.video_pre.get(&i.source).cloned().unwrap_or_default())
        .collect();

    let encoder = resolve_encoder(opts.encoder, tools, os)?;
    let text_dir = renders.join(".titles");
    let fit = opts
        .fit
        .clone()
        .unwrap_or_else(|| opts.preset.default_fit());
    // Caption burn-in: cues → drawtext filters on the composed output. Needs the bundled
    // font (same asset titles use); text files are written by `execute` with the titles.
    let mut captions = Vec::new();
    let mut caption_files: Vec<TextFile> = Vec::new();
    if !base_captions.is_empty() {
        let font = opts.font.as_ref().filter(|f| f.is_file()).ok_or_else(|| {
            ProjectError::MediaToolMissing(
                "caption burn-in needs the engine font (runtime/fonts)".into(),
            )
        })?;
        let assets = TitleAssets {
            font,
            text_dir: &text_dir,
        };
        let face = rightkit_captions::FontFace::parse(
            fs::read(font)
                .map_err(|e| ProjectError::io(format!("reading font {}", font.display()), e))?,
        )
        .map_err(|e| ProjectError::BadInput(e.to_string()))?;
        let (filters, files) = if let Some(style_id) = opts.caption_style.as_deref() {
            let styles = rightkit_captions::StyleCatalog::with_project(target.dir)
                .map_err(ProjectError::BadInput)?;
            let zones = rightkit_captions::SafeZoneCatalog::builtin();
            let style = styles.get(style_id).ok_or_else(|| {
                ProjectError::BadInput(format!("unknown caption_style {style_id:?}"))
            })?;
            let zone = zones.get(caption_zone_id(opts.preset)).ok_or_else(|| {
                ProjectError::BadInput(format!("no safe zone for preset {}", opts.preset.name()))
            })?;
            let (w, h) = (opts.preset.width() as i64, opts.preset.height() as i64);
            // Rebuild cues with style segmentation caps and zone fit, while
            // retaining timeline occurrence boundaries. `base_captions` was
            // built one clip occurrence at a time; rebuilding one shared word
            // list would let a short gap merge the end of one placement with
            // the start of the next.
            let mut cues = Vec::new();
            for base in &base_captions {
                let Some(words) = base_words.get(base.first_word..=base.last_word) else {
                    continue;
                };
                let offset = base.first_word;
                let mut styled = rightkit_captions::styled::build_styled_cues_with_font(
                    words,
                    style,
                    &styles,
                    zone,
                    w,
                    h,
                    Some(&face),
                )
                .map_err(ProjectError::BadInput)?;
                for cue in &mut styled {
                    cue.first_word += offset;
                    cue.last_word += offset;
                }
                cues.extend(styled);
            }
            let target = rightkit_captions::CaptionTarget {
                width: w,
                height: h,
                fps: plan.fps,
                total_frames: plan.total_frames,
            };
            rightkit_captions::styled::styled_caption_filters_with_font(
                &base_words,
                &cues,
                style,
                zone,
                &rightkit_captions::Emphasis::default(),
                &target,
                &assets,
                Some(&face),
            )?
        } else {
            rightkit_captions::burn::caption_filters_with_font(
                &base_captions,
                opts.preset,
                plan.fps,
                plan.total_frames,
                &assets,
                Some(&face),
            )?
        };
        captions = filters;
        caption_files = files;
    }
    let style = RenderStyle {
        fit: fit.clone(),
        audio: AudioFinish::None,
        video_pre: video_pre.clone(),
        captions: captions.clone(),
        video_post: opts.video_post.clone(),
        audio_post: opts.audio_post.clone(),
        encoder_args: opts.encoder_args.clone(),
    };
    let (command, mut text_files) = build_command(
        &RenderRequest {
            plan: &plan,
            preset: opts.preset,
            encoder,
            inputs: &inputs,
            output: &partial_path,
        },
        &opts.font,
        &text_dir,
        &style,
    )?;
    for f in caption_files {
        if !text_files.iter().any(|t| t.path == f.path) {
            text_files.push(f);
        }
    }

    let total_ms = plan.fps.frame_to_ticks(plan.total_frames).to_units(1000);
    Ok(Prepared {
        verified_sources: inputs.len(),
        text_files,
        captions,
        video_pre,
        video_post: opts.video_post.clone(),
        audio_post: opts.audio_post.clone(),
        encoder_args: opts.encoder_args.clone(),
        plan,
        command,
        encoder,
        preset: opts.preset,
        final_path,
        partial_path,
        head_hash,
        cursor: target.cursor,
        total_ms,
        fit,
        loudness: opts.loudness,
        origin: opts.origin.clone(),
        inputs,
        font: opts.font.clone(),
        text_dir,
    })
}

#[derive(Debug, Clone)]
pub struct RenderReport {
    pub output: PathBuf,
    pub bytes: u64,
    pub run: RunReport,
    /// `None` when loudness normalisation is off.
    pub loudness: Option<LoudnessReport>,
    /// `renders/<stem>.mp4.render.json` — hash, revision and source↔output map.
    pub record_path: PathBuf,
}

/// Runs a prepared render. On success the partial file is renamed to its final name;
/// on any failure the partial file is removed.
pub fn execute(
    prepared: &Prepared,
    tools: &MediaTools,
    opts: &RenderOptions,
    on_event: &mut dyn FnMut(&ProgressEvent),
) -> Result<RenderReport, ProjectError> {
    if let Some(dir) = prepared.partial_path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| ProjectError::io(format!("creating {}", dir.display()), e))?;
        rightkit_fs::sweep_stale_temps(dir, Duration::ZERO)
            .map_err(|e| ProjectError::io(format!("sweeping {}", dir.display()), e.into()))?;
    }
    let _ = fs::remove_file(&prepared.partial_path);
    for f in &prepared.text_files {
        if let Some(dir) = f.path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| ProjectError::io(format!("creating {}", dir.display()), e))?;
        }
        crate::fsutil::atomic_write(&f.path, f.content.as_bytes())?;
    }
    // Loudness pass 1: measure the composed mix; then render with the linear second pass.
    let mut command = prepared.command.clone();
    let mut pending: Option<(rightkit_ffmpeg::loudnorm::LoudnessTarget, Option<_>)> = None;
    if let LoudnessSetting::Normalize(target) = prepared.loudness {
        let measure = build_ffmpeg_measure(
            &RenderRequest {
                plan: &prepared.plan,
                preset: prepared.preset,
                encoder: prepared.encoder,
                inputs: &prepared.inputs,
                output: &prepared.partial_path,
            },
            &target,
            &prepared.audio_post,
        )?;
        let measured =
            loudness::measure_command(&tools.ffmpeg, &measure, opts.timeout, &opts.cancel)
                .inspect_err(|_| {
                    let _ = fs::remove_file(&prepared.partial_path);
                })?;
        if measured.is_silent() {
            pending = Some((target, None));
        } else {
            let style = RenderStyle {
                fit: prepared.fit.clone(),
                audio: AudioFinish::Normalize { target, measured },
                video_pre: prepared.video_pre.clone(),
                captions: prepared.captions.clone(),
                video_post: prepared.video_post.clone(),
                audio_post: prepared.audio_post.clone(),
                encoder_args: prepared.encoder_args.clone(),
            };
            command = build_command(
                &RenderRequest {
                    plan: &prepared.plan,
                    preset: prepared.preset,
                    encoder: prepared.encoder,
                    inputs: &prepared.inputs,
                    output: &prepared.partial_path,
                },
                &prepared.font,
                &prepared.text_dir,
                &style,
            )?
            .0;
            pending = Some((target, Some(measured)));
        }
    }
    let result = run_ffmpeg(
        &tools.ffmpeg,
        &command,
        RunOptions {
            timeout: opts.timeout,
            cancel: Arc::clone(&opts.cancel),
            on_progress: on_event,
        },
    );
    let run = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = fs::remove_file(&prepared.partial_path);
            return Err(e);
        }
    };
    let bytes = finalize(&prepared.partial_path, &prepared.final_path)?;
    // Loudness pass 3: measure what was delivered, so the report states the real result.
    let loudness = match pending {
        None => None,
        Some((target, None)) => Some(loudness::report(target, None, None)),
        Some((target, Some(measured))) => {
            let after = loudness::measure_file(
                &tools.ffmpeg,
                &prepared.final_path,
                &target,
                opts.timeout,
                &opts.cancel,
            )?;
            let report = loudness::report(target, Some(measured), Some(after));
            // Delivered loudness is verified, not assumed: outside tolerance is a
            // render failure, never a silent degrade. One exception: for high-crest
            // material `loudnorm` correctly undershoots integrated to satisfy the
            // true-peak ceiling — that is mastering working, not a miss. Undershoot
            // is only accepted when the delivered peak sits at the ceiling; anything
            // else (overshoot, TP breach, or low level with peak headroom) fails.
            let tp_bound_undershoot = after.input_i
                < target.integrated_lufs - crate::loudness::TOLERANCE_LU
                && after.input_tp <= -1.0
                && after.input_tp >= target.true_peak_db - 0.6;
            if !report.within_tolerance && !tp_bound_undershoot {
                return Err(ProjectError::RenderFailed {
                    message: format!(
                        "delivered loudness out of tolerance: {:.2} LUFS / {:.2} dBTP \
                         (target {:.1} LUFS ± {:.1}, TP <= -1.0)",
                        after.input_i,
                        after.input_tp,
                        target.integrated_lufs,
                        crate::loudness::TOLERANCE_LU,
                    ),
                });
            }
            Some(report)
        }
    };
    let record_path = write_render_record(prepared, opts, &loudness, bytes)?;
    Ok(RenderReport {
        output: prepared.final_path.clone(),
        bytes,
        run,
        loudness,
        record_path,
    })
}

/// Source↔output rows of the render record (`cutright.render.v1`), in plan order.
fn time_map_json(plan: &RenderPlan) -> Vec<serde_json::Value> {
    use serde_json::json;
    let mut out = Vec::new();
    let mut off = 0i64;
    for seg in &plan.video {
        let frames = seg.frames();
        let out_end = off + plan.fps.frame_to_ticks(frames).to_units(1000);
        if let VideoSegment::Clip {
            clip_id,
            input,
            source_in,
            ..
        } = seg
        {
            out.push(json!({
                "kind": "video",
                "clip_id": clip_id,
                "source": plan.inputs[*input].source.as_str(),
                "source_in_ms": source_in.to_units(1000),
                "source_out_ms": source_in.to_units(1000)
                    + plan.fps.frame_to_ticks(frames).to_units(1000),
                "out_in_ms": off,
                "out_end_ms": out_end,
            }));
        }
        off = out_end;
    }
    for layer in &plan.audio.layers {
        out.push(json!({
            "kind": "audio",
            "clip_id": layer.id,
            "source": plan.inputs[layer.input].source.as_str(),
            "source_in_ms": layer.source_in.to_units(1000),
            "source_out_ms": rightkit_timeline::Ticks(layer.source_in.0 + layer.duration.0)
                .to_units(1000),
            "out_in_ms": layer.start.to_units(1000),
            "out_end_ms": rightkit_timeline::Ticks(layer.start.0 + layer.duration.0)
                .to_units(1000),
        }));
    }
    out
}

fn record_path_for(final_path: &Path) -> PathBuf {
    let mut p = final_path.as_os_str().to_owned();
    p.push(".render.json");
    PathBuf::from(p)
}

/// Stable finishing identity for restart-safe reuse. Excludes job ids and
/// output paths, but includes framing, mapped captions and mastering options.
pub fn finishing_signature(opts: &RenderOptions) -> Result<String, ProjectError> {
    let value = serde_json::json!({
        "font": font_binding(opts)?,
        "preset": opts.preset.name(),
        "fit": opts.fit,
        "loudness": format!("{:?}", opts.loudness),
        "captions": opts.captions,
        "caption_style": opts.caption_style,
        "caption_words": opts.caption_words,
        "video_pre": opts.video_pre,
        "video_post": opts.video_post,
        "audio_post": opts.audio_post,
        "music_duck_windows": opts.music_duck_windows,
        "encoder_args": opts.encoder_args,
    });
    let bytes = serde_json::to_vec(&value)
        .map_err(|e| ProjectError::BadInput(format!("finishing signature: {e}")))?;
    Ok(rightkit_fs::digest::blake3_bytes(&bytes))
}

/// The font a render burns with, as path plus BLAKE3 of its bytes; `null` when none is set.
/// Both enter the finishing signature and the render record, so a different font face
/// is a different render even when the path stays the same.
pub fn font_binding(opts: &RenderOptions) -> Result<serde_json::Value, ProjectError> {
    let Some(font) = opts.font.as_ref() else {
        return Ok(serde_json::Value::Null);
    };
    let digest = rightkit_fs::digest::blake3_file(font)
        .map_err(|e| ProjectError::io(format!("hashing font {}", font.display()), e))?;
    Ok(serde_json::json!({
        "path": font.display().to_string(),
        "blake3": digest,
    }))
}

/// Writes `renders/<stem>.mp4.render.json`: export hash, rendered revision and the
/// source↔output time map so retention can bind to exactly this file.
fn write_render_record(
    prepared: &Prepared,
    opts: &RenderOptions,
    loudness: &Option<LoudnessReport>,
    bytes: u64,
) -> Result<PathBuf, ProjectError> {
    let output_blake3 = rightkit_fs::digest::blake3_file(&prepared.final_path)
        .map_err(|e| ProjectError::io(format!("hashing {}", prepared.final_path.display()), e))?;
    let stem = prepared
        .final_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let mut origin = serde_json::to_value(&opts.origin)
        .map_err(|e| ProjectError::BadInput(format!("render origin: {e}")))?;
    origin["timeline_head_hash"] = serde_json::json!(prepared.head_hash);
    origin["timeline_cursor"] = serde_json::json!(prepared.cursor);
    let record = serde_json::json!({
        "schema": "cutright.render.v1",
        "name": stem,
        "output": prepared.final_path.display().to_string(),
        "output_blake3": output_blake3,
        "bytes": bytes,
        "preset": prepared.preset.name(),
        "encoder": encoder_name(prepared.encoder),
        "duration_ms": prepared.total_ms,
        "frames": prepared.plan.total_frames,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "origin": origin,
        "loudness": loudness,
        "finishing_signature": finishing_signature(opts)?,
        "font": font_binding(opts)?,
        "time_map": time_map_json(&prepared.plan),
    });
    let path = record_path_for(&prepared.final_path);
    let mut text = serde_json::to_vec_pretty(&record)
        .map_err(|e| ProjectError::BadInput(format!("render record: {e}")))?;
    text.push(b'\n');
    crate::fsutil::atomic_write(&path, &text)?;
    Ok(path)
}

fn finalize(partial: &Path, dest: &Path) -> Result<u64, ProjectError> {
    let bytes = fs::metadata(partial).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        let _ = fs::remove_file(partial);
        return Err(ProjectError::RenderFailed {
            message: "ffmpeg reported success but produced no output".into(),
        });
    }
    rightkit_fs::publish_file(partial, dest).map_err(|e| {
        let _ = fs::remove_file(partial);
        ProjectError::io(
            format!("renaming render to {}", dest.display()),
            std::io::Error::other(e),
        )
    })?;
    Ok(bytes)
}

/// Room-tone bed for the plan's cut gaps. Each gap takes the tone of its nearest preceding
/// layer's source. A source's tone is captured once and cached under `analysis` as
/// `room-tone-<source id>.json`; the bed is written as `room-tone-bed-<hash>.wav`.
fn attach_room_tone(
    plan: &mut RenderPlan,
    tools: &MediaTools,
    analysis: &Path,
    verify: &dyn Fn(&SourceId) -> Result<PathBuf, ProjectError>,
) -> Result<(), ProjectError> {
    use rightkit_audio::room_tone::RoomToneParams;
    use rightkit_audio::{encode_mono_f32_wav, room_tone_bed, FillParams, ToneFill};
    use rightkit_timeline::plan::{room_tone_gaps, RoomToneBed};

    const RATE: u32 = 16_000;
    let gaps = room_tone_gaps(&plan.audio);
    if gaps.is_empty() {
        return Ok(());
    }
    let binary = rk::verify_ffmpeg_handle(
        &tools.ffmpeg,
        rk::EXPORT_ASS_ZIMG_VARIANT,
        crate::tools::VERIFY_TIMEOUT,
    )
    .or_else(|_| {
        rk::verify_ffmpeg_handle(
            &tools.ffmpeg,
            rk::EXPORT_ASS_VARIANT,
            crate::tools::VERIFY_TIMEOUT,
        )
    })
    .map_err(|e| ProjectError::MediaToolMissing(e.to_string()))?;
    let audio = rightkit_audio::AudioTools::new(&binary);
    let audio_err = |e: rightkit_audio::AudioError| {
        ProjectError::AudioSignalUnavailable(format!("room tone: {e}"))
    };
    let mut tones: BTreeMap<usize, Vec<f32>> = BTreeMap::new();
    for gap in &gaps {
        if tones.contains_key(&gap.input) {
            continue;
        }
        let source = plan.inputs[gap.input].source.clone();
        let path = verify(&source)?;
        let cache = analysis.join(format!("room-tone-{}.json", source.as_str()));
        let window = match read_tone_window(&cache) {
            Some(window) => window,
            None => {
                let sample = audio
                    .capture_room_tone(&path, &[], &RoomToneParams::default())
                    .map_err(audio_err)?;
                let window = (sample.start_ms, sample.start_ms + sample.duration_ms);
                fs::create_dir_all(analysis).map_err(|e| {
                    ProjectError::BadInput(format!("cannot create {}: {e}", analysis.display()))
                })?;
                let bytes = serde_json::to_vec(&serde_json::json!({
                    "start_ms": window.0,
                    "end_ms": window.1,
                }))
                .map_err(|e| ProjectError::BadInput(e.to_string()))?;
                crate::fsutil::atomic_write(&cache, &bytes)?;
                window
            }
        };
        // The decode window is (start_ms, duration_ms); the cache stores (start, end).
        let tone = audio
            .decode_mono_f32(&path, RATE, Some((window.0, window.1 - window.0)), None)
            .map_err(audio_err)?;
        tones.insert(gap.input, tone);
    }
    let fills: Vec<ToneFill<'_>> = gaps
        .iter()
        .map(|gap| ToneFill {
            start: gap.start.to_units(RATE as i64) as usize,
            len: gap.duration.to_units(RATE as i64) as usize,
            tone: &tones[&gap.input],
        })
        .collect();
    let total = plan.audio.total.to_units(RATE as i64) as usize;
    let bed = room_tone_bed(total, RATE, &fills, &FillParams::default())
        .map_err(|e| ProjectError::AudioSignalUnavailable(format!("room tone: {e}")))?;
    let bytes = encode_mono_f32_wav(&bed, RATE);
    let hash = rightkit_fs::digest::blake3_bytes(&bytes);
    let file = analysis.join(format!("room-tone-bed-{}.wav", &hash.as_str()[..16]));
    crate::fsutil::atomic_write(&file, &bytes)?;
    plan.audio.room_tone = Some(RoomToneBed { file });
    Ok(())
}

/// The cached capture window `(start_ms, end_ms)`, or `None` when absent or unreadable.
fn read_tone_window(cache: &Path) -> Option<(i64, i64)> {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(cache).ok()?).ok()?;
    Some((
        value.get("start_ms")?.as_i64()?,
        value.get("end_ms")?.as_i64()?,
    ))
}
