//! Turning an accepted [`Proposal`] into a short: a variant timeline (an ordered
//! revision-backed [`Action`] list), the cut plan and source->output map behind it,
//! caption titles from the shared caption segmenter, and a render request.
//!
//! Nothing here renders or touches disk. The actions can be applied to a fresh
//! [`TimelineStore`] (a new variant document, [`ShortPlan::variant_store`]) or to a
//! project's own store; the render request is what the project layer's `prepare` /
//! `execute` consume (preset, fit policy, name).
//!
//! Locked invariant: vertical delivery needs an approved reframe plan. The 9:16 fit
//! here is a centre-crop placeholder, so [`ShortRenderRequest::deliverable`] is false
//! until the reframe lane supplies an approved plan ([`ReframeState::Approved`]).

use rightkit_autoedit::cuts::{
    plan_cuts, CutConfig, CutPlan, Preset as CutPreset, RealiseOptions, TimeMap,
};
use rightkit_captions::{
    build_cues, config_for_preset, to_srt, words_from_transcript, Cue, Newline,
};
use rightkit_timeline::{
    Action, FitPolicy, Fps, OverlayClip, Preset, SourceInfo, Ticks, TimelineStore, TitleTemplate,
};
use rightkit_transcript::{Span, Transcript};
use serde::{Deserialize, Serialize};

use crate::error::ShortsError;
use crate::propose::Proposal;

#[derive(Debug, Clone)]
pub struct ShortOptions {
    pub preset: Preset,
    /// `Some` runs the cut engine over the clip (default: tight); `None` keeps the
    /// padded range whole.
    pub cut: Option<CutConfig>,
    /// Add caption titles (revision-backed `AddTitle` actions).
    pub captions: bool,
    /// Fit of the source to the preset frame. Default: the preset's (Reels: centre-crop).
    pub fit: FitPolicy,
    pub reframe: ReframeState,
    pub caption_track: String,
    pub realise: RealiseOptions,
}

impl Default for ShortOptions {
    fn default() -> Self {
        Self {
            preset: Preset::Reels,
            cut: Some(CutConfig::tight()),
            captions: true,
            fit: Preset::Reels.default_fit(),
            reframe: ReframeState::CentreCropPlaceholder,
            caption_track: "T1".into(),
            realise: RealiseOptions::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReframeState {
    /// Centre crop; fine for preview, not for delivery.
    CentreCropPlaceholder,
    /// An approved reframe plan (and anchors) the reframe lane produced.
    Approved { plan_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortRenderRequest {
    pub preset: Preset,
    pub fit: FitPolicy,
    pub reframe: ReframeState,
    /// File stem for `renders/` (sanitised-safe: letters, digits, `-`).
    pub out_name: String,
    /// Suggested post title (metadata; not burned in).
    pub title: String,
    pub platform: String,
    /// Two-pass loudness normalisation for the delivered file.
    pub loudness_normalize: bool,
    /// False while the reframe is a placeholder: render for preview only.
    pub deliverable: bool,
}

#[derive(Debug, Clone)]
pub struct ShortPlan {
    /// `<proposal id>-<8 hex>` (hash of the cut plan and placement).
    pub variant_id: String,
    pub proposal_id: String,
    pub source: SourceInfo,
    pub fps: Fps,
    /// RegisterSource, the cut-plan clips and audio, then caption titles; apply in order.
    pub actions: Vec<Action>,
    /// Plan over the clip's words (word indices refer to the clip's own transcript).
    pub cut_plan: CutPlan,
    pub time_map: TimeMap,
    pub cues: Vec<Cue>,
    pub srt: String,
    pub request: ShortRenderRequest,
    pub output_frames: i64,
}

impl ShortPlan {
    /// A fresh timeline document holding only this short.
    pub fn variant_store(&self) -> Result<TimelineStore, ShortsError> {
        let mut store = TimelineStore::new(self.fps);
        for a in &self.actions {
            store.apply(a.clone())?;
        }
        Ok(store)
    }
}

fn floor_frame(fps: Fps, ms: u64) -> Ticks {
    let t = Ticks::from_millis(ms as i64);
    let snapped = fps.snap(t);
    if snapped > t {
        fps.frame_to_ticks(fps.ticks_to_frame(t) - 1)
    } else {
        snapped
    }
}

fn ceil_frame(fps: Fps, ms: u64) -> Ticks {
    let t = Ticks::from_millis(ms as i64);
    let snapped = fps.snap(t);
    if snapped < t {
        fps.frame_to_ticks(fps.ticks_to_frame(t) + 1)
    } else {
        snapped
    }
}

/// Builds the short for `proposal`. `envelope` is the optional per-10ms RMS the cut
/// engine uses to snap cuts to the quietest frame.
pub fn build_short(
    proposal: &Proposal,
    transcript: &Transcript,
    source: &SourceInfo,
    fps: Fps,
    opts: &ShortOptions,
    envelope: Option<&[f32]>,
) -> Result<ShortPlan, ShortsError> {
    transcript.validate()?;
    if proposal.source_blake3 != transcript.source_blake3
        || source.id.as_str() != transcript.source_blake3
    {
        return Err(ShortsError::Mismatch(
            "proposal, transcript and source name different BLAKE3 ids".into(),
        ));
    }
    let (first, last) = (proposal.first_word, proposal.last_word);
    let words = transcript
        .words
        .get(first..=last)
        .ok_or_else(|| ShortsError::Mismatch("word range is outside the transcript".into()))?;
    if words[0].start_ms != proposal.start_ms || words[words.len() - 1].end_ms != proposal.end_ms {
        return Err(ShortsError::Mismatch(
            "proposal range does not match its words (stale proposal?)".into(),
        ));
    }

    // Clip bounds on the frame grid: wide enough to keep every word whole, and never
    // beyond the source or the silence the proposal padded into.
    let src_dur = source.duration;
    let lo = floor_frame(fps, proposal.clip_start_ms);
    let hi = ceil_frame(fps, proposal.clip_end_ms).min(fps.snap(src_dur).max(Ticks::ZERO));
    let hi = if hi <= lo { src_dur } else { hi };
    if hi <= lo {
        return Err(ShortsError::Invalid("empty clip range".into()));
    }

    // The clip's own transcript: only its words, so the cut engine can never reach
    // into neighbouring speech.
    let clip_duration = (hi.to_units(1000) as u64)
        .max(proposal.end_ms)
        .min(transcript.duration_ms);
    let clip = Transcript {
        schema: transcript.schema.clone(),
        source_blake3: transcript.source_blake3.clone(),
        language: transcript.language.clone(),
        duration_ms: clip_duration,
        words: words.to_vec(),
        vad: transcript
            .vad
            .iter()
            .filter(|s| s.end_ms > proposal.clip_start_ms && s.start_ms < clip_duration)
            .map(|s| Span {
                start_ms: s.start_ms.max(proposal.clip_start_ms),
                end_ms: s.end_ms.min(clip_duration),
            })
            .filter(|s| s.start_ms < s.end_ms)
            .collect(),
        model: transcript.model.clone(),
    };

    let mut plan = match &opts.cut {
        Some(cfg) => plan_cuts(&clip, envelope, cfg, fps)?,
        None => CutPlan {
            preset: CutPreset::Natural,
            source: source.id.clone(),
            fps,
            source_duration_ms: transcript.duration_ms,
            kept: vec![rightkit_autoedit::cuts::KeptRange {
                source_in: lo,
                source_out: hi,
            }],
            kept_words: (0..clip.words.len()).collect(),
            removed: Vec::new(),
            decisions: Vec::new(),
        },
    };
    // Clamp to the clip bounds (the engine's lead-in/tail-out are silence, but the
    // clip transcript has no neighbours to stop them early).
    for k in &mut plan.kept {
        k.source_in = k.source_in.max(lo);
        k.source_out = k.source_out.min(hi);
    }
    plan.kept.retain(|k| k.source_out > k.source_in);
    if plan.kept.is_empty() {
        return Err(ShortsError::Invalid("cut plan kept nothing".into()));
    }

    let group = plan.group(&opts.realise);
    let time_map = plan.time_map();
    let output_frames = time_map.output_frames();

    // Captions from the words that survive the cuts, on the output timeline.
    let kept_clip = Transcript {
        words: plan
            .kept_words
            .iter()
            .map(|&i| clip.words[i].clone())
            .collect(),
        ..clip.clone()
    };
    let timed = words_from_transcript(&kept_clip, |ms| {
        time_map
            .source_ms_to_output_ms(ms as i64)
            .filter(|v| *v >= 0)
            .map(|v| v as u64)
    });
    let cues = build_cues(&timed, &config_for_preset(opts.preset));
    let srt = to_srt(&cues, Newline::Lf);

    let mut actions = Vec::with_capacity(group.actions.len() + cues.len() + 1);
    actions.push(Action::RegisterSource {
        source: source.clone(),
    });
    actions.extend(group.actions.iter().cloned());
    if opts.captions {
        let frame = |ms: u64| fps.ticks_to_frame(Ticks::from_millis(ms as i64));
        let mut prev_end = 0i64;
        for (n, c) in cues.iter().enumerate() {
            let start = frame(c.start_ms).max(prev_end);
            let end = frame(c.end_ms).max(start + 1).min(output_frames);
            if start >= output_frames || end <= start {
                continue;
            }
            prev_end = end;
            actions.push(Action::AddTitle {
                track_id: opts.caption_track.clone(),
                clip: OverlayClip {
                    id: format!("{}-cap{:03}", group.id, n + 1),
                    start_frame: start,
                    frames: end - start,
                    template: TitleTemplate::Caption { text: c.text() },
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                },
            });
        }
    }

    let variant_id = format!(
        "{}-{}",
        proposal.id,
        group.id.trim_start_matches("autocut-")
    );
    let deliverable = matches!(opts.reframe, ReframeState::Approved { .. });
    let request = ShortRenderRequest {
        preset: opts.preset,
        fit: opts.fit.clone(),
        reframe: opts.reframe.clone(),
        out_name: variant_id.clone(),
        title: proposal.title.clone(),
        platform: "vertical-short".into(),
        loudness_normalize: true,
        deliverable,
    };
    Ok(ShortPlan {
        variant_id,
        proposal_id: proposal.id.clone(),
        source: source.clone(),
        fps,
        actions,
        cut_plan: plan,
        time_map,
        cues,
        srt,
        request,
        output_frames,
    })
}
