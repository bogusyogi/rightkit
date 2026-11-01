//! Timeline data model: sources, video tracks, audio tracks, overlay extension point.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::TimelineError;
use crate::time::{Fps, MilliDb, Ticks};

/// Registered source identity: lowercase hex BLAKE3 of the immutable source file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceId(String);

impl SourceId {
    pub fn new(hex: impl Into<String>) -> Result<Self, TimelineError> {
        let hex = hex.into();
        let ok = hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !ok {
            return Err(TimelineError::InvalidSourceId(hex));
        }
        Ok(Self(hex))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SourceId {
    type Error = TimelineError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SourceId> for String {
    fn from(value: SourceId) -> Self {
        value.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<Ticks>,
}

/// What the timeline needs to know about a registered source (probed by the caller).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInfo {
    pub id: SourceId,
    pub duration: Ticks,
    /// `None` for audio-only sources.
    pub video: Option<VideoStreamInfo>,
    pub has_audio: bool,
}

/// A verified external microphone aligned to a camera source.
///
/// `offset` is microphone source time minus camera source time for matching
/// samples. It stays in source ticks, so every later clip occurrence can map
/// the same relationship after cuts, repeats, or reorders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MicAttachment {
    pub camera: SourceId,
    pub microphone: SourceId,
    pub offset: Ticks,
    pub sample_rate: u32,
    pub lag_samples: i64,
    pub confidence_milli: u32,
}

/// A video clip: a source range placed back to back with its neighbours (gapless track).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoClip {
    pub id: String,
    pub source: SourceId,
    pub source_in: Ticks,
    pub source_out: Ticks,
}

/// What happens to the video clips' own audio under an audio clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UnderAudio {
    /// Leave the audio underneath untouched.
    Keep,
    /// Attenuate the audio underneath by `amount` (positive thousandths of a dB).
    Duck { amount: MilliDb },
    /// Silence the audio underneath for the audio clip's span.
    Mute,
}

/// An audio clip placed at an explicit output time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioClip {
    pub id: String,
    pub source: SourceId,
    pub trim_in: Ticks,
    pub trim_out: Ticks,
    /// Output start; always frame-aligned.
    pub start: Ticks,
    pub gain: MilliDb,
    /// Video clip this narration follows when video ripples.
    pub link: Option<String>,
    pub under: UnderAudio,
}

impl AudioClip {
    pub fn duration(&self) -> Ticks {
        Ticks(self.trim_out.0 - self.trim_in.0)
    }

    pub fn end(&self) -> Ticks {
        Ticks(self.start.0 + self.duration().0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioRole {
    Narration,
    Music,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTrack {
    pub id: String,
    pub clips: Vec<VideoClip>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioTrack {
    pub id: String,
    pub role: AudioRole,
    /// Insertion order; placement is by each clip's `start`.
    pub clips: Vec<AudioClip>,
}

/// Longest accepted text field of a title, in characters.
pub const MAX_TITLE_CHARS: usize = 160;

/// A typed title template. Layout (position, size, colour, box) is fixed per
/// template and output preset in [`crate::title`]; only the text is user data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "template", rename_all = "snake_case", deny_unknown_fields)]
pub enum TitleTemplate {
    /// Left-aligned title with an optional smaller subtitle under it.
    LowerThird {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
    },
    /// Large text centred on the frame.
    Centered { text: String },
    /// Smaller text centred near the bottom (subtitle/caption style).
    Caption { text: String },
}

impl TitleTemplate {
    pub fn kind(&self) -> &'static str {
        match self {
            TitleTemplate::LowerThird { .. } => "lower_third",
            TitleTemplate::Centered { .. } => "centered",
            TitleTemplate::Caption { .. } => "caption",
        }
    }

    /// Checks every text field: non-blank, at most [`MAX_TITLE_CHARS`] characters,
    /// no control characters other than `\n` (an explicit line break).
    pub fn validate(&self, clip_id: &str) -> Result<(), TimelineError> {
        let bad = |why: &str| TimelineError::InvalidTitle(clip_id.into(), why.into());
        let check = |t: &str| -> Result<(), TimelineError> {
            if t.trim().is_empty() {
                return Err(bad("text is blank"));
            }
            if t.chars().count() > MAX_TITLE_CHARS {
                return Err(bad("text is too long"));
            }
            if t.chars().any(|c| c.is_control() && c != '\n') {
                return Err(bad("text contains a control character"));
            }
            Ok(())
        };
        match self {
            TitleTemplate::LowerThird { title, subtitle } => {
                check(title)?;
                if let Some(s) = subtitle {
                    check(s)?;
                }
            }
            TitleTemplate::Centered { text } | TitleTemplate::Caption { text } => check(text)?,
        }
        Ok(())
    }
}

/// A title: a typed template active over a frame span of the composed output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayClip {
    pub id: String,
    pub start_frame: i64,
    pub frames: i64,
    pub template: TitleTemplate,
    /// Fade-in length in frames (0 = cut in).
    #[serde(default)]
    pub fade_in_frames: i64,
    /// Fade-out length in frames (0 = cut out).
    #[serde(default)]
    pub fade_out_frames: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayTrack {
    pub id: String,
    pub clips: Vec<OverlayClip>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub fps: Fps,
    /// Sorted by id.
    pub sources: Vec<SourceInfo>,
    pub video_tracks: Vec<VideoTrack>,
    pub audio_tracks: Vec<AudioTrack>,
    pub overlay_tracks: Vec<OverlayTrack>,
    /// Opening and end cards framing the programme on V1. Card frames are not
    /// programme time: clip, audio and title times stay programme times, and the
    /// render places the cards before and after them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<CardClip>,
    /// Source-level external microphone mappings. Renderers expand these over
    /// every camera clip occurrence instead of baking one output clip here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mic_attachments: Vec<MicAttachment>,
    /// Optional correction state. Empty state is omitted to preserve existing
    /// canonical revision hashes when opening projects predating these edits.
    #[serde(default, skip_serializing_if = "EditorCorrections::is_empty")]
    pub corrections: EditorCorrections,
    /// Media filling a card's slot, keyed by card id. A card without an entry is
    /// the plain colour card; absent when empty so existing projects hash unchanged.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slot_fills: BTreeMap<String, SlotFill>,
}

/// A rectangle in permille of the output frame (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotRect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl SlotRect {
    /// The whole frame.
    pub const FULL: SlotRect = SlotRect {
        x: 0,
        y: 0,
        width: 1000,
        height: 1000,
    };

    /// Non-empty and inside the frame.
    pub fn validate(&self, id: &str) -> Result<(), TimelineError> {
        let inside = (0..=1000).contains(&self.x)
            && (0..=1000).contains(&self.y)
            && self.width >= 1
            && self.height >= 1
            && self.x + self.width <= 1000
            && self.y + self.height <= 1000;
        if inside {
            Ok(())
        } else {
            Err(TimelineError::InvalidRange(
                id.into(),
                "slot rectangle is empty or outside the frame".into(),
            ))
        }
    }
}

fn zero_ticks() -> Ticks {
    Ticks::ZERO
}

fn full_rect() -> SlotRect {
    SlotRect::FULL
}

/// What fills a card's slot: a registered video source (a still is a source made by
/// `cutright_project::still`) played from `source_in`, fitted inside `rect` over the
/// card colour. The card's own length is the slot duration: a shorter source holds its
/// last frame, a longer one is cut at the slot end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotFill {
    pub source: SourceId,
    #[serde(default = "zero_ticks")]
    pub source_in: Ticks,
    #[serde(default = "full_rect")]
    pub rect: SlotRect,
}

/// Which end of the programme a card sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardSlot {
    /// Before the first programme frame.
    Opening,
    /// After the last programme frame.
    Ending,
}

/// What the audio does under a card. `Silence` leaves the card with no audio, and
/// the programme audio (and music) is placed after it. `Bed` lets the music bed run
/// straight through an opening card; the programme's own audio still follows the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardAudio {
    Silence,
    Bed,
}

/// A solid-colour card on the V1 edge of the programme, with optional centred title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardClip {
    pub id: String,
    pub slot: CardSlot,
    /// Background colour as `#RRGGBB`.
    pub colour: String,
    /// Centred title text drawn with the project title font. Absent for a plain card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Length in output frames (at least one).
    pub frames: i64,
    pub audio: CardAudio,
}

impl CardClip {
    /// Checks the colour form, the length and the title text.
    pub fn validate(&self) -> Result<(), TimelineError> {
        let bad = |why: &str| TimelineError::InvalidRange(self.id.clone(), why.into());
        let hex = self
            .colour
            .strip_prefix('#')
            .is_some_and(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()));
        if !hex {
            return Err(bad("card colour must be #RRGGBB"));
        }
        if self.frames < 1 {
            return Err(bad("card is shorter than one frame"));
        }
        if let Some(text) = &self.title {
            TitleTemplate::Centered { text: text.clone() }.validate(&self.id)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorCorrections {
    pub captions: BTreeMap<String, CaptionCorrection>,
    pub positions: BTreeMap<String, TextPosition>,
    pub audio: BTreeMap<String, ClipAudioCorrection>,
    /// Keyed visual parameters per video clip id. Absent for every clip that
    /// has none, so projects predating these edits load and hash unchanged.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub visuals: BTreeMap<String, ClipVisualKeys>,
}

impl EditorCorrections {
    pub fn is_empty(&self) -> bool {
        self.captions.is_empty()
            && self.positions.is_empty()
            && self.audio.is_empty()
            && self.visuals.is_empty()
    }
}

/// One keyed value at a clip-local time (ticks from the clip's first frame).
/// Interpolation is linear between keys and holds outside the keyed span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipKey<T> {
    pub at: Ticks,
    pub value: T,
}

/// Keyed transform in permille: `dx`/`dy` are canvas-relative offsets
/// (-1000..=1000), `scale` is a multiplier (1..=10000, 1000 = 1.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyedTransform {
    pub dx: i64,
    pub dy: i64,
    pub scale: i64,
}

/// Keyed source crop in permille of the source frame (0..=1000 each). The
/// window must lie inside the frame and be non-empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyedCrop {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

/// Durable visual parameters of one video clip. Opacity is permille (0..=1000).
/// Empty tracks mean the clip carries no keyed value for that parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipVisualKeys {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opacity: Vec<ClipKey<i64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transform: Vec<ClipKey<KeyedTransform>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crop: Vec<ClipKey<KeyedCrop>>,
    /// Cross-fade in over this many frames from the previous clip; `0` is a cut.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cross_fade_in_frames: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

impl ClipVisualKeys {
    pub fn is_empty(&self) -> bool {
        self.opacity.is_empty()
            && self.transform.is_empty()
            && self.crop.is_empty()
            && self.cross_fade_in_frames == 0
    }

    /// Checks ranges and that keys are strictly increasing and non-negative in time.
    pub fn validate(&self, clip_id: &str) -> Result<(), TimelineError> {
        let bad = |why: &str| TimelineError::InvalidRange(clip_id.into(), why.into());
        fn ordered<T>(
            keys: &[ClipKey<T>],
            bad: &dyn Fn(&str) -> TimelineError,
        ) -> Result<(), TimelineError> {
            let mut prev: Option<i64> = None;
            for k in keys {
                if k.at.0 < 0 {
                    return Err(bad("key before clip start"));
                }
                if prev.is_some_and(|p| k.at.0 <= p) {
                    return Err(bad("keys must be strictly increasing in time"));
                }
                prev = Some(k.at.0);
            }
            Ok(())
        }
        ordered(&self.opacity, &bad)?;
        ordered(&self.transform, &bad)?;
        ordered(&self.crop, &bad)?;
        if self.opacity.iter().any(|k| !(0..=1000).contains(&k.value)) {
            return Err(bad("opacity outside 0..=1000"));
        }
        for k in &self.transform {
            let t = k.value;
            if !(-1000..=1000).contains(&t.dx) || !(-1000..=1000).contains(&t.dy) {
                return Err(bad("transform offset outside -1000..=1000"));
            }
            if !(1..=10000).contains(&t.scale) {
                return Err(bad("transform scale outside 1..=10000"));
            }
        }
        for k in &self.crop {
            let c = k.value;
            let inside = (0..=1000).contains(&c.x)
                && (0..=1000).contains(&c.y)
                && c.width >= 1
                && c.height >= 1
                && c.x + c.width <= 1000
                && c.y + c.height <= 1000;
            if !inside {
                return Err(bad("crop window outside the source frame"));
            }
        }
        if self.cross_fade_in_frames < 0 {
            return Err(bad("cross-fade length is negative"));
        }
        Ok(())
    }
}

impl ClipVisualKeys {
    /// Splits the keys at clip-local `at` (ticks from the clip's first frame).
    /// The left half keeps keys before `at` and the incoming cross-fade. The
    /// right half is rebased so its first frame is `at` and starts as a cut,
    /// since its previous clip is now the left half.
    pub fn split_at(&self, at: Ticks) -> (ClipVisualKeys, ClipVisualKeys) {
        let (lo, ro) = split_track(&self.opacity, at.0);
        let (lt, rt) = split_track(&self.transform, at.0);
        let (lc, rc) = split_track(&self.crop, at.0);
        (
            ClipVisualKeys {
                opacity: lo,
                transform: lt,
                crop: lc,
                cross_fade_in_frames: self.cross_fade_in_frames,
            },
            ClipVisualKeys {
                opacity: ro,
                transform: rt,
                crop: rc,
                cross_fade_in_frames: 0,
            },
        )
    }

    /// Keys follow the source content when a slip moves the clip's window by
    /// `delta`: a key at clip-local `t` moves to `t - delta`. A key that would
    /// fall before the clip start is replaced by the sampled value at 0.
    /// The cross-fade is unchanged.
    pub fn slipped(&self, delta: Ticks) -> ClipVisualKeys {
        ClipVisualKeys {
            opacity: shift_track(&self.opacity, delta.0),
            transform: shift_track(&self.transform, delta.0),
            crop: shift_track(&self.crop, delta.0),
            cross_fade_in_frames: self.cross_fade_in_frames,
        }
    }
}

/// Values that interpolate linearly between two keys, `num / den` of the way.
trait KeyLerp: Copy {
    fn lerp(from: Self, to: Self, num: i64, den: i64) -> Self;
}

/// `a + (b - a) * num / den`, rounded half up. i128 keeps the product exact.
fn lerp_i64(a: i64, b: i64, num: i64, den: i64) -> i64 {
    let (a, b, num, den) = (a as i128, b as i128, num as i128, den as i128);
    (a + ((b - a) * num * 2 + den).div_euclid(2 * den)) as i64
}

impl KeyLerp for i64 {
    fn lerp(from: Self, to: Self, num: i64, den: i64) -> Self {
        lerp_i64(from, to, num, den)
    }
}

impl KeyLerp for KeyedTransform {
    fn lerp(from: Self, to: Self, num: i64, den: i64) -> Self {
        KeyedTransform {
            dx: lerp_i64(from.dx, to.dx, num, den),
            dy: lerp_i64(from.dy, to.dy, num, den),
            scale: lerp_i64(from.scale, to.scale, num, den),
        }
    }
}

impl KeyLerp for KeyedCrop {
    fn lerp(from: Self, to: Self, num: i64, den: i64) -> Self {
        KeyedCrop {
            x: lerp_i64(from.x, to.x, num, den),
            y: lerp_i64(from.y, to.y, num, den),
            width: lerp_i64(from.width, to.width, num, den),
            height: lerp_i64(from.height, to.height, num, den),
        }
    }
}

/// The track's value at clip-local `t`: held before the first and after the
/// last key, linear between keys. `None` when the track has no keys.
fn sample_track<T: KeyLerp>(keys: &[ClipKey<T>], t: i64) -> Option<T> {
    let first = keys.first()?;
    let last = keys.last()?;
    if t <= first.at.0 {
        return Some(first.value);
    }
    if t >= last.at.0 {
        return Some(last.value);
    }
    // Keys are validated strictly increasing, so `i` is in 1..len here.
    let i = keys.partition_point(|k| k.at.0 <= t);
    let (a, b) = (keys[i - 1], keys[i]);
    Some(T::lerp(a.value, b.value, t - a.at.0, b.at.0 - a.at.0))
}

/// Splits one track at `at`; see [`ClipVisualKeys::split_at`].
fn split_track<T: KeyLerp>(keys: &[ClipKey<T>], at: i64) -> (Vec<ClipKey<T>>, Vec<ClipKey<T>>) {
    let Some(value) = sample_track(keys, at) else {
        return (Vec::new(), Vec::new());
    };
    let mut left: Vec<ClipKey<T>> = keys.iter().filter(|k| k.at.0 < at).copied().collect();
    if keys.iter().any(|k| k.at.0 >= at) {
        // Closes the left half at the split with the value the curve reaches there.
        left.push(ClipKey {
            at: Ticks(at),
            value,
        });
    }
    let mut right = vec![ClipKey {
        at: Ticks(0),
        value,
    }];
    right.extend(keys.iter().filter(|k| k.at.0 > at).map(|k| ClipKey {
        at: Ticks(k.at.0 - at),
        value: k.value,
    }));
    (left, right)
}

/// Moves one track with the source content by `delta`; see [`ClipVisualKeys::slipped`].
fn shift_track<T: KeyLerp>(keys: &[ClipKey<T>], delta: i64) -> Vec<ClipKey<T>> {
    if delta <= 0 {
        return keys
            .iter()
            .map(|k| ClipKey {
                at: Ticks(k.at.0 - delta),
                value: k.value,
            })
            .collect();
    }
    let Some(value) = sample_track(keys, delta) else {
        return Vec::new();
    };
    let mut out = vec![ClipKey {
        at: Ticks(0),
        value,
    }];
    out.extend(keys.iter().filter(|k| k.at.0 > delta).map(|k| ClipKey {
        at: Ticks(k.at.0 - delta),
        value: k.value,
    }));
    out
}

/// Text block centre, in permille of output dimensions (0..=1000).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPosition {
    pub x: i64,
    pub y: i64,
}

impl TextPosition {
    pub fn validate(&self) -> Result<(), TimelineError> {
        if !(0..=1000).contains(&self.x) || !(0..=1000).contains(&self.y) {
            return Err(TimelineError::InvalidRange(
                "position".into(),
                "position outside frame".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionCorrection {
    pub text: String,
    pub start_frame: i64,
    pub frames: i64,
    pub position: TextPosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipAudioCorrection {
    pub gain: MilliDb,
    pub muted: bool,
}

impl Default for ClipAudioCorrection {
    fn default() -> Self {
        Self {
            gain: MilliDb::ZERO,
            muted: false,
        }
    }
}

/// True for a caption correction key: a stable cue id (`cue-` + 16 lowercase hex,
/// optionally `-N` for a repeat) or a legacy time/text key (64 lowercase hex, see
/// [`caption_id`]). Legacy keys are migrated on read by the engine.
pub fn is_caption_key(key: &str) -> bool {
    if SourceId::new(key).is_ok() {
        return true;
    }
    let Some(rest) = key.strip_prefix("cue-") else {
        return false;
    };
    let (hex, repeat) = match rest.split_once('-') {
        Some((h, r)) => (h, Some(r)),
        None => (rest, None),
    };
    hex.len() == 16
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && repeat.is_none_or(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
}

/// Identity of an automatically mapped cue. Timing/text changes in underlying
/// footage invalidate corrections rather than applying them to another cue.
/// Legacy caption correction key (output time and text). Kept for migration.
pub fn caption_id(start_ms: u64, end_ms: u64, text: &str) -> String {
    rightkit_fs::digest::blake3_bytes(format!("{start_ms}:{end_ms}:{text}").as_bytes())
}

/// Placement of a video clip on its track, in output frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPlacement {
    pub clip_id: String,
    pub start_frame: i64,
    pub frames: i64,
}

impl Timeline {
    /// The empty project timeline: video track `V1`, narration track `A1`.
    pub fn new(fps: Fps) -> Self {
        Self {
            fps,
            sources: Vec::new(),
            video_tracks: vec![VideoTrack {
                id: "V1".into(),
                clips: Vec::new(),
            }],
            audio_tracks: vec![AudioTrack {
                id: "A1".into(),
                role: AudioRole::Narration,
                clips: Vec::new(),
            }],
            overlay_tracks: Vec::new(),
            cards: Vec::new(),
            mic_attachments: Vec::new(),
            corrections: EditorCorrections::default(),
            slot_fills: BTreeMap::new(),
        }
    }

    /// The cards in one slot, in timeline order.
    pub fn cards_in(&self, slot: CardSlot) -> impl Iterator<Item = &CardClip> {
        self.cards.iter().filter(move |c| c.slot == slot)
    }

    /// Total frames of the cards in one slot.
    pub fn card_frames(&self, slot: CardSlot) -> i64 {
        self.cards_in(slot).map(|c| c.frames).sum()
    }

    pub fn source(&self, id: &SourceId) -> Option<&SourceInfo> {
        self.sources.iter().find(|s| &s.id == id)
    }

    /// Duration of a video clip in output frames (shared rounding rule).
    pub fn clip_frames(&self, clip: &VideoClip) -> i64 {
        self.fps
            .ticks_to_frame(Ticks(clip.source_out.0 - clip.source_in.0))
    }

    pub fn placements(&self, track: &VideoTrack) -> Vec<VideoPlacement> {
        let mut cursor = 0;
        track
            .clips
            .iter()
            .map(|c| {
                let frames = self.clip_frames(c);
                let p = VideoPlacement {
                    clip_id: c.id.clone(),
                    start_frame: cursor,
                    frames,
                };
                cursor += frames;
                p
            })
            .collect()
    }

    /// Start frame of every video clip across all video tracks.
    pub fn video_starts(&self) -> BTreeMap<String, i64> {
        self.video_tracks
            .iter()
            .flat_map(|t| self.placements(t))
            .map(|p| (p.clip_id, p.start_frame))
            .collect()
    }

    pub fn video_frames(&self) -> i64 {
        self.video_tracks
            .iter()
            .map(|t| {
                self.placements(t)
                    .last()
                    .map_or(0, |p| p.start_frame + p.frames)
            })
            .max()
            .unwrap_or(0)
    }

    /// Total output length in frames: video, extended by any audio that runs past it
    /// (the last video frame then holds).
    pub fn total_frames(&self) -> i64 {
        let audio = self
            .audio_tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| self.fps.ticks_to_frame(c.end()))
            .max()
            .unwrap_or(0);
        self.video_frames().max(audio)
    }

    pub fn find_video_clip(&self, id: &str) -> Option<(usize, usize)> {
        for (ti, t) in self.video_tracks.iter().enumerate() {
            if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
                return Some((ti, ci));
            }
        }
        None
    }

    /// Position (track, clip) of a title.
    pub fn find_title(&self, id: &str) -> Option<(usize, usize)> {
        for (ti, t) in self.overlay_tracks.iter().enumerate() {
            if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
                return Some((ti, ci));
            }
        }
        None
    }

    pub fn find_audio_clip(&self, id: &str) -> Option<(usize, usize)> {
        for (ti, t) in self.audio_tracks.iter().enumerate() {
            if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
                return Some((ti, ci));
            }
        }
        None
    }

    pub fn mic_attachment(&self, camera: &SourceId) -> Option<&MicAttachment> {
        self.mic_attachments.iter().find(|a| &a.camera == camera)
    }

    /// Checks every structural invariant. Run after every applied action and on load.
    pub fn validate(&self) -> Result<(), TimelineError> {
        let mut ids = BTreeSet::new();
        let mut claim = |id: &str| -> Result<(), TimelineError> {
            if !ids.insert(id.to_string()) {
                return Err(TimelineError::DuplicateId(id.to_string()));
            }
            Ok(())
        };
        for t in &self.video_tracks {
            claim(&t.id)?;
        }
        for t in &self.audio_tracks {
            claim(&t.id)?;
        }
        for t in &self.overlay_tracks {
            claim(&t.id)?;
        }
        for w in self.sources.windows(2) {
            if w[0].id >= w[1].id {
                return Err(TimelineError::DuplicateId(w[1].id.to_string()));
            }
        }
        let range_err = |id: &str, why: &str| TimelineError::InvalidRange(id.into(), why.into());
        for t in &self.video_tracks {
            for c in &t.clips {
                claim(&c.id)?;
                let s = self
                    .source(&c.source)
                    .ok_or_else(|| TimelineError::UnknownSource(c.source.to_string()))?;
                if s.video.is_none() {
                    return Err(TimelineError::MissingStream(c.source.to_string(), "video"));
                }
                if c.source_in.0 < 0 || c.source_out <= c.source_in || c.source_out > s.duration {
                    return Err(range_err(&c.id, "source range outside the source"));
                }
                if self.clip_frames(c) < 1 {
                    return Err(range_err(&c.id, "shorter than one frame"));
                }
            }
        }
        for (clip_id, visuals) in &self.corrections.visuals {
            visuals.validate(clip_id)?;
        }
        for card in &self.cards {
            claim(&card.id)?;
            card.validate()?;
        }
        for (card_id, fill) in &self.slot_fills {
            if !self.cards.iter().any(|c| &c.id == card_id) {
                return Err(range_err(card_id, "slot fill names no card"));
            }
            fill.rect.validate(card_id)?;
            let s = self
                .source(&fill.source)
                .ok_or_else(|| TimelineError::UnknownSource(fill.source.to_string()))?;
            if s.video.is_none() {
                return Err(TimelineError::MissingStream(
                    fill.source.to_string(),
                    "video",
                ));
            }
            let duration = s
                .video
                .as_ref()
                .and_then(|v| v.duration)
                .unwrap_or(s.duration);
            if fill.source_in.0 < 0 || fill.source_in >= duration {
                return Err(range_err(card_id, "slot fill starts outside the source"));
            }
        }
        for slot in [CardSlot::Opening, CardSlot::Ending] {
            // One audio mode per slot: the render places a single bed rule per edge.
            let mut modes = self.cards_in(slot).map(|c| c.audio);
            if let Some(first) = modes.next() {
                if modes.any(|m| m != first) {
                    return Err(range_err(
                        "cards",
                        "cards in one slot must share one audio mode",
                    ));
                }
            }
        }
        for t in &self.audio_tracks {
            for c in &t.clips {
                claim(&c.id)?;
                let s = self
                    .source(&c.source)
                    .ok_or_else(|| TimelineError::UnknownSource(c.source.to_string()))?;
                if !s.has_audio {
                    return Err(TimelineError::MissingStream(c.source.to_string(), "audio"));
                }
                if c.trim_in.0 < 0 || c.trim_out <= c.trim_in || c.trim_out > s.duration {
                    return Err(range_err(&c.id, "trim range outside the source"));
                }
                if c.start.0 < 0 {
                    return Err(TimelineError::NegativeStart(c.id.clone()));
                }
                if !self.fps.is_aligned(c.start) {
                    return Err(range_err(&c.id, "start is not frame-aligned"));
                }
                if let UnderAudio::Duck { amount } = c.under {
                    if amount.0 < 0 {
                        return Err(range_err(&c.id, "duck amount must not be negative"));
                    }
                }
                if let Some(link) = &c.link {
                    if t.role != AudioRole::Narration {
                        return Err(TimelineError::NotNarration(c.id.clone()));
                    }
                    if self.find_video_clip(link).is_none() {
                        return Err(TimelineError::UnknownClip(link.clone()));
                    }
                }
            }
        }
        for t in &self.overlay_tracks {
            for c in &t.clips {
                claim(&c.id)?;
                if c.start_frame < 0 || c.frames < 1 {
                    return Err(range_err(&c.id, "overlay span"));
                }
                if c.fade_in_frames < 0
                    || c.fade_out_frames < 0
                    || c.fade_in_frames + c.fade_out_frames > c.frames
                {
                    return Err(range_err(&c.id, "fades longer than the title"));
                }
                c.template.validate(&c.id)?;
            }
        }
        for (id, c) in &self.corrections.captions {
            if !is_caption_key(id) {
                return Err(TimelineError::InvalidSourceId(id.clone()));
            }
            TitleTemplate::Caption {
                text: c.text.clone(),
            }
            .validate(id)?;
            c.position.validate()?;
            if c.start_frame < 0
                || c.frames < 1
                || c.start_frame
                    .checked_add(c.frames)
                    .is_none_or(|e| e > self.total_frames())
            {
                return Err(range_err(id, "caption span outside timeline"));
            }
        }
        let mut mic_cameras = BTreeSet::new();
        for a in &self.mic_attachments {
            if !mic_cameras.insert(a.camera.clone()) {
                return Err(TimelineError::DuplicateId(format!(
                    "mic camera {}",
                    a.camera
                )));
            }
            let camera = self
                .source(&a.camera)
                .ok_or_else(|| TimelineError::UnknownSource(a.camera.to_string()))?;
            if camera.video.is_none() || !camera.has_audio {
                return Err(TimelineError::MissingStream(
                    a.camera.to_string(),
                    "camera audio",
                ));
            }
            let microphone = self
                .source(&a.microphone)
                .ok_or_else(|| TimelineError::UnknownSource(a.microphone.to_string()))?;
            if !microphone.has_audio {
                return Err(TimelineError::MissingStream(
                    a.microphone.to_string(),
                    "audio",
                ));
            }
            if a.camera == a.microphone {
                return Err(TimelineError::InvalidRange(
                    a.camera.to_string(),
                    "camera and microphone must differ".into(),
                ));
            }
            if a.sample_rate == 0 || a.confidence_milli > 1000 {
                return Err(TimelineError::InvalidRange(
                    a.camera.to_string(),
                    "invalid microphone alignment metadata".into(),
                ));
            }
        }
        for position in self.corrections.positions.values() {
            position.validate()?;
        }
        Ok(())
    }
}
