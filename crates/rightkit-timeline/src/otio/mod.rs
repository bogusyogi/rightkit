//! OpenTimelineIO export of a CutRight timeline.
//!
//! Port of `video-project/src/export.rs::export_otio` onto the revision-backed
//! `cutright-timeline` model. Serialises to OTIO JSON using the core schemas
//! `Timeline.1`, `Stack.1`, `Track.1`, `Clip.2`, `Gap.1`, `Marker.2`,
//! `ExternalReference.1`, `RationalTime.1` and `TimeRange.1`.
//!
//! # Layout
//!
//! - One `Video` track per populated video track. Clips are back to back
//!   (the model's video tracks are gapless); `source_range` is the source in/out.
//! - One `Audio` track `<id> (linked audio)` per populated video track carrying
//!   the video clips' own audio (the clips whose source has audio, `Gap.1` for
//!   the others), each clip tagged `cutright.linked_video_clip`.
//! - One `Audio` track per timeline audio track: clips placed by `Gap.1`
//!   fillers at their output start; narration keeps its `link` in metadata.
//! - Titles (and extra caption cues) become `Marker.2` items on the first video
//!   track with the template, text and fades in `metadata.cutright`.
//!
//! All times are `RationalTime.1` values in frames at the timeline fps
//! (`rate = num/den`), computed with the timeline's shared frame-rounding rule.
//! Every clip's media is an `ExternalReference.1` with an absolute `file://` URL.
//!
//! Not carried: ducking of the audio under a narration clip, title rendering
//! (markers only), and video track overlaps (none exist in the model).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{AudioRole, Fps, SourceId, Ticks, Timeline, TitleTemplate, UnderAudio};
use serde_json::{json, Map, Value};
use thiserror::Error;

pub mod validate;
pub use validate::{validate_otio, ShapeError};

#[derive(Debug, Error)]
pub enum OtioError {
    #[error("{0}")]
    BadInput(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Timeline(#[from] crate::TimelineError),
    #[error("serialising OTIO: {0}")]
    Json(#[from] serde_json::Error),
}

/// One extra caption cue (output-time milliseconds), exported as a marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionCue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// File stem of `<name>.otio`; also the timeline name.
    pub name: String,
    /// Extra caption cues exported as markers beside the timeline's titles.
    pub extra_cues: Vec<CaptionCue>,
}

impl ExportOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            extra_cues: Vec::new(),
        }
    }
}

/// Something present in the timeline that the export does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// `ducking`.
    pub kind: &'static str,
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub otio: Option<PathBuf>,
    pub video_clips: usize,
    pub audio_clips: usize,
    pub linked_audio_clips: usize,
    pub markers: usize,
    pub total_frames: i64,
    pub unsupported: Vec<Unsupported>,
}

fn rate(fps: Fps) -> f64 {
    fps.num as f64 / fps.den as f64
}

fn rt(frames: i64, fps: Fps) -> Value {
    json!({"OTIO_SCHEMA": "RationalTime.1", "rate": rate(fps), "value": frames as f64})
}

fn range(start: i64, duration: i64, fps: Fps) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "duration": rt(duration, fps),
        "start_time": rt(start, fps),
    })
}

/// `file://` URL for an absolute path, percent-encoding everything but
/// unreserved characters, `/` and `:`.
pub fn file_url(path: &Path) -> Result<String, OtioError> {
    if !path.is_absolute() {
        return Err(OtioError::BadInput(format!(
            "media path is not absolute: {}",
            path.display()
        )));
    }
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            url.push(b as char);
        } else {
            let _ = write!(url, "%{b:02X}");
        }
    }
    Ok(url)
}

fn sanitize_name(name: &str) -> Result<String, OtioError> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(name.to_string())
    } else {
        Err(OtioError::BadInput(format!(
            "invalid export name {name:?}: use letters, digits, '.', '_' or '-'"
        )))
    }
}

fn media_reference(
    tl: &Timeline,
    id: &SourceId,
    sources: &BTreeMap<SourceId, PathBuf>,
) -> Result<Value, OtioError> {
    let file = sources
        .get(id)
        .ok_or_else(|| OtioError::BadInput(format!("no media file for source {id}")))?;
    let info = tl.source(id).expect("validated source");
    let fps = tl.fps;
    let frames = fps.ticks_to_frame(info.duration);
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(json!({
        "OTIO_SCHEMA": "ExternalReference.1",
        "name": name,
        "available_range": range(0, frames, fps),
        "available_image_bounds": Value::Null,
        "target_url": file_url(file)?,
        "metadata": {"cutright": {"source": id.as_str(), "has_video": info.video.is_some(), "has_audio": info.has_audio}},
    }))
}

#[allow(clippy::too_many_arguments)]
fn clip(
    tl: &Timeline,
    name: &str,
    source: &SourceId,
    start: i64,
    frames: i64,
    sources: &BTreeMap<SourceId, PathBuf>,
    meta: Value,
) -> Result<Value, OtioError> {
    Ok(json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": name,
        "source_range": range(start, frames, tl.fps),
        "effects": [],
        "markers": [],
        "enabled": true,
        "color": Value::Null,
        "metadata": {"cutright": meta},
        "media_references": {"DEFAULT_MEDIA": media_reference(tl, source, sources)?},
        "active_media_reference_key": "DEFAULT_MEDIA",
    }))
}

fn gap(frames: i64, fps: Fps) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "name": "",
        "source_range": range(0, frames, fps),
        "effects": [],
        "markers": [],
        "enabled": true,
        "color": Value::Null,
        "metadata": {},
    })
}

fn track(name: &str, kind: &str, children: Vec<Value>, markers: Vec<Value>, meta: Value) -> Value {
    json!({
        "OTIO_SCHEMA": "Track.1",
        "name": name,
        "kind": kind,
        "source_range": Value::Null,
        "effects": [],
        "markers": markers,
        "enabled": true,
        "color": Value::Null,
        "metadata": meta,
        "children": children,
    })
}

fn marker(name: &str, color: &str, start: i64, frames: i64, fps: Fps, meta: Value) -> Value {
    json!({
        "OTIO_SCHEMA": "Marker.2",
        "name": name,
        "color": color,
        "comment": "",
        "marked_range": range(start, frames, fps),
        "metadata": {"cutright": meta},
    })
}

fn ms_to_frame(ms: i64, fps: Fps) -> i64 {
    fps.ticks_to_frame(Ticks::from_millis(ms))
}

/// Builds the OTIO document (nothing is written).
pub fn to_otio(
    tl: &Timeline,
    sources: &BTreeMap<SourceId, PathBuf>,
    opts: &ExportOptions,
) -> Result<(Value, Report), OtioError> {
    tl.validate()?;
    let name = sanitize_name(&opts.name)?;
    let fps = tl.fps;
    let total_frames = tl.total_frames();
    if total_frames < 1 {
        return Err(crate::TimelineError::EmptyTimeline.into());
    }
    let mut report = Report {
        otio: None,
        video_clips: 0,
        audio_clips: 0,
        linked_audio_clips: 0,
        markers: 0,
        total_frames,
        unsupported: Vec::new(),
    };
    let mut tracks: Vec<Value> = Vec::new();
    let mut audio_tracks: Vec<Value> = Vec::new();

    // Markers (titles, extra cues), attached to the first video track.
    let mut markers: Vec<Value> = Vec::new();
    for t in &tl.overlay_tracks {
        for c in &t.clips {
            let (kind, color, text) = match &c.template {
                TitleTemplate::Caption { text } => ("caption", "CYAN", text.clone()),
                TitleTemplate::Centered { text } => ("title", "YELLOW", text.clone()),
                TitleTemplate::LowerThird { title, subtitle } => (
                    "title",
                    "YELLOW",
                    match subtitle {
                        Some(s) => format!("{title}\n{s}"),
                        None => title.clone(),
                    },
                ),
            };
            markers.push(marker(
                &c.id,
                color,
                c.start_frame,
                c.frames,
                fps,
                json!({
                    "kind": kind,
                    "template": c.template.kind(),
                    "text": text,
                    "overlay_track": t.id,
                    "fade_in_frames": c.fade_in_frames,
                    "fade_out_frames": c.fade_out_frames,
                }),
            ));
        }
    }
    for (i, cue) in opts.extra_cues.iter().enumerate() {
        let start = ms_to_frame(cue.start_ms, fps);
        let end = ms_to_frame(cue.end_ms, fps).max(start + 1);
        markers.push(marker(
            &format!("cue-{:04}", i + 1),
            "CYAN",
            start,
            end - start,
            fps,
            json!({"kind": "caption", "text": cue.text.trim()}),
        ));
    }
    markers.sort_by(|a, b| {
        let key = |v: &Value| {
            v["marked_range"]["start_time"]["value"]
                .as_f64()
                .unwrap_or(0.0)
        };
        key(a)
            .partial_cmp(&key(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    report.markers = markers.len();
    let mut markers = Some(markers);

    for vt in tl.video_tracks.iter().filter(|t| !t.clips.is_empty()) {
        let mut video = Vec::new();
        let mut linked = Vec::new();
        let mut any_audio = false;
        for (c, place) in vt.clips.iter().zip(tl.placements(vt)) {
            let src_start = fps.ticks_to_frame(c.source_in);
            let meta = json!({"clip": c.id, "source": c.source.as_str(), "output_start_frame": place.start_frame});
            video.push(clip(
                tl,
                &c.id,
                &c.source,
                src_start,
                place.frames,
                sources,
                meta,
            )?);
            report.video_clips += 1;
            let has_audio = tl.source(&c.source).is_some_and(|s| s.has_audio);
            if has_audio {
                any_audio = true;
                let meta = json!({"linked_video_clip": c.id, "role": "source_audio"});
                linked.push(clip(
                    tl,
                    &format!("{} audio", c.id),
                    &c.source,
                    src_start,
                    place.frames,
                    sources,
                    meta,
                )?);
                report.linked_audio_clips += 1;
            } else {
                linked.push(gap(place.frames, fps));
            }
        }
        tracks.push(track(
            &vt.id,
            "Video",
            video,
            markers.take().unwrap_or_default(),
            json!({"cutright": {"track": vt.id}}),
        ));
        if any_audio {
            audio_tracks.push(track(
                &format!("{} (linked audio)", vt.id),
                "Audio",
                linked,
                Vec::new(),
                json!({"cutright": {"track": vt.id, "linked_to": vt.id}}),
            ));
        }
    }
    // Titles with no video track at all still need a home.
    if let Some(m) = markers.take().filter(|m| !m.is_empty()) {
        tracks.push(track(
            "V1",
            "Video",
            vec![gap(total_frames, fps)],
            m,
            json!({}),
        ));
    }

    for at in tl.audio_tracks.iter().filter(|t| !t.clips.is_empty()) {
        let mut ordered: Vec<_> = at.clips.iter().collect();
        ordered.sort_by_key(|c| (c.start, c.id.clone()));
        let mut children = Vec::new();
        let mut cursor = 0i64;
        for c in ordered {
            let start = fps.ticks_to_frame(c.start);
            let frames = fps.ticks_to_frame(c.duration()).max(1);
            if start > cursor {
                children.push(gap(start - cursor, fps));
            } else if start < cursor {
                return Err(OtioError::BadInput(format!(
                    "audio clip {} overlaps the previous clip on track {}: overlapping audio cannot be written as one OTIO track",
                    c.id, at.id
                )));
            }
            let under = match c.under {
                UnderAudio::Keep => json!({"mode": "keep"}),
                UnderAudio::Duck { amount } => {
                    report.unsupported.push(Unsupported {
                        kind: "ducking",
                        id: c.id.clone(),
                        reason: "ducking of the audio underneath is recorded in metadata only"
                            .into(),
                    });
                    json!({"mode": "duck", "amount_millidb": amount.0})
                }
                UnderAudio::Mute => {
                    report.unsupported.push(Unsupported {
                        kind: "ducking",
                        id: c.id.clone(),
                        reason: "muting of the audio underneath is recorded in metadata only"
                            .into(),
                    });
                    json!({"mode": "mute"})
                }
            };
            let meta = json!({
                "clip": c.id,
                "source": c.source.as_str(),
                "gain_millidb": c.gain.0,
                "link": c.link,
                "under": under,
            });
            children.push(clip(
                tl,
                &c.id,
                &c.source,
                fps.ticks_to_frame(c.trim_in),
                frames,
                sources,
                meta,
            )?);
            report.audio_clips += 1;
            cursor = start + frames;
        }
        let role = match at.role {
            AudioRole::Narration => "narration",
            AudioRole::Music => "music",
        };
        audio_tracks.push(track(
            &at.id,
            "Audio",
            children,
            Vec::new(),
            json!({"cutright": {"track": at.id, "role": role}}),
        ));
    }
    tracks.extend(audio_tracks);

    let mut stack = Map::new();
    stack.insert("OTIO_SCHEMA".into(), json!("Stack.1"));
    stack.insert("name".into(), json!("tracks"));
    stack.insert("source_range".into(), Value::Null);
    stack.insert("effects".into(), json!([]));
    stack.insert("markers".into(), json!([]));
    stack.insert("enabled".into(), json!(true));
    stack.insert("color".into(), Value::Null);
    stack.insert("metadata".into(), json!({}));
    stack.insert("children".into(), Value::Array(tracks));

    let doc = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": name,
        "global_start_time": rt(0, fps),
        "metadata": {"cutright": {
            "fps": {"num": fps.num, "den": fps.den},
            "total_frames": total_frames,
            "producer": "cutright-otio",
        }},
        "tracks": Value::Object(stack),
    });
    Ok((doc, report))
}

/// Writes `<out_dir>/<name>.otio` (pretty JSON, trailing newline) and returns the report.
pub fn export_otio(
    tl: &Timeline,
    sources: &BTreeMap<SourceId, PathBuf>,
    out_dir: &Path,
    opts: &ExportOptions,
) -> Result<Report, OtioError> {
    let (doc, mut report) = to_otio(tl, sources, opts)?;
    validate_otio(&doc).map_err(|e| OtioError::BadInput(format!("internal: invalid OTIO: {e}")))?;
    fs::create_dir_all(out_dir).map_err(|source| OtioError::Io {
        context: format!("creating {}", out_dir.display()),
        source,
    })?;
    let path = out_dir.join(format!("{}.otio", sanitize_name(&opts.name)?));
    let mut text = serde_json::to_string_pretty(&doc)?;
    text.push('\n');
    rightkit_fs::atomic_write(&path, text.as_bytes()).map_err(|source| OtioError::Io {
        context: format!("writing {}", path.display()),
        source: source.into(),
    })?;
    report.otio = Some(path);
    Ok(report)
}
