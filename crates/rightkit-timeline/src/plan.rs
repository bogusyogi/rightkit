//! Render plan compiler: timeline -> typed, encoder-agnostic plan.
//!
//! The plan is plain data. It contains no paths, presets, or encoders; those are
//! bound later by [`crate::ffmpeg`]. All counts of frames come from the timeline's
//! shared rounding rule.

use serde::{Deserialize, Serialize};

use crate::error::TimelineError;
use crate::model::{
    AudioClip, AudioRole, CardAudio, CardSlot, ClipVisualKeys, SlotRect, SourceId, Timeline,
    TitleTemplate, UnderAudio,
};
use crate::time::{Fps, MilliDb, Ticks};

pub const PLAN_SCHEMA_VERSION: u32 = 1;
/// Mix sample rate used by every plan.
pub const MIX_SAMPLE_RATE: i64 = 48_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanInput {
    pub source: SourceId,
    pub has_video: bool,
    pub has_audio: bool,
    #[serde(default = "zero_ticks")]
    pub duration: Ticks,
    /// Display width/height (0 when the source has no video).
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VideoSegment {
    /// `frames` frames of `input` starting at `source_in`, resampled to project fps.
    /// `visuals` carries the clip's keyed opacity, transform, crop and cross-fade
    /// (clip-local key times); absent when the clip has no keyed visuals.
    Clip {
        clip_id: String,
        input: usize,
        source_in: Ticks,
        frames: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visuals: Option<ClipVisualKeys>,
    },
    /// Repeats the last frame of the preceding segments (narration outran the video).
    Hold { frames: i64 },
    /// Black frames (no video at all).
    Black { frames: i64 },
}

impl VideoSegment {
    pub fn frames(&self) -> i64 {
        match self {
            VideoSegment::Clip { frames, .. }
            | VideoSegment::Hold { frames }
            | VideoSegment::Black { frames } => *frames,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerRole {
    SourceAudio,
    Narration,
    Music,
    /// The room-tone bed mixed under cut gaps (see [`RoomToneBed`]).
    RoomTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Attenuation {
    Duck { amount: MilliDb },
    Mute,
}

/// Output-time window in which a layer is attenuated (ducked or muted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuckWindow {
    pub start: Ticks,
    pub end: Ticks,
    pub attenuation: Attenuation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioLayer {
    pub id: String,
    pub role: LayerRole,
    pub input: usize,
    pub source_in: Ticks,
    pub duration: Ticks,
    /// Output start.
    pub start: Ticks,
    pub gain: MilliDb,
    /// Attenuation windows in output time, applied after placement.
    pub windows: Vec<DuckWindow>,
}

/// Audio mix graph: a silent base of exactly `total` length plus the layers; the
/// gaps before, between, and after layers are therefore silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioMix {
    pub sample_rate: i64,
    pub total: Ticks,
    pub layers: Vec<AudioLayer>,
    /// Programme span in output time: after the opening cards, before the ending cards.
    #[serde(default = "zero_ticks")]
    pub programme_start: Ticks,
    #[serde(default = "zero_ticks")]
    pub programme_end: Ticks,
    /// Room-tone bed under the programme's cut gaps. Set by the render path only when the
    /// project opts in (`room_tone: true`); absent means silence in the gaps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_tone: Option<RoomToneBed>,
}

fn zero_ticks() -> Ticks {
    Ticks::ZERO
}

/// A WAV of exactly `audio.total` length at the engine's room-tone rate, zero outside the
/// gaps. The render mixes it as a [`LayerRole::RoomTone`] layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomToneBed {
    pub file: std::path::PathBuf,
}

/// A programme-time interval with no audio layer: a cut gap. `input` is the nearest
/// preceding layer's plan input, or the first layer's input when no layer precedes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomToneGap {
    pub start: Ticks,
    pub duration: Ticks,
    pub input: usize,
}

/// The programme's cut gaps: uncovered stretches of `programme_start..programme_end`.
pub fn room_tone_gaps(mix: &AudioMix) -> Vec<RoomToneGap> {
    let mut spans: Vec<(i64, i64, usize)> = mix
        .layers
        .iter()
        .map(|l| (l.start.0, l.start.0 + l.duration.0, l.input))
        .collect();
    spans.sort();
    let (ps, pe) = (mix.programme_start.0, mix.programme_end.0);
    let mut found: Vec<(i64, i64, Option<usize>)> = Vec::new();
    let mut cursor = ps;
    let mut before: Option<usize> = None;
    for (start, end, input) in spans {
        if cursor >= pe {
            break;
        }
        if start > cursor {
            let stop = start.min(pe);
            if stop > cursor {
                found.push((cursor, stop, before));
            }
        }
        if end > cursor {
            cursor = end;
            before = Some(input);
        }
    }
    if cursor < pe {
        found.push((cursor, pe, before));
    }
    let first = mix.layers.iter().min_by_key(|l| l.start.0).map(|l| l.input);
    found
        .into_iter()
        .filter_map(|(s, e, b)| {
            b.or(first).map(|input| RoomToneGap {
                start: Ticks(s),
                duration: Ticks(e - s),
                input,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayItem {
    pub id: String,
    pub start_frame: i64,
    pub frames: i64,
    pub template: TitleTemplate,
    pub fade_in_frames: i64,
    pub fade_out_frames: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<crate::TextPosition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderPlan {
    pub schema_version: u32,
    pub fps: Fps,
    pub total_frames: i64,
    pub inputs: Vec<PlanInput>,
    pub video: Vec<VideoSegment>,
    pub audio: AudioMix,
    /// Titles in draw order (track order, then clip order); later items draw on top.
    pub overlays: Vec<OverlayItem>,
    /// Opening and end cards in output order. `start_frame` is an output frame.
    /// Card frames are not in `video`: `video` covers the programme only, and the
    /// renderer places each card before or after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<CardSpan>,
}

/// One card's place in the output: a solid colour over `frames` output frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSpan {
    pub id: String,
    pub slot: CardSlot,
    pub start_frame: i64,
    pub frames: i64,
    pub colour: String,
    /// Media in the card's slot, drawn over the colour; absent for a plain card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<CardFillSpan>,
}

/// A card slot's media: plan input `input`, played from `source_in`, fitted inside
/// `rect` (permille of the output frame) for the card's `frames`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardFillSpan {
    pub input: usize,
    pub source_in: Ticks,
    pub rect: SlotRect,
}

/// The clip with its output start moved by `off` (programme time to output time).
fn shifted_audio(clip: &AudioClip, off: Ticks) -> AudioClip {
    AudioClip {
        start: Ticks(clip.start.0 + off.0),
        ..clip.clone()
    }
}

struct InputTable {
    inputs: Vec<PlanInput>,
}

impl InputTable {
    fn index(&mut self, tl: &Timeline, id: &SourceId) -> Result<usize, TimelineError> {
        if let Some(i) = self.inputs.iter().position(|p| &p.source == id) {
            return Ok(i);
        }
        let s = tl
            .source(id)
            .ok_or_else(|| TimelineError::UnknownSource(id.to_string()))?;
        self.inputs.push(PlanInput {
            source: id.clone(),
            has_video: s.video.is_some(),
            has_audio: s.has_audio,
            duration: s
                .video
                .as_ref()
                .and_then(|v| v.duration)
                .unwrap_or(s.duration),
            width: s.video.as_ref().map(|v| v.width).unwrap_or(0),
            height: s.video.as_ref().map(|v| v.height).unwrap_or(0),
        });
        Ok(self.inputs.len() - 1)
    }
}

/// Compiles a validated timeline into a render plan.
pub fn compile(tl: &Timeline) -> Result<RenderPlan, TimelineError> {
    tl.validate()?;
    let populated: Vec<_> = tl
        .video_tracks
        .iter()
        .filter(|t| !t.clips.is_empty())
        .collect();
    if populated.len() > 1 {
        return Err(TimelineError::Unsupported(
            "more than one populated video track (compositing is not part of step 1)".into(),
        ));
    }
    let programme_frames = tl.total_frames();
    if programme_frames < 1 {
        return Err(TimelineError::EmptyTimeline);
    }
    let fps = tl.fps;
    let head_frames = tl.card_frames(CardSlot::Opening);
    let tail_frames = tl.card_frames(CardSlot::Ending);
    let total_frames = head_frames + programme_frames + tail_frames;
    // Programme time moves after the opening cards. Music moves with it unless an
    // opening card carries the bed, which then runs straight through the card.
    let shift = fps.frame_to_ticks(head_frames);
    let music_shift = if tl
        .cards_in(CardSlot::Opening)
        .any(|c| c.audio == CardAudio::Bed)
    {
        Ticks::ZERO
    } else {
        shift
    };
    let mut table = InputTable { inputs: Vec::new() };
    let mut video = Vec::new();
    let mut source_layers = Vec::new();
    let mut cursor = 0;
    if let Some(track) = populated.first() {
        for (clip, place) in track.clips.iter().zip(tl.placements(track)) {
            let input = table.index(tl, &clip.source)?;
            // Auto-edit normally emits an explicit linked AudioClip for a
            // camera occurrence. When it does, that layer is mapped below;
            // suppress the implicit source layer here so camera audio cannot
            // leak alongside the attached microphone.
            let has_linked_camera_audio = tl.audio_tracks.iter().any(|track| {
                track.clips.iter().any(|audio| {
                    audio.link.as_deref() == Some(clip.id.as_str()) && audio.source == clip.source
                })
            });
            let visuals = tl
                .corrections
                .visuals
                .get(&clip.id)
                .filter(|keys| !keys.is_empty())
                .cloned();
            video.push(VideoSegment::Clip {
                clip_id: clip.id.clone(),
                input,
                source_in: clip.source_in,
                frames: place.frames,
                visuals,
            });
            // A source-level microphone attachment replaces camera audio for
            // every occurrence. The mapped mic range can start before or end
            // after the mic file; omit those portions and let the exact-length
            // silent base preserve leading/trailing gaps.
            let clip_duration = fps
                .frame_to_ticks(place.frames)
                .min(Ticks(clip.source_out.0 - clip.source_in.0));
            let output_start = Ticks(fps.frame_to_ticks(place.start_frame).0 + shift.0);
            if let Some(attachment) = tl.mic_attachment(&clip.source) {
                if has_linked_camera_audio {
                    cursor = place.start_frame + place.frames;
                    continue;
                }
                let mic_input = table.index(tl, &attachment.microphone)?;
                let mic_duration = tl
                    .source(&attachment.microphone)
                    .map(|source| source.duration)
                    .unwrap_or(Ticks::ZERO);
                let invalid_offset = || {
                    TimelineError::InvalidRange(
                        clip.id.clone(),
                        "microphone offset exceeds timestamp range".into(),
                    )
                };
                let wanted_start = Ticks(
                    clip.source_in
                        .0
                        .checked_add(attachment.offset.0)
                        .ok_or_else(invalid_offset)?,
                );
                let wanted_end = Ticks(
                    wanted_start
                        .0
                        .checked_add(clip_duration.0)
                        .ok_or_else(invalid_offset)?,
                );
                let overlap_start = wanted_start.max(Ticks::ZERO);
                let overlap_end = wanted_end.min(mic_duration);
                if overlap_end > overlap_start {
                    let head = overlap_start
                        .0
                        .checked_sub(wanted_start.0)
                        .ok_or_else(invalid_offset)?;
                    let start = Ticks(
                        output_start
                            .0
                            .checked_add(head)
                            .ok_or_else(invalid_offset)?,
                    );
                    source_layers.push(crate::plan::AudioLayer {
                        id: clip.id.clone(),
                        role: LayerRole::SourceAudio,
                        input: mic_input,
                        source_in: overlap_start,
                        duration: Ticks(overlap_end.0 - overlap_start.0),
                        start,
                        gain: MilliDb::ZERO,
                        windows: Vec::new(),
                    });
                }
            } else if table.inputs[input].has_audio {
                source_layers.push(crate::plan::AudioLayer {
                    id: clip.id.clone(),
                    role: LayerRole::SourceAudio,
                    input,
                    source_in: clip.source_in,
                    // Output frames may round up beyond a precise source trim.
                    // Never read removed audio into that final fractional frame;
                    // the mix's silent base still fills the output duration.
                    duration: clip_duration,
                    start: output_start,
                    gain: MilliDb::ZERO,
                    windows: Vec::new(),
                });
            }
            cursor = place.start_frame + place.frames;
        }
    }
    if cursor < programme_frames {
        let frames = programme_frames - cursor;
        video.push(if cursor == 0 {
            VideoSegment::Black { frames }
        } else {
            VideoSegment::Hold { frames }
        });
    }

    let mut layers = Vec::new();
    for track in &tl.audio_tracks {
        let off = if track.role == AudioRole::Music {
            music_shift
        } else {
            shift
        };
        for programme_clip in &track.clips {
            let clip = &shifted_audio(programme_clip, off);
            let input = table.index(tl, &clip.source)?;
            if track.role == AudioRole::Narration {
                let attenuation = match clip.under {
                    UnderAudio::Keep => None,
                    UnderAudio::Duck { amount } => Some(Attenuation::Duck { amount }),
                    UnderAudio::Mute => Some(Attenuation::Mute),
                };
                if let Some(attenuation) = attenuation {
                    for layer in &mut source_layers {
                        let l_end = Ticks(layer.start.0 + layer.duration.0);
                        if clip.start < l_end && layer.start < clip.end() {
                            layer.windows.push(DuckWindow {
                                start: clip.start.max(layer.start),
                                end: clip.end().min(l_end),
                                attenuation,
                            });
                        }
                    }
                }
            }
            // A linked camera audio clip is the explicit occurrence mapping
            // produced by automatic editing. If its camera source has an
            // external microphone attachment, route this layer to the mic
            // source while preserving the clip's gain, timing, corrections,
            // and under-audio semantics.
            let linked_camera_source = clip.link.as_ref().and_then(|link| {
                tl.find_video_clip(link)
                    .map(|(track, index)| tl.video_tracks[track].clips[index].source.clone())
            });
            let attachment = linked_camera_source
                .as_ref()
                .filter(|source| **source == clip.source)
                .and_then(|source| tl.mic_attachment(source));
            if let Some(attachment) = attachment {
                let mic_input = table.index(tl, &attachment.microphone)?;
                let mic_duration = tl
                    .source(&attachment.microphone)
                    .map(|source| source.duration)
                    .ok_or_else(|| {
                        TimelineError::UnknownSource(attachment.microphone.to_string())
                    })?;
                let invalid_offset = || {
                    TimelineError::InvalidRange(
                        clip.id.clone(),
                        "microphone offset exceeds timestamp range".into(),
                    )
                };
                let wanted_start = Ticks(
                    clip.trim_in
                        .0
                        .checked_add(attachment.offset.0)
                        .ok_or_else(invalid_offset)?,
                );
                let wanted_end = Ticks(
                    clip.trim_out
                        .0
                        .checked_add(attachment.offset.0)
                        .ok_or_else(invalid_offset)?,
                );
                let overlap_start = wanted_start.max(Ticks::ZERO);
                let overlap_end = wanted_end.min(mic_duration);
                if overlap_end > overlap_start {
                    let head = overlap_start
                        .0
                        .checked_sub(wanted_start.0)
                        .ok_or_else(invalid_offset)?;
                    let start = Ticks(clip.start.0.checked_add(head).ok_or_else(invalid_offset)?);
                    layers.push(AudioLayer {
                        id: clip.id.clone(),
                        role: match track.role {
                            AudioRole::Narration => LayerRole::Narration,
                            AudioRole::Music => LayerRole::Music,
                        },
                        input: mic_input,
                        source_in: overlap_start,
                        duration: Ticks(overlap_end.0 - overlap_start.0),
                        start,
                        gain: clip.gain,
                        windows: Vec::new(),
                    });
                }
            } else {
                layers.push(AudioLayer {
                    id: clip.id.clone(),
                    role: match track.role {
                        AudioRole::Narration => LayerRole::Narration,
                        AudioRole::Music => LayerRole::Music,
                    },
                    input,
                    source_in: clip.trim_in,
                    duration: clip.duration(),
                    start: clip.start,
                    gain: clip.gain,
                    windows: Vec::new(),
                });
            }
        }
    }
    // Music beds duck under narration too: a narration clip's `under` attenuation
    // covers music-role layers overlapping it, same as source audio.
    for track in &tl.audio_tracks {
        if track.role != AudioRole::Narration {
            continue;
        }
        for programme_clip in &track.clips {
            let clip = &shifted_audio(programme_clip, shift);
            let attenuation = match clip.under {
                UnderAudio::Keep => None,
                UnderAudio::Duck { amount } => Some(Attenuation::Duck { amount }),
                UnderAudio::Mute => Some(Attenuation::Mute),
            };
            let Some(attenuation) = attenuation else {
                continue;
            };
            for layer in layers.iter_mut().filter(|l| l.role == LayerRole::Music) {
                let l_end = Ticks(layer.start.0 + layer.duration.0);
                if clip.start < l_end && layer.start < clip.end() {
                    layer.windows.push(DuckWindow {
                        start: clip.start.max(layer.start),
                        end: clip.end().min(l_end),
                        attenuation,
                    });
                }
            }
        }
    }
    let mut all_layers = source_layers;
    all_layers.extend(layers);
    // Silent closing card: the bed stops at the programme's end, so mute it there.
    if tl
        .cards_in(CardSlot::Ending)
        .any(|c| c.audio == CardAudio::Silence)
    {
        let from = fps.frame_to_ticks(head_frames + programme_frames);
        let to = fps.frame_to_ticks(total_frames);
        for layer in all_layers.iter_mut().filter(|l| l.role == LayerRole::Music) {
            layer.windows.push(DuckWindow {
                start: from,
                end: to,
                attenuation: Attenuation::Mute,
            });
        }
    }
    for layer in &mut all_layers {
        if let Some(correction) = tl.corrections.audio.get(&layer.id) {
            if layer.role == LayerRole::SourceAudio {
                layer.gain = correction.gain;
            }
            if correction.muted {
                layer.windows.push(DuckWindow {
                    start: layer.start,
                    end: Ticks(layer.start.0 + layer.duration.0),
                    attenuation: Attenuation::Mute,
                });
            }
        }
    }
    let mut overlays: Vec<_> = tl
        .overlay_tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| OverlayItem {
            id: c.id.clone(),
            start_frame: c.start_frame + head_frames,
            frames: c.frames,
            template: c.template.clone(),
            fade_in_frames: c.fade_in_frames,
            fade_out_frames: c.fade_out_frames,
            position: tl.corrections.positions.get(&c.id).copied(),
        })
        .collect();
    // Card titles draw on their card, after the programme's own titles.
    let mut card_spans = Vec::new();
    for (slot, base) in [
        (CardSlot::Opening, 0),
        (CardSlot::Ending, head_frames + programme_frames),
    ] {
        let mut at = base;
        for card in tl.cards_in(slot) {
            let fill = tl
                .slot_fills
                .get(&card.id)
                .map(|f| {
                    Ok::<_, TimelineError>(CardFillSpan {
                        input: table.index(tl, &f.source)?,
                        source_in: f.source_in,
                        rect: f.rect,
                    })
                })
                .transpose()?;
            card_spans.push(CardSpan {
                id: card.id.clone(),
                slot,
                start_frame: at,
                frames: card.frames,
                colour: card.colour.clone(),
                fill,
            });
            if let Some(text) = &card.title {
                overlays.push(OverlayItem {
                    id: format!("card:{}", card.id),
                    start_frame: at,
                    frames: card.frames,
                    template: TitleTemplate::Centered { text: text.clone() },
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    position: None,
                });
            }
            at += card.frames;
        }
    }
    overlays.extend(tl.corrections.captions.iter().map(|(id, c)| OverlayItem {
        id: format!("caption:{id}"),
        start_frame: c.start_frame + head_frames,
        frames: c.frames,
        template: TitleTemplate::Caption {
            text: c.text.clone(),
        },
        fade_in_frames: 0,
        fade_out_frames: 0,
        position: Some(c.position),
    }));
    Ok(RenderPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        fps,
        total_frames,
        inputs: table.inputs,
        video,
        audio: AudioMix {
            sample_rate: MIX_SAMPLE_RATE,
            total: fps.frame_to_ticks(total_frames),
            layers: all_layers,
            programme_start: shift,
            programme_end: fps.frame_to_ticks(head_frames + programme_frames),
            room_tone: None,
        },
        overlays,
        cards: card_spans,
    })
}
