//! FFmpeg argument builder: [`RenderPlan`] + preset + encoder + input paths -> typed args.
//!
//! Nothing here executes FFmpeg or builds a shell string. The caller (the media
//! worker) resolves each source id to a file path it has re-verified against the
//! registered BLAKE3, and runs the returned argument vector without a shell.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rightkit_ffmpeg::loudnorm::{loudnorm_filter, LoudnessMeasurement, LoudnessTarget};
use rightkit_ffmpeg::FfmpegCommand;
use serde::{Deserialize, Serialize};

use crate::error::TimelineError;
use crate::model::{CardSlot, ClipKey, ClipVisualKeys, KeyedCrop, SourceId};
use crate::plan::{Attenuation, AudioLayer, CardSpan, PlanInput, RenderPlan, VideoSegment};
use crate::time::{Fps, Ticks};
use crate::title::{title_filters, TextFile, TitleAssets};

/// Output preset. The fit policy ([`FitPolicy`]) decides how a source that does not
/// match the preset's aspect ratio is fitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// 16:9, 1920x1080.
    YouTube,
    /// 9:16, 1080x1920.
    Reels,
    /// 9:16, 1080x1920; larger bottom and right-rail UI zones than Reels.
    #[serde(rename = "tiktok")]
    TikTok,
    /// 1:1, 1080x1080.
    Square,
}

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Preset::YouTube => "youtube",
            Preset::Reels => "reels",
            Preset::TikTok => "tiktok",
            Preset::Square => "square",
        }
    }

    pub fn width(self) -> u32 {
        match self {
            Preset::YouTube => 1920,
            Preset::Reels | Preset::TikTok | Preset::Square => 1080,
        }
    }

    pub fn height(self) -> u32 {
        match self {
            Preset::YouTube => 1080,
            Preset::Reels | Preset::TikTok => 1920,
            Preset::Square => 1080,
        }
    }

    pub fn video_kbps(self) -> u32 {
        match self {
            Preset::YouTube => 12_000,
            Preset::Reels | Preset::TikTok | Preset::Square => 8_000,
        }
    }

    pub fn audio_kbps(self) -> u32 {
        192
    }

    /// Fit used when the caller names none: YouTube letterboxes; Reels (talking-head
    /// vertical) centre-crops so there are no black bars.
    pub fn default_fit(self) -> FitPolicy {
        match self {
            Preset::YouTube => FitPolicy::Letterbox,
            Preset::Reels | Preset::TikTok | Preset::Square => FitPolicy::CenterCrop,
        }
    }
}

/// One crop-centre anchor on the output timeline, in thousandths of the source frame
/// (`0..=1000`, top-left origin). Integers keep the policy `Eq` and the JSON exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropAnchor {
    pub start_ms: i64,
    pub center_x_milli: i32,
    pub center_y_milli: i32,
}

/// Smoothed crop path for ONE clip occurrence (see `cutright-project`'s
/// `reframe_track`). `start_ms` is SOURCE time of that clip's source, so each
/// clip samples only its own source's approved plan: interpolation never
/// crosses a clip or source boundary. Anchors are sorted by `start_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceTrack {
    pub anchors: Vec<CropAnchor>,
}

/// The per-clip-occurrence crop paths of a `FaceCenter` render: `clips[clip_id]`
/// holds the approved plan anchors (source milliseconds) for that clip. A clip
/// absent from the map falls back to a centred crop — its source already
/// matches the target aspect or is audio-described only.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FaceTracks {
    pub clips: BTreeMap<String, FaceTrack>,
}

/// How a source is fitted into the preset frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fit", rename_all = "snake_case")]
pub enum FitPolicy {
    /// Scale to fit inside the frame and pad the rest with black.
    Letterbox,
    /// Crop the source to the frame's aspect ratio around its centre, then scale.
    CenterCrop,
    /// Crop around a moving face-centre path. Each clip occurrence gets its own
    /// source-time track (see [`FaceTracks`]); interpolation is isolated to that
    /// clip's source and the first frame samples the track at `source_in`.
    FaceCenter(FaceTracks),
}

/// What the final audio chain does after the mix.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum AudioFinish {
    /// Mix only (no normalisation).
    #[default]
    None,
    /// Second `loudnorm` pass with the first pass's measurement, `linear=true`.
    Normalize {
        target: LoudnessTarget,
        measured: LoudnessMeasurement,
    },
}

/// Per-render choices beyond the preset.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderStyle {
    pub fit: FitPolicy,
    pub audio: AudioFinish,
    /// Per-input finishing filters applied inside each clip chain after trim/fps
    /// and before fit (per-source colour normalisation: mixed HDR/SDR is unified
    /// before composition). `video_pre[i]` applies to plan input `i`.
    pub video_pre: Vec<Vec<String>>,
    /// Extra video filters drawn on the composed output after titles (caption burn-in).
    pub captions: Vec<String>,
    /// Finishing filters on the composed video, after captions (output tagging).
    /// Pixel-affecting colour work belongs in `video_pre` so captions and titles
    /// are never recoloured.
    pub video_post: Vec<String>,
    /// Finishing filters on the composed mix BEFORE loudness mastering
    /// (dialogue cleanup/compression); loudnorm always runs last.
    pub audio_post: Vec<String>,
    /// Extra encoder/muxer args appended before the output (e.g. colour tags).
    pub encoder_args: Vec<String>,
}

impl RenderStyle {
    pub fn for_preset(preset: Preset) -> Self {
        Self {
            fit: preset.default_fit(),
            audio: AudioFinish::None,
            video_pre: Vec::new(),
            captions: Vec::new(),
            video_post: Vec::new(),
            audio_post: Vec::new(),
            encoder_args: Vec::new(),
        }
    }
}

/// H.264 encoder chosen by the caller (platform and availability are its decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoder {
    VideoToolbox,
    Nvenc,
    /// Windows Media Foundation (`h264_mf`): the system encoder, hardware when present.
    #[serde(rename = "mf")]
    MediaFoundation,
}

impl Encoder {
    /// The host operating system's own H.264 encoder, with no check that this ffmpeg
    /// lists it or that it runs. For callers that need a fixed encoder without probing;
    /// a render resolves its encoder through the project crate instead.
    pub const fn host_system() -> Self {
        if cfg!(target_os = "macos") {
            Encoder::VideoToolbox
        } else {
            Encoder::MediaFoundation
        }
    }
}

pub struct RenderRequest<'a> {
    pub plan: &'a RenderPlan,
    pub preset: Preset,
    pub encoder: Encoder,
    /// Verified file for every source the plan uses.
    pub inputs: &'a BTreeMap<SourceId, PathBuf>,
    pub output: &'a Path,
}

/// A built render: the command plus the title text files it reads. The caller
/// must write every [`TextFile`] (exact content) before running the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltRender {
    pub command: FfmpegCommand,
    pub text_files: Vec<TextFile>,
}

/// Seconds with microsecond precision, via the shared rounding rule.
fn seconds(t: Ticks) -> String {
    let micros = t.to_units(1_000_000);
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

struct Graph {
    chains: Vec<String>,
}

impl Graph {
    fn push(&mut self, inputs: &[String], filters: Vec<String>, output: &str) {
        let mut s = String::new();
        for i in inputs {
            s.push_str(&format!("[{i}]"));
        }
        s.push_str(&filters.join(","));
        s.push_str(&format!("[{output}]"));
        self.chains.push(s);
    }

    fn render(&self) -> String {
        self.chains.join(";")
    }
}

/// Builds the FFmpeg command for a plan without titles. A plan with titles needs
/// [`build_ffmpeg_render`] (font and text files); this refuses it rather than
/// silently dropping the titles.
pub fn build_ffmpeg_command(req: &RenderRequest<'_>) -> Result<FfmpegCommand, TimelineError> {
    if !req.plan.overlays.is_empty() {
        return Err(TimelineError::Unsupported(
            "the plan has titles: use build_ffmpeg_render with TitleAssets".into(),
        ));
    }
    Ok(build_ffmpeg_render(req, None)?.command)
}

/// Builds the FFmpeg command for `req`; `titles` is required when the plan has
/// overlay titles (they become `drawtext` filters on the composed video).
pub fn build_ffmpeg_render(
    req: &RenderRequest<'_>,
    titles: Option<&TitleAssets<'_>>,
) -> Result<BuiltRender, TimelineError> {
    build_ffmpeg_render_styled(req, titles, &RenderStyle::for_preset(req.preset))
}

/// Like [`build_ffmpeg_render`] with an explicit fit policy and audio finish.
pub fn build_ffmpeg_render_styled(
    req: &RenderRequest<'_>,
    titles: Option<&TitleAssets<'_>>,
    style: &RenderStyle,
) -> Result<BuiltRender, TimelineError> {
    let plan = req.plan;
    let assets = match (plan.overlays.is_empty(), titles) {
        (true, _) => None,
        (false, Some(a)) => Some(a),
        (false, None) => {
            return Err(TimelineError::Unsupported(
                "the plan has titles but no title font/text directory was supplied".into(),
            ))
        }
    };
    let (w, h) = (req.preset.width(), req.preset.height());
    let fps = plan.fps.ffmpeg_rational();

    // Which inputs are used, in plan order, and how often each stream is consumed.
    let n_inputs = plan.inputs.len();
    let mut video_uses = vec![0usize; n_inputs];
    let mut audio_uses = vec![0usize; n_inputs];
    for seg in &plan.video {
        if let VideoSegment::Clip { input, .. } = seg {
            video_uses[*input] += 1;
        }
    }
    for card in &plan.cards {
        if let Some(fill) = &card.fill {
            video_uses[fill.input] += 1;
        }
    }
    for layer in &plan.audio.layers {
        audio_uses[layer.input] += 1;
    }

    let mut cmd = FfmpegCommand::default();
    cmd.texts([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "warning",
        "-nostats",
        "-progress",
        "pipe:1",
    ]);
    let mut ffmpeg_index: Vec<Option<usize>> = vec![None; n_inputs];
    let mut next = 0;
    for (i, input) in plan.inputs.iter().enumerate() {
        if video_uses[i] + audio_uses[i] == 0 {
            continue;
        }
        let path = req
            .inputs
            .get(&input.source)
            .ok_or_else(|| TimelineError::MissingInputPath(input.source.to_string()))?;
        cmd.text("-i");
        cmd.path(path);
        ffmpeg_index[i] = Some(next);
        next += 1;
    }

    let bed_index = push_room_tone_input(&mut cmd, plan, next);
    let mut g = Graph { chains: Vec::new() };

    // Video: split shared inputs, trim/resample/fit each clip, concat, hold.
    let mut video_taps: Vec<Vec<String>> = vec![Vec::new(); n_inputs];
    for (i, uses) in video_uses.iter().enumerate() {
        let Some(fi) = ffmpeg_index[i] else { continue };
        match *uses {
            0 => {}
            1 => video_taps[i].push(format!("{fi}:v")),
            n => {
                let labels: Vec<String> = (0..n).map(|k| format!("vs{fi}_{k}")).collect();
                let out = labels.iter().map(|l| format!("[{l}]")).collect::<String>();
                g.chains.push(format!("[{fi}:v]split={n}{out}"));
                video_taps[i] = labels;
            }
        }
    }
    // `fit_filter` has no per-clip context; FaceCenter is resolved per segment below.
    let shared_fit = match &style.fit {
        FitPolicy::FaceCenter(_) => None,
        other => Some(fit_filter(other, w, h)?),
    };
    let mut text_files: Vec<TextFile> = Vec::new();
    let mut seg_labels = Vec::new();
    // Per concatenated segment: its frame count and its cross-fade-in length (0 = cut).
    let mut seg_frames: Vec<i64> = Vec::new();
    let mut seg_fades: Vec<i64> = Vec::new();
    let mut seg_handles: Vec<i64> = Vec::new();
    let mut hold_frames = 0;
    for (k, seg) in plan.video.iter().enumerate() {
        let label = format!("v{k}");
        match seg {
            VideoSegment::Clip {
                clip_id,
                input,
                source_in,
                frames,
                visuals,
            } => {
                let fade = plan.video.get(k + 1).map_or(0, |next| match next {
                    VideoSegment::Clip {
                        frames, visuals, ..
                    } => visuals
                        .as_ref()
                        .map_or(0, |keys| keys.cross_fade_in_frames)
                        .min(*frames),
                    _ => 0,
                });
                let handle = if fade > 1
                    && plan.inputs[*input].duration.0
                        >= source_in.0
                            + plan.fps.frame_to_ticks(*frames).0
                            + plan.fps.frame_to_ticks(fade).0
                {
                    fade
                } else {
                    0
                };
                let render_frames = *frames + handle;
                let tap = video_taps[*input].remove(0);
                let mut chain = vec![
                    format!("trim=start={}", seconds(*source_in)),
                    "setpts=PTS-STARTPTS".into(),
                    format!("fps={fps}"),
                    // VFR or stream-short sources can leave `fps` one frame
                    // shy at EOF; clone-pad so every clip yields exactly
                    // `frames` and video length always matches the plan.
                    format!("tpad=stop_mode=clone:stop={render_frames}"),
                    format!("trim=end_frame={render_frames}"),
                ];
                // Per-source normalisation (e.g. HDR/Apple Log -> bt709) happens
                // inside the clip chain, before fit and before composition, so
                // captions/titles drawn later are never recoloured.
                if let Some(pre) = style.video_pre.get(*input) {
                    chain.extend(pre.iter().cloned());
                }
                // Keyed crop samples the source frame, so it runs before the fit.
                if let Some(keys) = visuals.as_ref().filter(|k| !k.crop.is_empty()) {
                    chain.push(crop_filter(&keys.crop));
                }
                match &style.fit {
                    FitPolicy::FaceCenter(tracks) => {
                        // A clip without a track holds still on the centre of its
                        // source — same labelled crop shape, no command file.
                        static EMPTY_TRACK: FaceTrack = FaceTrack {
                            anchors: Vec::new(),
                        };
                        let track = tracks.clips.get(clip_id).unwrap_or(&EMPTY_TRACK);
                        let (filters, cmd_file) = face_clip_chain(
                            k,
                            track,
                            &plan.inputs[*input],
                            *source_in,
                            *frames,
                            plan.fps,
                            w,
                            h,
                            req.output,
                        )?;
                        chain.extend(filters);
                        if let Some(t) = cmd_file {
                            text_files.push(t);
                        }
                    }
                    _ => chain.push(shared_fit.clone().expect("non-FaceCenter fit")),
                }
                match visuals {
                    Some(keys) if !keys.transform.is_empty() || !keys.opacity.is_empty() => {
                        keyed_overlay(
                            &mut g,
                            tap,
                            chain,
                            keys,
                            plan.fps,
                            (w, h),
                            render_frames,
                            &label,
                        );
                    }
                    _ => g.push(&[tap], chain, &label),
                }
                seg_labels.push(label);
                seg_frames.push(*frames);
                seg_fades.push(visuals.as_ref().map_or(0, |k| k.cross_fade_in_frames));
                seg_handles.push(handle);
            }
            VideoSegment::Black { frames } => {
                g.chains.push(format!(
                    "color=c=black:s={w}x{h}:r={fps},trim=end_frame={frames},setsar=1,format=yuv420p[{label}]"
                ));
                seg_labels.push(label);
                seg_frames.push(*frames);
                seg_fades.push(0);
                seg_handles.push(0);
            }
            VideoSegment::Hold { frames } => {
                if k + 1 != plan.video.len() || seg_labels.is_empty() {
                    return Err(TimelineError::Unsupported(
                        "a hold is only valid as the final video segment".into(),
                    ));
                }
                hold_frames = *frames;
            }
        }
    }
    let joined = if seg_labels.len() == 1 {
        seg_labels[0].clone()
    } else if seg_fades.iter().any(|n| *n > 0) {
        join_with_fades(
            &mut g,
            &seg_labels,
            &seg_frames,
            &seg_fades,
            &seg_handles,
            plan.fps,
        )
    } else {
        let ins: Vec<String> = seg_labels.clone();
        g.push(
            &ins,
            vec![format!("concat=n={}:v=1:a=0", seg_labels.len())],
            "vcat",
        );
        "vcat".to_string()
    };
    // Titles are drawn on the composed output, so the base chain gets its own label.
    let base = if assets.is_some() { "vbase" } else { "vout" };
    // With cards, the programme keeps its own label and the cards are stitched around it.
    let programme = if plan.cards.is_empty() { base } else { "vprog" };
    if hold_frames > 0 {
        g.push(
            &[joined],
            vec![format!("tpad=stop_mode=clone:stop={hold_frames}")],
            programme,
        );
    } else {
        g.push(&[joined], vec!["null".into()], programme);
    }
    if !plan.cards.is_empty() {
        let mut ins: Vec<String> = Vec::new();
        for (i, card) in plan.cards.iter().enumerate() {
            if card.slot == CardSlot::Opening {
                let label = format!("card{i}");
                push_card(
                    &mut g,
                    &mut video_taps,
                    &style.video_pre,
                    card,
                    i,
                    (w, h),
                    &fps,
                    &label,
                );
                ins.push(label);
            }
        }
        ins.push(programme.to_string());
        for (i, card) in plan.cards.iter().enumerate() {
            if card.slot == CardSlot::Ending {
                let label = format!("card{i}");
                push_card(
                    &mut g,
                    &mut video_taps,
                    &style.video_pre,
                    card,
                    i,
                    (w, h),
                    &fps,
                    &label,
                );
                ins.push(label);
            }
        }
        g.push(&ins, vec![format!("concat=n={}:v=1:a=0", ins.len())], base);
    }
    if let Some(assets) = assets {
        let mut filters = Vec::new();
        for item in &plan.overlays {
            let (f, files) = title_filters(item, req.preset, plan.total_frames, assets)?;
            filters.extend(f);
            for file in files {
                if !text_files.iter().any(|t| t.path == file.path) {
                    text_files.push(file);
                }
            }
        }
        if filters.is_empty() {
            filters.push("null".into());
        }
        g.push(&[base.to_string()], filters, "vout");
    }
    // Caption burn-in draws on the composed output, then colour/finishing last.
    let mut video_label = if style.captions.is_empty() {
        "vout".to_string()
    } else {
        g.push(&["vout".to_string()], style.captions.clone(), "vcap");
        "vcap".to_string()
    };
    if !style.video_post.is_empty() {
        g.push(&[video_label], style.video_post.clone(), "vpost");
        video_label = "vpost".to_string();
    }

    let rate = plan.audio.sample_rate;
    // Dialogue cleanup/compression runs BEFORE final loudness mastering so the
    // measured pass sees the same signal the listener gets; the room-tone bed joins
    // after the cleanup (see `audio_graph`).
    let mut audio_label = audio_graph(
        &mut g,
        plan,
        &ffmpeg_index,
        &audio_uses,
        bed_index,
        &style.audio_post,
    );
    if let AudioFinish::Normalize { target, measured } = style.audio {
        g.push(
            &[audio_label],
            vec![
                loudnorm_filter(&target, Some(&measured), "summary"),
                format!("aresample={rate}"),
            ],
            "aln",
        );
        audio_label = "aln".to_string();
    }

    cmd.text("-filter_complex");
    cmd.text(g.render());
    cmd.text("-map");
    cmd.text(format!("[{video_label}]"));
    cmd.text("-map");
    cmd.text(format!("[{audio_label}]"));
    encoder_args(&mut cmd, req.encoder, req.preset, plan);
    for a in &style.encoder_args {
        cmd.text(a.clone());
    }
    cmd.texts(["-pix_fmt", "yuv420p", "-r"]);
    cmd.text(fps);
    cmd.texts(["-c:a", "aac", "-b:a"]);
    cmd.text(format!("{}k", req.preset.audio_kbps()));
    cmd.texts(["-ar"]);
    cmd.text(rate.to_string());
    cmd.texts(["-ac", "2"]);
    // Bound the container to the planned length: loudnorm/cleanup filters can
    // flush a tail past the last video frame (seen as +67ms audio on shorts).
    cmd.text("-t");
    cmd.text(seconds(plan.audio.total));
    cmd.texts(["-movflags", "+faststart"]);
    cmd.path(req.output);
    Ok(BuiltRender {
        command: cmd,
        text_files,
    })
}

/// Clip-local seconds of a key time (microsecond precision, like [`seconds`]).
fn key_seconds(t: Ticks) -> f64 {
    t.to_units(1_000_000) as f64 / 1_000_000.0
}

/// Piecewise-linear FFmpeg expression of `var` (seconds) through `points` (seconds,
/// value), held at the first value before the first key and the last after the last.
/// Every value is parenthesised so negative literals stay unambiguous.
fn keyed_expr(points: &[(f64, f64)], var: &str) -> String {
    let (first_t, first_v) = points[0];
    let (_, last_v) = points[points.len() - 1];
    let mut expr = format!("({last_v:.6})");
    for pair in points.windows(2).rev() {
        let (a, va) = pair[0];
        let (b, vb) = pair[1];
        expr = format!(
            "if(lt({var},{b:.6}),({va:.6})+(({vb:.6})-({va:.6}))*({var}-{a:.6})/({b:.6}-{a:.6}),{expr})"
        );
    }
    format!("if(lt({var},{first_t:.6}),({first_v:.6}),{expr})")
}

/// Keyed crop of the source frame. Permille windows become even pixel values, so the
/// yuv420 chroma stays aligned; the expressions are evaluated per frame in `t`.
fn crop_filter(keys: &[ClipKey<KeyedCrop>]) -> String {
    let track = |field: fn(&KeyedCrop) -> i64| -> String {
        let points: Vec<(f64, f64)> = keys
            .iter()
            .map(|k| (key_seconds(k.at), field(&k.value) as f64))
            .collect();
        keyed_expr(&points, "t")
    };
    format!(
        "crop=w='trunc(iw*({})/2000)*2':h='trunc(ih*({})/2000)*2':x='trunc(iw*({})/2000)*2':y='trunc(ih*({})/2000)*2'",
        track(|c: &KeyedCrop| c.width),
        track(|c: &KeyedCrop| c.height),
        track(|c: &KeyedCrop| c.x),
        track(|c: &KeyedCrop| c.y),
    )
}

/// A clip with keyed transform or opacity. Its chain scales the fitted frame by the
/// keyed scale, takes the keyed opacity as alpha (yuva444p + `geq`), and overlays the
/// result on black at the keyed offset, so the keys blend against black exactly as
/// the preview's composition does. Offsets are canvas fractions (permille / 1000).
#[allow(clippy::too_many_arguments)]
fn keyed_overlay(
    g: &mut Graph,
    tap: String,
    mut chain: Vec<String>,
    keys: &ClipVisualKeys,
    fps: Fps,
    (w, h): (u32, u32),
    frames: i64,
    label: &str,
) {
    let clip_label = format!("{label}_clip");
    let mut x = String::from("0");
    let mut y = String::from("0");
    // Opacity runs before the keyed scale: `geq` sizes its output once, from its first
    // frame, so after an `eval=frame` scale it would pin every frame to the first size.
    if !keys.opacity.is_empty() {
        let points: Vec<(f64, f64)> = keys
            .opacity
            .iter()
            .map(|k| (key_seconds(k.at), k.value as f64 / 1000.0))
            .collect();
        chain.push("format=yuva444p".into());
        chain.push(format!(
            "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({})'",
            keyed_expr(&points, "T")
        ));
    }
    if !keys.transform.is_empty() {
        let scale: Vec<(f64, f64)> = keys
            .transform
            .iter()
            .map(|k| (key_seconds(k.at), k.value.scale as f64 / 1000.0))
            .collect();
        let dx: Vec<(f64, f64)> = keys
            .transform
            .iter()
            .map(|k| (key_seconds(k.at), k.value.dx as f64 / 1000.0))
            .collect();
        let dy: Vec<(f64, f64)> = keys
            .transform
            .iter()
            .map(|k| (key_seconds(k.at), k.value.dy as f64 / 1000.0))
            .collect();
        let s = keyed_expr(&scale, "t");
        chain.push(format!(
            "scale=w='trunc(iw*({s})/2)*2':h='trunc(ih*({s})/2)*2':eval=frame"
        ));
        x = format!("(main_w-overlay_w)/2+({})*main_w", keyed_expr(&dx, "t"));
        y = format!("(main_h-overlay_h)/2+({})*main_h", keyed_expr(&dy, "t"));
    }
    g.push(&[tap], chain, &clip_label);
    let bg_label = format!("{label}_bg");
    g.chains.push(format!(
        "color=c=black:s={w}x{h}:r={rate},trim=end_frame={frames},format=yuv420p[{bg_label}]",
        rate = fps.ffmpeg_rational()
    ));
    g.push(
        &[bg_label, clip_label],
        vec![format!("overlay=x='{x}':y='{y}'")],
        label,
    );
}

/// Joins segments in order. A segment with a cross-fade-in of `n` frames overlaps the
/// programme so far by `n` frames (`xfade=fade`). The previous clip supplies a real
/// media handle when available, otherwise its last frame is held. The total length
/// equals the sum of the segment lengths.
fn join_with_fades(
    g: &mut Graph,
    labels: &[String],
    frames: &[i64],
    fades: &[i64],
    handles: &[i64],
    fps: Fps,
) -> String {
    let mut acc = labels[0].clone();
    let mut acc_frames = frames[0];
    for k in 1..labels.len() {
        let out = format!("vj{k}");
        let fade = fades[k].min(frames[k]);
        if fade > 0 {
            let held = if handles[k - 1] >= fade {
                acc.clone()
            } else {
                let held = format!("vjh{k}");
                g.push(
                    &[acc.clone()],
                    vec![format!("tpad=stop_mode=clone:stop={fade}")],
                    &held,
                );
                held
            };
            g.push(
                &[held, labels[k].clone()],
                vec![format!(
                    "xfade=transition=fade:duration={}:offset={}",
                    seconds(fps.frame_to_ticks(fade)),
                    seconds(fps.frame_to_ticks(acc_frames))
                )],
                &out,
            );
        } else {
            g.push(
                &[acc.clone(), labels[k].clone()],
                vec!["concat=n=2:v=1:a=0".into()],
                &out,
            );
        }
        acc = out;
        acc_frames += frames[k];
    }
    acc
}

/// Adds a card's chains: the plain colour card, or the colour card with its slot media
/// fitted inside the slot rectangle for exactly `card.frames` frames.
#[allow(clippy::too_many_arguments)]
fn push_card(
    g: &mut Graph,
    taps: &mut [Vec<String>],
    video_pre: &[Vec<String>],
    card: &CardSpan,
    index: usize,
    (w, h): (u32, u32),
    fps: &str,
    label: &str,
) {
    let Some(fill) = &card.fill else {
        g.chains.push(card_chain(card, w, h, fps, label));
        return;
    };
    let tap = taps[fill.input].remove(0);
    let bg = format!("cardbg{index}");
    let fg = format!("cardfg{index}");
    let colour = format!("0x{}", card.colour.trim_start_matches('#'));
    let frames = card.frames;
    g.chains.push(format!(
        "color=c={colour}:s={w}x{h}:r={fps},trim=end_frame={frames},setsar=1,format=yuv420p[{bg}]"
    ));
    let even = |v: i64| (v.max(2) / 2) * 2;
    let rw = even(w as i64 * fill.rect.width / 1000);
    let rh = even(h as i64 * fill.rect.height / 1000);
    let x0 = w as i64 * fill.rect.x / 1000;
    let y0 = h as i64 * fill.rect.y / 1000;
    let mut chain = vec![
        format!("trim=start={}", seconds(fill.source_in)),
        "setpts=PTS-STARTPTS".into(),
        format!("fps={fps}"),
        // A source shorter than the slot holds its last frame.
        format!("tpad=stop_mode=clone:stop={frames}"),
        format!("trim=end_frame={frames}"),
    ];
    if let Some(pre) = video_pre.get(fill.input) {
        chain.extend(pre.iter().cloned());
    }
    chain.push(format!(
        "scale={rw}:{rh}:force_original_aspect_ratio=decrease:force_divisible_by=2"
    ));
    chain.push("setsar=1".into());
    chain.push("format=yuv420p".into());
    g.push(&[tap], chain, &fg);
    g.chains.push(format!(
        "[{bg}][{fg}]overlay=x={x0}+({rw}-w)/2:y={y0}+({rh}-h)/2,trim=end_frame={frames},setsar=1,format=yuv420p[{label}]"
    ));
}

/// One card as a solid-colour source of exactly `card.frames` frames at the preset size.
fn card_chain(card: &CardSpan, w: u32, h: u32, fps: &str, label: &str) -> String {
    let colour = format!("0x{}", card.colour.trim_start_matches('#'));
    format!(
        "color=c={colour}:s={w}x{h}:r={fps},trim=end_frame={},setsar=1,format=yuv420p[{label}]",
        card.frames
    )
}

/// Appends the room-tone bed WAV as the last ffmpeg input when the plan has one. Returns
/// its input index; `next` is the first index after the plan inputs.
fn push_room_tone_input(cmd: &mut FfmpegCommand, plan: &RenderPlan, next: usize) -> Option<usize> {
    let bed = plan.audio.room_tone.as_ref()?;
    cmd.text("-i");
    cmd.path(&bed.file);
    Some(next)
}

/// Audio: a silent base of exact length plus one chain per layer, plus the room-tone bed
/// when present, mixed without normalisation. Returns the final audio label.
fn audio_graph(
    g: &mut Graph,
    plan: &RenderPlan,
    ffmpeg_index: &[Option<usize>],
    audio_uses: &[usize],
    bed_index: Option<usize>,
    audio_post: &[String],
) -> String {
    let rate = plan.audio.sample_rate;
    let mut audio_taps: Vec<Vec<String>> = vec![Vec::new(); plan.inputs.len()];
    for (i, uses) in audio_uses.iter().enumerate() {
        let Some(fi) = ffmpeg_index[i] else { continue };
        match *uses {
            0 => {}
            1 => audio_taps[i].push(format!("{fi}:a")),
            n => {
                let labels: Vec<String> = (0..n).map(|k| format!("as{fi}_{k}")).collect();
                let out = labels.iter().map(|l| format!("[{l}]")).collect::<String>();
                g.chains.push(format!("[{fi}:a]asplit={n}{out}"));
                audio_taps[i] = labels;
            }
        }
    }
    g.chains.push(format!(
        "anullsrc=r={rate}:cl=stereo,atrim=end_sample={}[abase]",
        plan.audio.total.to_units(rate)
    ));
    let mut mix_inputs = vec!["abase".to_string()];
    for (k, layer) in plan.audio.layers.iter().enumerate() {
        let tap = audio_taps[layer.input].remove(0);
        let label = format!("a{k}");
        g.push(&[tap], layer_filters(layer, rate), &label);
        mix_inputs.push(label);
    }
    // The room-tone bed must not be gated. `audio_post` leads with high-pass + denoise, then
    // the expander (`agate`: threshold -40 dB, range 0.1 = -20 dB), then compressor,
    // de-esser and limiter. Under speech the gate is open and the room passes; in a gap the
    // dialogue mix is silent and the bed alone sits below the threshold, so the gate would
    // close on it and bury the bed (`rightkit_audio` pads the bed by the gate range for the
    // same reason). So with a gate present the bed takes only the conditioning filters
    // before it (same room timbre as the processed dialogue), joins AFTER the gate, and
    // shares the dynamics (compressor/de-esser/limiter) with the dialogue. Bed and layers
    // never sound together, so the shared dynamics see one signal at a time.
    // Without a gate the bed just joins before the whole chain.
    let gate = audio_post.iter().position(|f| f.starts_with("agate"));
    let (pre, rest): (&[String], &[String]) = match (bed_index, gate) {
        (Some(_), Some(k)) => (&audio_post[..=k], &audio_post[k + 1..]),
        (Some(_), None) => (&[], audio_post),
        (None, _) => (audio_post, &[]),
    };
    let mix_label = if bed_index.is_some() { "adlg" } else { "aout" };
    g.push(
        &mix_inputs,
        vec![format!(
            "amix=inputs={}:duration=first:normalize=0",
            mix_inputs.len()
        )],
        mix_label,
    );
    let mut cur = mix_label.to_string();
    if !pre.is_empty() {
        let out = if bed_index.is_some() {
            "adlg_g"
        } else {
            "acln"
        };
        g.push(&[cur.clone()], pre.to_vec(), out);
        cur = out.to_string();
    }
    if let Some(bi) = bed_index {
        // The bed is exactly `total` long and zero outside the gaps, so it adds room
        // tone only where no layer plays.
        // The bed WAV is mono and takes the same mono-to-stereo upmix as every mono layer
        // (`layer_filters`): the default center-mix, -3 dB per channel. Speech and bed must
        // share one law, or the bed sits 3 dB off the speech it is compared against (a
        // `pan` copy at unity measured +3.0 dB against a camera layer's `aformat` upmix).
        let mut bed = vec![format!(
            "aformat=sample_rates={rate}:channel_layouts=stereo"
        )];
        // Conditioning = everything before the gate (high-pass, denoise).
        if let Some(k) = gate {
            bed.extend(audio_post[..k].iter().cloned());
        }
        g.push(&[format!("{bi}:a")], bed, "rtbed");
        let out = if rest.is_empty() { "aout" } else { "adlg_m" };
        g.push(
            &[cur, "rtbed".to_string()],
            vec!["amix=inputs=2:duration=first:normalize=0".to_string()],
            out,
        );
        cur = out.to_string();
    }
    if !rest.is_empty() {
        g.push(&[cur], rest.to_vec(), "acln");
        cur = "acln".to_string();
    }
    cur
}

/// The video fit chain for one clip (ends in `setsar=1,format=yuv420p`).
fn fit_filter(fit: &FitPolicy, w: u32, h: u32) -> Result<String, TimelineError> {
    Ok(match fit {
        FitPolicy::Letterbox => format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=yuv420p"
        ),
        // Centred crop to the frame's aspect ratio (never larger than the source), then scale.
        FitPolicy::CenterCrop => format!(
            "crop=w=min(iw\\,ih*{w}/{h}):h=min(ih\\,iw*{h}/{w}),scale={w}:{h}:flags=bicubic,setsar=1,format=yuv420p"
        ),
        // Resolved per clip in the segment loop (needs the input's dimensions and the
        // clip's output offset); never reached through `shared_fit`.
        FitPolicy::FaceCenter(_) => unreachable!("FaceCenter is resolved per segment"),
    })
}

/// Per-clip `FaceCenter` fit: an aspect-locked crop window repositioned per output frame
/// by `sendcmd`, driven by THIS clip's own track — anchors in SOURCE milliseconds of the
/// clip's source (centres in millicentesimals of the source frame, top-left origin),
/// sampled at `source_in + frame time`. The first frame therefore initialises the crop
/// at the clip's source_in and interpolation never borrows another clip's or source's
/// anchors. Returns the clip's filters and, when the path actually moves, the command
/// file the caller writes before FFmpeg runs (same mechanism as title [`TextFile`]s).
#[allow(clippy::too_many_arguments)]
fn face_clip_chain(
    seg: usize,
    track: &FaceTrack,
    input: &PlanInput,
    source_in: Ticks,
    frames: i64,
    fps: Fps,
    out_w: u32,
    out_h: u32,
    output: &Path,
) -> Result<(Vec<String>, Option<TextFile>), TimelineError> {
    let (sw, sh) = (input.width, input.height);
    if sw == 0 || sh == 0 {
        return Err(TimelineError::Unsupported(format!(
            "face-centre fit needs probed dimensions for source {}",
            input.source
        )));
    }
    // The same aspect-locked window as centre crop, resolved numerically so `sendcmd`
    // can drive absolute pixel positions.
    let cw = ((sw as f64).min(sh as f64 * out_w as f64 / out_h as f64) as u32) & !1;
    let ch = ((sh as f64).min(sw as f64 * out_h as f64 / out_w as f64) as u32) & !1;
    if cw < 2 || ch < 2 {
        return Err(TimelineError::Unsupported(format!(
            "face-centre crop resolves to {cw}x{ch} for source {}",
            input.source
        )));
    }
    let mut anchors = track.anchors.clone();
    anchors.sort_by_key(|a| a.start_ms);
    // Interpolated centre at output-timeline `t` (ms); held before the first and after
    // the last anchor.
    let centre_at = |t: f64| -> (f64, f64) {
        let Some(a0) = anchors.first() else {
            return (500.0, 500.0);
        };
        let mut s = 0usize;
        while s + 1 < anchors.len() && anchors[s + 1].start_ms as f64 <= t {
            s += 1;
        }
        let a = &anchors[s];
        match anchors.get(s + 1) {
            Some(b) if t > a.start_ms as f64 && b.start_ms > a.start_ms => {
                let f =
                    ((t - a.start_ms as f64) / (b.start_ms - a.start_ms) as f64).clamp(0.0, 1.0);
                (
                    a.center_x_milli as f64 + (b.center_x_milli - a.center_x_milli) as f64 * f,
                    a.center_y_milli as f64 + (b.center_y_milli - a.center_y_milli) as f64 * f,
                )
            }
            _ if s == 0 && t < a0.start_ms as f64 => {
                (a0.center_x_milli as f64, a0.center_y_milli as f64)
            }
            _ => (a.center_x_milli as f64, a.center_y_milli as f64),
        }
    };
    let max_x = sw.saturating_sub(cw) as f64;
    let max_y = sh.saturating_sub(ch) as f64;
    let pos = |cx: f64, cy: f64| -> (u32, u32) {
        let even = |v: f64, max: f64| ((((v.clamp(0.0, max)) / 2.0).round() as i64) * 2) as u32;
        (
            even(cx * sw as f64 / 1000.0 - cw as f64 / 2.0, max_x),
            even(cy * sh as f64 / 1000.0 - ch as f64 / 2.0, max_y),
        )
    };
    let fps_f = fps.num as f64 / fps.den as f64;
    let source_in_ms = source_in.to_units(1000) as f64;
    let mut cmds: Vec<(f64, u32, u32)> = Vec::new();
    let mut last: Option<(u32, u32)> = None;
    for f in 0..frames.max(0) {
        let (cx, cy) = centre_at(source_in_ms + f as f64 * 1000.0 / fps_f);
        let p = pos(cx, cy);
        if last != Some(p) {
            // After setpts=PTS-STARTPTS the clip's first frame is t=0.
            cmds.push((f as f64 / fps_f, p.0, p.1));
            last = Some(p);
        }
    }
    let (x0, y0) = cmds
        .first()
        .map(|c| (c.1, c.2))
        .unwrap_or(((sw - cw) / 2, (sh - ch) / 2));
    let mut filters = Vec::new();
    let mut file = None;
    if cmds.len() > 1 {
        let dir = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let path = dir.join(format!(".reframe-crop-{seg}.cmd"));
        let mut text = String::new();
        for (t, x, y) in &cmds {
            text.push_str(&format!("{t:.4} crop@fc{seg} x {x}, crop@fc{seg} y {y};\n"));
        }
        filters.push(format!(
            "sendcmd=f={}",
            crate::title::escape_filter_path(&path)?
        ));
        file = Some(TextFile {
            path,
            content: text,
        });
    }
    filters.push(format!("crop@fc{seg}=w={cw}:h={ch}:x={x0}:y={y0}"));
    filters.push(format!("scale={out_w}:{out_h}:flags=bicubic"));
    filters.push("setsar=1".into());
    filters.push("format=yuv420p".into());
    Ok((filters, file))
}

/// Audio-only command that measures the composed mix (first `loudnorm` pass, JSON on
/// stderr at `info` level). Runs the same audio graph as the render — including the
/// `audio_post` cleanup/compression chain, so the measurement matches the signal the
/// final pass will master. Writes nothing.
pub fn build_ffmpeg_measure(
    req: &RenderRequest<'_>,
    target: &LoudnessTarget,
    audio_post: &[String],
) -> Result<FfmpegCommand, TimelineError> {
    let plan = req.plan;
    let n_inputs = plan.inputs.len();
    let mut audio_uses = vec![0usize; n_inputs];
    for layer in &plan.audio.layers {
        audio_uses[layer.input] += 1;
    }
    let mut cmd = FfmpegCommand::default();
    cmd.texts([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "info",
        "-nostats",
    ]);
    let mut ffmpeg_index: Vec<Option<usize>> = vec![None; n_inputs];
    let mut next = 0;
    for (i, input) in plan.inputs.iter().enumerate() {
        if audio_uses[i] == 0 {
            continue;
        }
        let path = req
            .inputs
            .get(&input.source)
            .ok_or_else(|| TimelineError::MissingInputPath(input.source.to_string()))?;
        cmd.text("-i");
        cmd.path(path);
        ffmpeg_index[i] = Some(next);
        next += 1;
    }
    let bed_index = push_room_tone_input(&mut cmd, plan, next);
    let mut g = Graph { chains: Vec::new() };
    let measured_in = audio_graph(
        &mut g,
        plan,
        &ffmpeg_index,
        &audio_uses,
        bed_index,
        audio_post,
    );
    g.push(
        &[measured_in],
        vec![loudnorm_filter(target, None, "json")],
        "ameasured",
    );
    cmd.text("-filter_complex");
    cmd.text(g.render());
    cmd.texts(["-map", "[ameasured]", "-vn", "-f", "null", "-"]);
    Ok(cmd)
}

fn layer_filters(layer: &AudioLayer, rate: i64) -> Vec<String> {
    let mut f = vec![
        format!(
            "atrim=start={}:duration={}",
            seconds(layer.source_in),
            seconds(layer.duration)
        ),
        "asetpts=PTS-STARTPTS".into(),
        format!("aformat=sample_rates={rate}:channel_layouts=stereo"),
    ];
    if layer.gain.0 != 0 {
        f.push(format!("volume={}", layer.gain.ffmpeg_db()));
    }
    let delay = layer.start.to_units(rate);
    if delay > 0 {
        f.push(format!("adelay=delays={delay}S:all=1"));
    }
    for w in &layer.windows {
        let level = match w.attenuation {
            Attenuation::Duck { amount } => crate::time::MilliDb(-amount.0).ffmpeg_db(),
            Attenuation::Mute => "0".to_string(),
        };
        f.push(format!(
            "volume={level}:enable='between(t,{},{})'",
            seconds(w.start),
            seconds(w.end)
        ));
    }
    f
}

fn encoder_args(cmd: &mut FfmpegCommand, encoder: Encoder, preset: Preset, plan: &RenderPlan) {
    let kbps = format!("{}k", preset.video_kbps());
    match encoder {
        Encoder::MediaFoundation => {
            cmd.texts(["-c:v", "h264_mf", "-rate_control", "u_vbr", "-b:v"]);
            cmd.text(kbps);
        }
        Encoder::Nvenc => {
            cmd.texts([
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p5",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-cq",
                "21",
                "-b:v",
                "0",
                "-profile:v",
                "high",
            ]);
        }
        Encoder::VideoToolbox => {
            cmd.texts([
                "-c:v",
                "h264_videotoolbox",
                "-profile:v",
                "high",
                "-allow_sw",
                "0",
                "-b:v",
            ]);
            cmd.text(kbps);
        }
    }
    // Keyframe every two seconds (shared rounding rule).
    let gop = crate::time::round_div_half_down(plan.fps.num as i128 * 2, plan.fps.den as i128);
    cmd.text("-g");
    cmd.text(gop.max(1).to_string());
}
