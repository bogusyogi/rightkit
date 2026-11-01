//! Realising a [`CutPlan`]: timeline action group and source->output time map.

use rightkit_timeline::{
    Action, ApplyOutcome, AudioClip, Fps, MilliDb, Ticks, TimelineStore, UnderAudio, VideoClip,
};
use rightkit_transcript::Transcript;
use serde::{Deserialize, Serialize};

use crate::cuts::error::CutError;
use crate::cuts::plan::CutPlan;

/// An existing full-source clip (and its linked narration) that the auto-edit
/// turns into the first kept range instead of removing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceTarget {
    pub video_clip_id: String,
    pub audio_clip_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealiseOptions {
    pub video_track: String,
    pub audio_track: String,
    /// Index on the video track where the first kept clip lands (the replaced
    /// clip's index when `replace` is set).
    pub insert_index: usize,
    pub replace: Option<ReplaceTarget>,
    /// Also place linked source audio on `audio_track`.
    pub with_audio: bool,
}

impl Default for RealiseOptions {
    fn default() -> Self {
        Self {
            video_track: "V1".into(),
            audio_track: "A1".into(),
            insert_index: 0,
            replace: None,
            with_audio: true,
        }
    }
}

/// A whole auto-edit as an ordered action sequence with stable, descriptive ids.
/// Clip ids are `<id>-v001..` (video) and `<id>-a001..` (audio); the replaced
/// base clip, if any, keeps its own id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionGroup {
    /// `autocut-<8 hex>` of the plan and placement.
    pub id: String,
    pub label: String,
    pub video_clip_ids: Vec<String>,
    pub audio_clip_ids: Vec<String>,
    pub actions: Vec<Action>,
}

/// The revisions an applied group recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedGroup {
    pub group_id: String,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    /// Revisions recorded (an action that changes nothing records none).
    pub revisions: usize,
}

impl CutPlan {
    /// One action per kept clip (V1 + linked audio), in output order.
    pub fn group(&self, opts: &RealiseOptions) -> ActionGroup {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.to_json());
        hasher.update(opts.video_track.as_bytes());
        hasher.update(&(opts.insert_index as u64).to_le_bytes());
        let id = format!("autocut-{}", &hasher.finalize().to_hex()[..8]);
        let fps = self.fps;
        let mut actions = Vec::new();
        let (mut vids, mut aids) = (Vec::new(), Vec::new());
        let mut cum_frames = 0i64;
        for (i, k) in self.kept.iter().enumerate() {
            let first_replaced = i == 0 && opts.replace.is_some();
            let vid = match (&opts.replace, i) {
                (Some(r), 0) => r.video_clip_id.clone(),
                _ => format!("{id}-v{:03}", i + 1),
            };
            if first_replaced {
                actions.push(Action::TrimClip {
                    clip_id: vid.clone(),
                    source_in: k.source_in,
                    source_out: k.source_out,
                });
            } else {
                actions.push(Action::InsertClip {
                    track_id: opts.video_track.clone(),
                    index: opts.insert_index + i,
                    clip: VideoClip {
                        id: vid.clone(),
                        source: self.source.clone(),
                        source_in: k.source_in,
                        source_out: k.source_out,
                    },
                });
            }
            if opts.with_audio {
                let existing = opts
                    .replace
                    .as_ref()
                    .filter(|_| i == 0)
                    .and_then(|r| r.audio_clip_id.clone());
                match existing {
                    Some(aid) => {
                        actions.push(Action::TrimAudioClip {
                            clip_id: aid.clone(),
                            trim_in: k.source_in,
                            trim_out: k.source_out,
                        });
                        aids.push(aid);
                    }
                    None => {
                        let aid = format!("{id}-a{:03}", i + 1);
                        actions.push(Action::AddAudioClip {
                            track_id: opts.audio_track.clone(),
                            clip: AudioClip {
                                id: aid.clone(),
                                source: self.source.clone(),
                                trim_in: k.source_in,
                                trim_out: k.source_out,
                                start: fps.frame_to_ticks(cum_frames),
                                gain: MilliDb::ZERO,
                                link: Some(vid.clone()),
                                under: UnderAudio::Keep,
                            },
                        });
                        aids.push(aid);
                    }
                }
            }
            vids.push(vid);
            cum_frames += fps.ticks_to_frame(Ticks(k.source_out.0 - k.source_in.0));
        }
        ActionGroup {
            label: format!(
                "Auto-edit ({:?}): {} clips, {} cuts",
                self.preset,
                self.kept.len(),
                self.removed.len()
            ),
            id,
            video_clip_ids: vids,
            audio_clip_ids: aids,
            actions,
        }
    }

    pub fn time_map(&self) -> TimeMap {
        TimeMap::new(self)
    }
}

/// Applies every action as its own revision. On failure the revisions already
/// recorded are undone, so a group is all-or-nothing.
pub fn apply_group(
    store: &mut TimelineStore,
    group: &ActionGroup,
) -> Result<AppliedGroup, CutError> {
    let mut seqs = Vec::new();
    for action in &group.actions {
        match store.apply(action.clone()) {
            Ok(ApplyOutcome::Recorded(seq)) => seqs.push(seq),
            Ok(ApplyOutcome::NoChange) => {}
            Err(e) => {
                for _ in &seqs {
                    let _ = store.undo();
                }
                return Err(e.into());
            }
        }
    }
    Ok(AppliedGroup {
        group_id: group.id.clone(),
        first_seq: seqs.first().copied(),
        last_seq: seqs.last().copied(),
        revisions: seqs.len(),
    })
}

/// Undoes a group applied by [`apply_group`]; it must still be the latest edit.
pub fn undo_group(store: &mut TimelineStore, applied: &AppliedGroup) -> Result<(), CutError> {
    if let Some(last) = applied.last_seq {
        if store.cursor() as u64 != last {
            return Err(CutError::Group(format!(
                "{} is not the latest edit (cursor {}, group ends at {last})",
                applied.group_id,
                store.cursor()
            )));
        }
    }
    for _ in 0..applied.revisions {
        store.undo()?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapSegment {
    pub source_in: Ticks,
    pub source_out: Ticks,
    /// Output start (frame-aligned): where the matching video clip sits.
    pub out_start: Ticks,
    pub out_frames: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappedWord {
    /// Index into `Transcript::words`.
    pub index: usize,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Source time -> output time for the edited timeline (and back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeMap {
    pub fps: Fps,
    pub segments: Vec<MapSegment>,
    kept_words: Vec<usize>,
}

impl TimeMap {
    fn new(plan: &CutPlan) -> Self {
        let mut cum = 0i64;
        let segments = plan
            .kept
            .iter()
            .map(|k| {
                let frames = plan
                    .fps
                    .ticks_to_frame(Ticks(k.source_out.0 - k.source_in.0));
                let seg = MapSegment {
                    source_in: k.source_in,
                    source_out: k.source_out,
                    out_start: plan.fps.frame_to_ticks(cum),
                    out_frames: frames,
                };
                cum += frames;
                seg
            })
            .collect();
        Self {
            fps: plan.fps,
            segments,
            kept_words: plan.kept_words.clone(),
        }
    }

    pub fn output_frames(&self) -> i64 {
        self.segments.iter().map(|s| s.out_frames).sum()
    }

    pub fn output_duration(&self) -> Ticks {
        self.fps.frame_to_ticks(self.output_frames())
    }

    /// Output time of a source time inside a kept range; `None` if it was cut.
    pub fn source_to_output(&self, t: Ticks) -> Option<Ticks> {
        self.segments
            .iter()
            .find(|s| t >= s.source_in && t < s.source_out)
            .map(|s| Ticks(s.out_start.0 + t.0 - s.source_in.0))
    }

    /// Source time of an output time; `None` past the end.
    pub fn output_to_source(&self, t: Ticks) -> Option<Ticks> {
        self.segments
            .iter()
            .find(|s| t >= s.out_start && t.0 < s.out_start.0 + (s.source_out.0 - s.source_in.0))
            .map(|s| Ticks(s.source_in.0 + t.0 - s.out_start.0))
    }

    pub fn source_ms_to_output_ms(&self, ms: i64) -> Option<i64> {
        self.source_to_output(Ticks::from_millis(ms))
            .map(|t| t.to_units(1000))
    }

    pub fn output_ms_to_source_ms(&self, ms: i64) -> Option<i64> {
        self.output_to_source(Ticks::from_millis(ms))
            .map(|t| t.to_units(1000))
    }

    /// Output times of every surviving word, for captions. A word that ends
    /// past its kept range (approximate end) is clipped to the range.
    pub fn map_words(&self, transcript: &Transcript) -> Vec<MappedWord> {
        self.kept_words
            .iter()
            .filter_map(|&i| {
                let w = transcript.words.get(i)?;
                let s = Ticks::from_millis(w.start_ms as i64);
                let e = Ticks::from_millis(w.end_ms as i64);
                let seg = self
                    .segments
                    .iter()
                    .find(|g| s < g.source_out && e > g.source_in)?;
                let s = s.max(seg.source_in);
                let e = e.min(seg.source_out);
                let map = |t: Ticks| Ticks(seg.out_start.0 + t.0 - seg.source_in.0);
                Some(MappedWord {
                    index: i,
                    start_ms: map(s).to_units(1000),
                    end_ms: map(e).to_units(1000),
                })
            })
            .collect()
    }
}
