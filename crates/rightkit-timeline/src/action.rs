//! Typed edit actions, their ripple semantics, and the inverse actions undo replays.

use serde::{Deserialize, Serialize};

use crate::error::TimelineError;
use crate::model::{
    AudioClip, AudioRole, AudioTrack, CaptionCorrection, CardClip, ClipVisualKeys,
    EditorCorrections, MicAttachment, OverlayClip, OverlayTrack, SlotFill, SourceId, SourceInfo,
    TextPosition, Timeline, TitleTemplate, UnderAudio, VideoClip, VideoTrack,
};
use crate::time::{MilliDb, Ticks};

/// A user-level edit. Video edits ripple the video track and move narration linked
/// to the shifted clips; unlinked audio never moves implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Registers a probed source. Re-registering identical info is a no-op.
    RegisterSource {
        source: SourceInfo,
    },
    /// Records a source-level external microphone alignment. Renderers expand
    /// it for each camera occurrence, so edits do not orphan the microphone.
    AttachMicrophone {
        attachment: MicAttachment,
    },
    /// Replaces the opening and end cards. Card ids are validated with the timeline.
    SetCards {
        cards: Vec<CardClip>,
    },
    /// Fills a card's slot with a registered video source (or clears it with `None`).
    /// The card must exist; the fill is validated with the timeline.
    SetSlotFill {
        card_id: String,
        fill: Option<SlotFill>,
    },
    AddVideoTrack {
        track_id: String,
    },
    AddAudioTrack {
        track_id: String,
        role: AudioRole,
    },
    /// Inserts a clip at `index` on a video track; later clips ripple right.
    InsertClip {
        track_id: String,
        index: usize,
        clip: VideoClip,
    },
    /// Removes a clip; later clips ripple left. Narration linked to it is unlinked
    /// and stays put.
    RemoveClip {
        clip_id: String,
    },
    /// Changes the source range; the clip keeps its start, later clips ripple.
    TrimClip {
        clip_id: String,
        source_in: Ticks,
        source_out: Ticks,
    },
    /// Moves a clip so it sits at `to_index` once removed from its old position.
    MoveClip {
        clip_id: String,
        to_index: usize,
    },
    AddAudioClip {
        track_id: String,
        clip: AudioClip,
    },
    RemoveAudioClip {
        clip_id: String,
    },
    /// Moves an audio clip to a new output start (snapped to the frame grid).
    MoveAudioClip {
        clip_id: String,
        start: Ticks,
    },
    TrimAudioClip {
        clip_id: String,
        trim_in: Ticks,
        trim_out: Ticks,
    },
    SetGain {
        clip_id: String,
        gain: MilliDb,
    },
    /// Replaces a video clip's keyed visual parameters (opacity, transform,
    /// crop, cross-fade). An empty value clears them; undo restores the prior ones.
    SetClipVisuals {
        clip_id: String,
        visuals: ClipVisualKeys,
    },
    /// Mutes this clip's own audio, preserving gain for unmute.
    SetMute {
        clip_id: String,
        muted: bool,
    },
    /// Replaces an occurrence with another registered take; linked matching
    /// narration follows the replacement and later clips ripple atomically.
    SwapTake {
        clip_id: String,
        source: SourceId,
        source_in: Ticks,
        source_out: Ticks,
    },
    EditCaption {
        cue_id: String,
        correction: CaptionCorrection,
    },
    /// Drops a caption correction (Restore original). Undoable like any edit.
    RemoveCaption {
        cue_id: String,
    },
    PositionTitle {
        clip_id: String,
        position: TextPosition,
    },
    LinkNarration {
        audio_id: String,
        video_id: String,
    },
    UnlinkNarration {
        audio_id: String,
    },
    SetUnder {
        audio_id: String,
        under: UnderAudio,
    },
    /// Adds a title to an overlay track, creating the track if it does not exist yet.
    AddTitle {
        track_id: String,
        clip: OverlayClip,
    },
    RemoveTitle {
        clip_id: String,
    },
    /// Moves a title to a new start frame (duration unchanged).
    MoveTitle {
        clip_id: String,
        start_frame: i64,
    },
    /// Sets a title's span (start frame and length).
    TrimTitle {
        clip_id: String,
        start_frame: i64,
        frames: i64,
    },
    /// Replaces a title's template (text) and fades.
    EditTitle {
        clip_id: String,
        template: TitleTemplate,
        fade_in_frames: i64,
        fade_out_frames: i64,
    },
    /// Splits a video clip `at` ticks into its source range; the right half
    /// takes `new_clip_id`. Total output is unchanged (the halves stay gapless).
    SplitClip {
        clip_id: String,
        at: Ticks,
        new_clip_id: String,
    },
    /// Splits an audio clip `at` ticks into its (trimmed) duration; the right
    /// half takes `new_clip_id` and starts `at` after the clip's start.
    SplitAudioClip {
        clip_id: String,
        at: Ticks,
        new_clip_id: String,
    },
    /// Slips a video clip: shifts its source window by `delta` while keeping
    /// its output span. Later clips do not move.
    SlipClip {
        clip_id: String,
        delta: Ticks,
    },
    /// Slips an audio clip: shifts its trim window by `delta` while keeping
    /// its output start and duration.
    SlipAudioClip {
        clip_id: String,
        delta: Ticks,
    },
    /// Removes the output range `[start, end)` from every video track it
    /// covers: covered clips are trimmed, split, or removed and later clips
    /// ripple left (linked narration follows). Clips created by splits take
    /// deterministic `{clip_id}~r` ids so a byte-identical retry is a no-op.
    RippleDeleteRange {
        start: Ticks,
        end: Ticks,
    },
    /// Applies several actions as one revision (one undo step).
    Batch {
        actions: Vec<Action>,
    },
}

/// An inverse action recorded with a revision; replayed in order by undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InverseAction {
    RestoreCorrections { corrections: EditorCorrections },
    RestoreMicAttachments { attachments: Vec<MicAttachment> },
    RestoreCards { cards: Vec<CardClip> },
    RestoreSlotFills { fills: BTreeMap<String, SlotFill> },
    RestoreVideoTrack { track: VideoTrack },
    RestoreAudioTrack { track: AudioTrack },
    RestoreOverlayTrack { track: OverlayTrack },
    RemoveTrack { track_id: String },
    UnregisterSource { source_id: SourceId },
}

impl InverseAction {
    pub fn apply(&self, tl: &mut Timeline) -> Result<(), TimelineError> {
        match self {
            InverseAction::RestoreCorrections { corrections } => {
                tl.corrections = corrections.clone()
            }
            InverseAction::RestoreMicAttachments { attachments } => {
                tl.mic_attachments = attachments.clone()
            }
            InverseAction::RestoreCards { cards } => tl.cards = cards.clone(),
            InverseAction::RestoreSlotFills { fills } => tl.slot_fills = fills.clone(),
            InverseAction::RestoreVideoTrack { track } => {
                let slot = tl
                    .video_tracks
                    .iter_mut()
                    .find(|t| t.id == track.id)
                    .ok_or_else(|| TimelineError::UnknownTrack(track.id.clone()))?;
                *slot = track.clone();
            }
            InverseAction::RestoreAudioTrack { track } => {
                let slot = tl
                    .audio_tracks
                    .iter_mut()
                    .find(|t| t.id == track.id)
                    .ok_or_else(|| TimelineError::UnknownTrack(track.id.clone()))?;
                *slot = track.clone();
            }
            InverseAction::RestoreOverlayTrack { track } => {
                let slot = tl
                    .overlay_tracks
                    .iter_mut()
                    .find(|t| t.id == track.id)
                    .ok_or_else(|| TimelineError::UnknownTrack(track.id.clone()))?;
                *slot = track.clone();
            }
            InverseAction::RemoveTrack { track_id } => {
                let count = |tl: &Timeline| {
                    tl.video_tracks.len() + tl.audio_tracks.len() + tl.overlay_tracks.len()
                };
                let before = count(tl);
                tl.video_tracks.retain(|t| &t.id != track_id);
                tl.audio_tracks.retain(|t| &t.id != track_id);
                tl.overlay_tracks.retain(|t| &t.id != track_id);
                if count(tl) == before {
                    return Err(TimelineError::UnknownTrack(track_id.clone()));
                }
            }
            InverseAction::UnregisterSource { source_id } => {
                let before = tl.sources.len();
                tl.sources.retain(|s| &s.id != source_id);
                if tl.sources.len() == before {
                    return Err(TimelineError::UnknownSource(source_id.to_string()));
                }
            }
        }
        Ok(())
    }
}

/// Applies `inverse` actions in order, then validates the result.
pub fn apply_inverse(tl: &Timeline, inverse: &[InverseAction]) -> Result<Timeline, TimelineError> {
    let mut next = tl.clone();
    for step in inverse {
        step.apply(&mut next)?;
    }
    next.validate()?;
    Ok(next)
}

/// Applies `action` to a copy of `tl`, returning the new timeline and the inverse.
/// The input timeline is never mutated, so a failed action leaves no trace.
pub fn apply_action(
    tl: &Timeline,
    action: &Action,
) -> Result<(Timeline, Vec<InverseAction>), TimelineError> {
    let mut next = tl.clone();
    mutate(&mut next, action)?;
    next.validate()?;
    let inverse = diff_inverse(tl, &next);
    Ok((next, inverse))
}

fn diff_inverse(old: &Timeline, new: &Timeline) -> Vec<InverseAction> {
    let mut restores = Vec::new();
    if old.mic_attachments != new.mic_attachments {
        restores.push(InverseAction::RestoreMicAttachments {
            attachments: old.mic_attachments.clone(),
        });
    }
    if old.corrections != new.corrections {
        restores.push(InverseAction::RestoreCorrections {
            corrections: old.corrections.clone(),
        });
    }
    if old.cards != new.cards {
        restores.push(InverseAction::RestoreCards {
            cards: old.cards.clone(),
        });
    }
    if old.slot_fills != new.slot_fills {
        restores.push(InverseAction::RestoreSlotFills {
            fills: old.slot_fills.clone(),
        });
    }
    let mut removals = Vec::new();
    for t in &new.video_tracks {
        match old.video_tracks.iter().find(|o| o.id == t.id) {
            Some(o) if o != t => {
                restores.push(InverseAction::RestoreVideoTrack { track: o.clone() })
            }
            Some(_) => {}
            None => removals.push(InverseAction::RemoveTrack {
                track_id: t.id.clone(),
            }),
        }
    }
    for t in &new.audio_tracks {
        match old.audio_tracks.iter().find(|o| o.id == t.id) {
            Some(o) if o != t => {
                restores.push(InverseAction::RestoreAudioTrack { track: o.clone() })
            }
            Some(_) => {}
            None => removals.push(InverseAction::RemoveTrack {
                track_id: t.id.clone(),
            }),
        }
    }
    for t in &new.overlay_tracks {
        match old.overlay_tracks.iter().find(|o| o.id == t.id) {
            Some(o) if o != t => {
                restores.push(InverseAction::RestoreOverlayTrack { track: o.clone() })
            }
            Some(_) => {}
            None => removals.push(InverseAction::RemoveTrack {
                track_id: t.id.clone(),
            }),
        }
    }
    for s in &new.sources {
        if old.source(&s.id).is_none() {
            removals.push(InverseAction::UnregisterSource {
                source_id: s.id.clone(),
            });
        }
    }
    restores.extend(removals);
    restores
}

fn video_clip_pos(tl: &Timeline, id: &str) -> Result<(usize, usize), TimelineError> {
    tl.find_video_clip(id)
        .ok_or_else(|| TimelineError::UnknownClip(id.to_string()))
}

fn audio_clip_mut<'a>(tl: &'a mut Timeline, id: &str) -> Result<&'a mut AudioClip, TimelineError> {
    let (t, c) = tl
        .find_audio_clip(id)
        .ok_or_else(|| TimelineError::UnknownClip(id.to_string()))?;
    Ok(&mut tl.audio_tracks[t].clips[c])
}

fn title_mut<'a>(tl: &'a mut Timeline, id: &str) -> Result<&'a mut OverlayClip, TimelineError> {
    let (t, c) = tl
        .find_title(id)
        .ok_or_else(|| TimelineError::UnknownClip(id.to_string()))?;
    Ok(&mut tl.overlay_tracks[t].clips[c])
}

/// Moves every narration clip linked to a video clip by the shift that clip just
/// received. Unlinked audio is untouched.
fn ripple_linked(
    tl: &mut Timeline,
    before: &std::collections::BTreeMap<String, i64>,
) -> Result<(), TimelineError> {
    let after = tl.video_starts();
    let fps = tl.fps;
    for track in &mut tl.audio_tracks {
        for clip in &mut track.clips {
            let Some(link) = &clip.link else { continue };
            let (Some(b), Some(a)) = (before.get(link), after.get(link)) else {
                continue;
            };
            if a != b {
                let shift = fps.frame_to_ticks(*a).0 - fps.frame_to_ticks(*b).0;
                clip.start = Ticks(clip.start.0 + shift);
                if clip.start.0 < 0 {
                    return Err(TimelineError::NegativeStart(clip.id.clone()));
                }
            }
        }
    }
    Ok(())
}

fn mutate(tl: &mut Timeline, action: &Action) -> Result<(), TimelineError> {
    match action {
        Action::RegisterSource { source } => {
            if let Some(existing) = tl.source(&source.id) {
                if existing == source {
                    return Ok(());
                }
                return Err(TimelineError::DuplicateId(source.id.to_string()));
            }
            tl.sources.push(source.clone());
            tl.sources.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Action::SetCards { cards } => {
            tl.cards = cards.clone();
            // A fill follows its card: replacing the cards drops fills of cards that are gone.
            tl.slot_fills
                .retain(|id, _| cards.iter().any(|c| &c.id == id));
        }
        Action::SetSlotFill { card_id, fill } => match fill {
            Some(f) => {
                tl.slot_fills.insert(card_id.clone(), f.clone());
            }
            None => {
                tl.slot_fills.remove(card_id);
            }
        },
        Action::AttachMicrophone { attachment } => {
            if let Some(existing) = tl
                .mic_attachments
                .iter_mut()
                .find(|a| a.camera == attachment.camera)
            {
                if *existing == *attachment {
                    return Ok(());
                }
                *existing = attachment.clone();
            } else {
                tl.mic_attachments.push(attachment.clone());
            }
            tl.mic_attachments.sort_by(|a, b| a.camera.cmp(&b.camera));
        }
        Action::AddVideoTrack { track_id } => tl.video_tracks.push(VideoTrack {
            id: track_id.clone(),
            clips: Vec::new(),
        }),
        Action::AddAudioTrack { track_id, role } => tl.audio_tracks.push(AudioTrack {
            id: track_id.clone(),
            role: *role,
            clips: Vec::new(),
        }),
        Action::InsertClip {
            track_id,
            index,
            clip,
        } => {
            let before = tl.video_starts();
            let track = tl
                .video_tracks
                .iter_mut()
                .find(|t| &t.id == track_id)
                .ok_or_else(|| TimelineError::UnknownTrack(track_id.clone()))?;
            if *index > track.clips.len() {
                return Err(TimelineError::IndexOutOfRange {
                    index: *index,
                    len: track.clips.len(),
                });
            }
            track.clips.insert(*index, clip.clone());
            ripple_linked(tl, &before)?;
        }
        Action::RemoveClip { clip_id } => {
            let (t, c) = video_clip_pos(tl, clip_id)?;
            let before = tl.video_starts();
            tl.video_tracks[t].clips.remove(c);
            for track in &mut tl.audio_tracks {
                for clip in &mut track.clips {
                    if clip.link.as_deref() == Some(clip_id.as_str()) {
                        clip.link = None;
                    }
                }
            }
            ripple_linked(tl, &before)?;
        }
        Action::TrimClip {
            clip_id,
            source_in,
            source_out,
        } => {
            let (t, c) = video_clip_pos(tl, clip_id)?;
            let before = tl.video_starts();
            let clip = &mut tl.video_tracks[t].clips[c];
            clip.source_in = *source_in;
            clip.source_out = *source_out;
            ripple_linked(tl, &before)?;
        }
        Action::MoveClip { clip_id, to_index } => {
            let (t, c) = video_clip_pos(tl, clip_id)?;
            let before = tl.video_starts();
            let clip = tl.video_tracks[t].clips.remove(c);
            let len = tl.video_tracks[t].clips.len();
            if *to_index > len {
                return Err(TimelineError::IndexOutOfRange {
                    index: *to_index,
                    len,
                });
            }
            tl.video_tracks[t].clips.insert(*to_index, clip);
            ripple_linked(tl, &before)?;
        }
        Action::AddAudioClip { track_id, clip } => {
            let fps = tl.fps;
            let track = tl
                .audio_tracks
                .iter_mut()
                .find(|t| &t.id == track_id)
                .ok_or_else(|| TimelineError::UnknownTrack(track_id.clone()))?;
            let mut clip = clip.clone();
            clip.start = fps.snap(clip.start);
            track.clips.push(clip);
        }
        Action::RemoveAudioClip { clip_id } => {
            let (t, c) = tl
                .find_audio_clip(clip_id)
                .ok_or_else(|| TimelineError::UnknownClip(clip_id.clone()))?;
            tl.audio_tracks[t].clips.remove(c);
        }
        Action::MoveAudioClip { clip_id, start } => {
            let fps = tl.fps;
            audio_clip_mut(tl, clip_id)?.start = fps.snap(*start);
        }
        Action::TrimAudioClip {
            clip_id,
            trim_in,
            trim_out,
        } => {
            let clip = audio_clip_mut(tl, clip_id)?;
            clip.trim_in = *trim_in;
            clip.trim_out = *trim_out;
        }
        Action::SetGain { clip_id, gain } => {
            if tl.find_video_clip(clip_id).is_some() {
                tl.corrections
                    .audio
                    .entry(clip_id.clone())
                    .or_default()
                    .gain = *gain;
            } else {
                audio_clip_mut(tl, clip_id)?.gain = *gain;
            }
        }
        Action::SetMute { clip_id, muted } => {
            if tl.find_video_clip(clip_id).is_none() && tl.find_audio_clip(clip_id).is_none() {
                return Err(TimelineError::UnknownClip(clip_id.clone()));
            }
            tl.corrections
                .audio
                .entry(clip_id.clone())
                .or_default()
                .muted = *muted;
        }
        Action::SetClipVisuals { clip_id, visuals } => {
            if tl.find_video_clip(clip_id).is_none() {
                return Err(TimelineError::UnknownClip(clip_id.clone()));
            }
            visuals.validate(clip_id)?;
            if visuals.is_empty() {
                tl.corrections.visuals.remove(clip_id);
            } else {
                tl.corrections
                    .visuals
                    .insert(clip_id.clone(), visuals.clone());
            }
        }
        Action::PositionTitle { clip_id, position } => {
            title_mut(tl, clip_id)?;
            tl.corrections.positions.insert(clip_id.clone(), *position);
        }
        Action::EditCaption { cue_id, correction } => {
            tl.corrections
                .captions
                .insert(cue_id.clone(), correction.clone());
        }
        Action::RemoveCaption { cue_id } => {
            if tl.corrections.captions.remove(cue_id).is_none() {
                return Err(TimelineError::InvalidRange(
                    cue_id.clone(),
                    "no caption correction to restore".into(),
                ));
            }
        }
        Action::SwapTake {
            clip_id,
            source,
            source_in,
            source_out,
        } => {
            let (t, c) = video_clip_pos(tl, clip_id)?;
            let before = tl.video_starts();
            let old = tl.video_tracks[t].clips[c].clone();
            let clip = &mut tl.video_tracks[t].clips[c];
            clip.source = source.clone();
            clip.source_in = *source_in;
            clip.source_out = *source_out;
            for track in &mut tl.audio_tracks {
                for audio in &mut track.clips {
                    if audio.link.as_deref() == Some(clip_id)
                        && audio.source == old.source
                        && audio.trim_in == old.source_in
                        && audio.trim_out == old.source_out
                    {
                        audio.source = source.clone();
                        audio.trim_in = *source_in;
                        audio.trim_out = *source_out;
                    }
                }
            }
            ripple_linked(tl, &before)?;
        }
        Action::LinkNarration { audio_id, video_id } => {
            video_clip_pos(tl, video_id)?;
            audio_clip_mut(tl, audio_id)?.link = Some(video_id.clone());
        }
        Action::UnlinkNarration { audio_id } => audio_clip_mut(tl, audio_id)?.link = None,
        Action::SetUnder { audio_id, under } => audio_clip_mut(tl, audio_id)?.under = *under,
        Action::AddTitle { track_id, clip } => {
            if !tl.overlay_tracks.iter().any(|t| &t.id == track_id) {
                tl.overlay_tracks.push(OverlayTrack {
                    id: track_id.clone(),
                    clips: Vec::new(),
                });
            }
            let track = tl
                .overlay_tracks
                .iter_mut()
                .find(|t| &t.id == track_id)
                .expect("track exists");
            track.clips.push(clip.clone());
        }
        Action::RemoveTitle { clip_id } => {
            let (t, c) = tl
                .find_title(clip_id)
                .ok_or_else(|| TimelineError::UnknownClip(clip_id.clone()))?;
            tl.overlay_tracks[t].clips.remove(c);
        }
        Action::MoveTitle {
            clip_id,
            start_frame,
        } => title_mut(tl, clip_id)?.start_frame = *start_frame,
        Action::TrimTitle {
            clip_id,
            start_frame,
            frames,
        } => {
            let clip = title_mut(tl, clip_id)?;
            clip.start_frame = *start_frame;
            clip.frames = *frames;
        }
        Action::EditTitle {
            clip_id,
            template,
            fade_in_frames,
            fade_out_frames,
        } => {
            let clip = title_mut(tl, clip_id)?;
            clip.template = template.clone();
            clip.fade_in_frames = *fade_in_frames;
            clip.fade_out_frames = *fade_out_frames;
        }
        Action::SplitClip {
            clip_id,
            at,
            new_clip_id,
        } => {
            let (t, c) = video_clip_pos(tl, clip_id)?;
            let before = tl.video_starts();
            let (src_in, src_out) = {
                let clip = &tl.video_tracks[t].clips[c];
                (clip.source_in, clip.source_out)
            };
            let span = src_out.0 - src_in.0;
            if at.0 <= 0 || at.0 >= span {
                return Err(TimelineError::InvalidRange(
                    clip_id.clone(),
                    "split point outside clip".into(),
                ));
            }
            let right = VideoClip {
                id: new_clip_id.clone(),
                source: tl.video_tracks[t].clips[c].source.clone(),
                source_in: Ticks(src_in.0 + at.0),
                source_out: src_out,
            };
            tl.video_tracks[t].clips[c].source_out = Ticks(src_in.0 + at.0);
            tl.video_tracks[t].clips.insert(c + 1, right);
            if let Some(audio) = tl.corrections.audio.get(clip_id).cloned() {
                tl.corrections.audio.insert(new_clip_id.clone(), audio);
            }
            if let Some(keys) = tl.corrections.visuals.get(clip_id).cloned() {
                let (left, right) = keys.split_at(*at);
                put_visuals(&mut tl.corrections.visuals, clip_id, left);
                put_visuals(&mut tl.corrections.visuals, new_clip_id, right);
            }
            ripple_linked(tl, &before)?;
        }
        Action::SplitAudioClip {
            clip_id,
            at,
            new_clip_id,
        } => {
            let (t, c) = tl
                .find_audio_clip(clip_id)
                .ok_or_else(|| TimelineError::UnknownClip(clip_id.clone()))?;
            let clip = &tl.audio_tracks[t].clips[c];
            if at.0 <= 0 || at.0 >= clip.duration().0 {
                return Err(TimelineError::InvalidRange(
                    clip_id.clone(),
                    "split point outside clip".into(),
                ));
            }
            let mut right = clip.clone();
            right.id = new_clip_id.clone();
            right.trim_in = Ticks(clip.trim_in.0 + at.0);
            right.start = Ticks(clip.start.0 + at.0);
            tl.audio_tracks[t].clips[c].trim_out = right.trim_in;
            tl.audio_tracks[t].clips.push(right);
            if let Some(audio) = tl.corrections.audio.get(clip_id).cloned() {
                tl.corrections.audio.insert(new_clip_id.clone(), audio);
            }
        }
        Action::SlipClip { clip_id, delta } => {
            let (t, c) = video_clip_pos(tl, clip_id)?;
            let clip = &mut tl.video_tracks[t].clips[c];
            clip.source_in = Ticks(clip.source_in.0 + delta.0);
            clip.source_out = Ticks(clip.source_out.0 + delta.0);
            if let Some(keys) = tl.corrections.visuals.get(clip_id).cloned() {
                put_visuals(&mut tl.corrections.visuals, clip_id, keys.slipped(*delta));
            }
        }
        Action::SlipAudioClip { clip_id, delta } => {
            let clip = audio_clip_mut(tl, clip_id)?;
            clip.trim_in = Ticks(clip.trim_in.0 + delta.0);
            clip.trim_out = Ticks(clip.trim_out.0 + delta.0);
        }
        Action::RippleDeleteRange { start, end } => {
            ripple_delete_range(tl, *start, *end)?;
        }
        Action::Batch { actions } => {
            for a in actions {
                mutate(tl, a)?;
            }
        }
    }
    // Removed IDs cannot leak settings into a later clip reusing that ID.
    let audio_ids: std::collections::BTreeSet<_> = tl
        .video_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id.clone()))
        .chain(
            tl.audio_tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(|c| c.id.clone())),
        )
        .collect();
    tl.corrections.audio.retain(|id, _| audio_ids.contains(id));
    let video_ids: std::collections::BTreeSet<_> = tl
        .video_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id.clone()))
        .collect();
    tl.corrections
        .visuals
        .retain(|id, _| video_ids.contains(id));
    let title_ids: std::collections::BTreeSet<_> = tl
        .overlay_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id.clone()))
        .collect();
    tl.corrections
        .positions
        .retain(|id, _| title_ids.contains(id));
    Ok(())
}

/// Stores a clip's keyed visuals, or drops the entry when nothing is keyed, so
/// projects without visual keys keep their canonical shape.
fn put_visuals(
    visuals: &mut std::collections::BTreeMap<String, ClipVisualKeys>,
    clip_id: &str,
    keys: ClipVisualKeys,
) {
    if keys.is_empty() {
        visuals.remove(clip_id);
    } else {
        visuals.insert(clip_id.to_string(), keys);
    }
}

/// Deterministic id for a clip produced by a split inside [`RippleDeleteRange`]:
/// `{clip_id}~r`, or `~r2`, `~r3`, … when that id is already taken. `used` holds
/// every clip id across all tracks plus ids minted so far.
fn split_id(used: &mut std::collections::BTreeSet<String>, clip_id: &str) -> String {
    for n in 1.. {
        let id = if n == 1 {
            format!("{clip_id}~r")
        } else {
            format!("{clip_id}~r{n}")
        };
        if used.insert(id.clone()) {
            return id;
        }
    }
    unreachable!()
}

/// Removes output range `[start, end)` from every video track it covers.
/// `start`/`end` are output ticks converted to whole frames: a partially
/// covered frame is removed with the range (transcript word edges rarely land
/// on frame boundaries, and keeping a sliver frame would fail validation).
fn ripple_delete_range(tl: &mut Timeline, start: Ticks, end: Ticks) -> Result<(), TimelineError> {
    if start.0 < 0 || end.0 <= start.0 {
        return Err(TimelineError::InvalidRange(
            "ripple_delete_range".into(),
            "empty or negative range".into(),
        ));
    }
    let fps = tl.fps;
    let (sf, ef) = (fps.ticks_to_frame(start), fps.ticks_to_frame(end));
    if ef <= sf {
        return Err(TimelineError::InvalidRange(
            "ripple_delete_range".into(),
            "range is shorter than one frame".into(),
        ));
    }
    let before = tl.video_starts();
    let mut used: std::collections::BTreeSet<String> = tl
        .video_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id.clone()))
        .chain(
            tl.overlay_tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(|c| c.id.clone())),
        )
        .chain(
            tl.audio_tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(|c| c.id.clone())),
        )
        .collect();
    let mut removed_ids = Vec::new();
    for track in &mut tl.video_tracks {
        let mut cursor = 0i64;
        let mut i = 0;
        while i < track.clips.len() {
            let clip = &track.clips[i];
            let cf = fps.ticks_to_frame(Ticks(clip.source_out.0 - clip.source_in.0));
            let (cs, ce) = (cursor, cursor + cf);
            cursor = ce;
            let (os, oe) = (sf.max(cs), ef.min(ce));
            if oe <= os {
                i += 1;
                continue;
            }
            let clip = track.clips[i].clone();
            let keep_left = os > cs;
            let keep_right = oe < ce;
            match (keep_left, keep_right) {
                (true, true) => {
                    // Split out the middle: left keeps [in, in+left), right
                    // keeps [in+right, out) of the source range.
                    let left_ticks = fps.frame_to_ticks(os - cs);
                    let right_ticks = fps.frame_to_ticks(oe - cs);
                    let right = VideoClip {
                        id: split_id(&mut used, &clip.id),
                        source: clip.source.clone(),
                        source_in: Ticks(clip.source_in.0 + right_ticks.0),
                        source_out: clip.source_out,
                    };
                    if let Some(audio) = tl.corrections.audio.get(&clip.id).cloned() {
                        tl.corrections.audio.insert(right.id.clone(), audio);
                    }
                    if let Some(keys) = tl.corrections.visuals.get(&clip.id).cloned() {
                        let (kept, _) = keys.split_at(left_ticks);
                        let (_, moved) = keys.split_at(right_ticks);
                        put_visuals(&mut tl.corrections.visuals, &clip.id, kept);
                        put_visuals(&mut tl.corrections.visuals, &right.id, moved);
                    }
                    track.clips[i].source_out = Ticks(clip.source_in.0 + left_ticks.0);
                    track.clips.insert(i + 1, right);
                    i += 2;
                }
                (true, false) => {
                    track.clips[i].source_out =
                        Ticks(clip.source_in.0 + fps.frame_to_ticks(os - cs).0);
                    i += 1;
                }
                (false, true) => {
                    let trimmed = fps.frame_to_ticks(oe - cs);
                    track.clips[i].source_in = Ticks(clip.source_in.0 + trimmed.0);
                    // Removing the head moves the source window forward, like a slip.
                    if let Some(keys) = tl.corrections.visuals.get(&clip.id).cloned() {
                        put_visuals(&mut tl.corrections.visuals, &clip.id, keys.slipped(trimmed));
                    }
                    i += 1;
                }
                (false, false) => {
                    removed_ids.push(track.clips.remove(i).id);
                }
            }
        }
    }
    if removed_ids.is_empty() && before == tl.video_starts() {
        return Ok(());
    }
    // Narration linked to a removed clip unlinks and stays put (RemoveClip rule).
    for track in &mut tl.audio_tracks {
        for clip in &mut track.clips {
            if let Some(link) = &clip.link {
                if removed_ids.iter().any(|id| id == link) {
                    clip.link = None;
                }
            }
        }
    }
    ripple_linked(tl, &before)
}
