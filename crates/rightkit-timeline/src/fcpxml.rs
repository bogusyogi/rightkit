//! FCPXML hand-off for DaVinci Resolve (media plan 5.4, v1 subset).
//!
//! # Pinned version: FCPXML 1.10
//!
//! 1.10 (Final Cut Pro 10.6) is the newest version that every DaVinci Resolve
//! release with FCPXML import reads reliably: Resolve 17, 18 and 19 all import
//! 1.8 to 1.10, while 1.11 and later add FCP-only features and are not
//! guaranteed in 17/18. 1.10 also has the `media-rep` asset form current
//! Resolve builds write themselves. Nothing in the v1 subset needs a newer version.
//!
//! # What is exported
//!
//! - Video clips of the first populated video track, back to back on the primary
//!   storyline (`spine`), offsets and durations on the project frame grid as
//!   exact rationals (`1001/30000s`), using the timeline's shared frame-rounding rule.
//! - Narration and music clips as connected audio lanes (`lane="-1"`, `-2`, ...
//!   one per audio track) attached to the spine item they start under, with gain
//!   as `adjust-volume`.
//! - Media referenced by absolute `file://` URLs of the project's owned files, or
//!   copied next to the XML ([`ExportOptions::collect_media`]).
//! - Caption titles (and any extra cues) as an `.srt` beside the XML.
//!
//! # Reported, not exported ([`Unsupported`])
//!
//! Non-caption titles (baked title clips are out for now), narration ducking or
//! muting of the audio underneath, extra video tracks, and the last-frame hold
//! after the video (a `gap` keeps the sequence length instead).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    AudioRole, Fps, SourceId, Ticks, Timeline, TitleTemplate, UnderAudio, TICKS_PER_SECOND,
};
use thiserror::Error;

/// The one FCPXML version this crate writes.
pub const FCPXML_VERSION: &str = "1.10";

#[derive(Debug, Error)]
pub enum FcpxmlError {
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
}

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> FcpxmlError {
    let context = context.into();
    move |source| FcpxmlError::Io { context, source }
}

/// One caption cue for the SRT (times on the output timeline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionCue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// File stem of `<name>.fcpxml` / `<name>.srt`; also the project name.
    pub name: String,
    /// Copy each used source into `<out_dir>/Media/` and reference the copy.
    pub collect_media: bool,
    /// Sequence frame size. `None` uses the first video source's size, else 1920x1080.
    pub frame_size: Option<(u32, u32)>,
    /// Extra cues written to the SRT after the timeline's own caption titles.
    pub extra_cues: Vec<CaptionCue>,
}

impl ExportOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            collect_media: false,
            frame_size: None,
            extra_cues: Vec::new(),
        }
    }
}

/// An item present in the timeline that the export does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// `title`, `ducking`, `video_track`, `hold`.
    pub kind: &'static str,
    /// Clip, title or track id.
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRef {
    pub source: SourceId,
    /// File the XML points at (the collected copy when collecting).
    pub path: PathBuf,
    pub url: String,
    pub collected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub fcpxml: PathBuf,
    pub srt: Option<PathBuf>,
    pub version: &'static str,
    pub media: Vec<MediaRef>,
    pub video_clips: usize,
    pub audio_clips: usize,
    pub caption_cues: usize,
    pub total_frames: i64,
    /// Sequence duration as written (`3/1s`).
    pub duration: String,
    pub unsupported: Vec<Unsupported>,
}

fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `num/den` seconds as an FCPXML time (`0s`, `2s`, `1001/30000s`).
fn rational(num: i128, den: i128) -> String {
    if num == 0 {
        return "0s".into();
    }
    let g = gcd(num, den);
    let (n, d) = (num / g, den / g);
    if d == 1 {
        format!("{n}s")
    } else {
        format!("{n}/{d}s")
    }
}

fn frames_time(fps: Fps, frames: i64) -> String {
    rational(frames as i128 * fps.den as i128, fps.num as i128)
}

fn ticks_time(t: Ticks) -> String {
    rational(t.0 as i128, TICKS_PER_SECOND as i128)
}

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => o.push(c),
        }
    }
    o
}

/// `file://` URL for an absolute path, percent-encoding everything but unreserved
/// characters and `/`.
pub fn file_url(path: &Path) -> Result<String, FcpxmlError> {
    if !path.is_absolute() {
        return Err(FcpxmlError::BadInput(format!(
            "media path is not absolute: {}",
            path.display()
        )));
    }
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/'); // Windows drive path: file:///C:/...
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

fn sanitize_name(name: &str) -> Result<String, FcpxmlError> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(name.to_string())
    } else {
        Err(FcpxmlError::BadInput(format!(
            "invalid export name {name:?}: use letters, digits, '.', '_' or '-'"
        )))
    }
}

fn srt_time(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// SRT text for cues (blank text is skipped; numbering restarts at 1).
pub fn srt_text(cues: &[CaptionCue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        let _ = write!(
            out,
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_time(c.start_ms),
            srt_time(c.end_ms),
            c.text.trim()
        );
    }
    out
}

/// A spine item that connected audio can attach to.
struct SpineItem {
    start_frame: i64,
    frames: i64,
    /// Source time (ticks) at the item's left edge; zero for a gap.
    source_start: Ticks,
}

/// Exports `tl` as `<out_dir>/<name>.fcpxml` (plus SRT and collected media if asked).
///
/// `sources` maps every source the timeline uses to its absolute file (the project's
/// owned media).
pub fn export_fcpxml(
    tl: &Timeline,
    sources: &BTreeMap<SourceId, PathBuf>,
    out_dir: &Path,
    opts: &ExportOptions,
) -> Result<Report, FcpxmlError> {
    tl.validate()?;
    let name = sanitize_name(&opts.name)?;
    let fps = tl.fps;
    let total_frames = tl.total_frames();
    if total_frames < 1 {
        return Err(crate::TimelineError::EmptyTimeline.into());
    }
    let mut unsupported = Vec::new();

    // Used sources, in a stable order.
    let mut used: Vec<SourceId> = Vec::new();
    let mut want = |id: &SourceId| {
        if !used.contains(id) {
            used.push(id.clone());
        }
    };
    let populated: Vec<_> = tl
        .video_tracks
        .iter()
        .filter(|t| !t.clips.is_empty())
        .collect();
    if let Some(track) = populated.first() {
        for c in &track.clips {
            want(&c.source);
        }
    }
    for t in &tl.audio_tracks {
        for c in &t.clips {
            want(&c.source);
        }
    }
    for t in populated.iter().skip(1) {
        for c in &t.clips {
            unsupported.push(Unsupported {
                kind: "video_track",
                id: c.id.clone(),
                reason: format!(
                    "clip on extra video track {}: only one video track is exported",
                    t.id
                ),
            });
        }
    }

    // Media: resolve, optionally collect.
    fs::create_dir_all(out_dir).map_err(io(format!("creating {}", out_dir.display())))?;
    let mut media = Vec::new();
    let mut asset_of: BTreeMap<SourceId, String> = BTreeMap::new();
    for (i, id) in used.iter().enumerate() {
        let file = sources
            .get(id)
            .ok_or_else(|| FcpxmlError::BadInput(format!("no media file for source {id}")))?;
        if !file.is_absolute() {
            return Err(FcpxmlError::BadInput(format!(
                "media path is not absolute: {}",
                file.display()
            )));
        }
        let (path, collected) = if opts.collect_media {
            let dir = out_dir.join("Media");
            fs::create_dir_all(&dir).map_err(io(format!("creating {}", dir.display())))?;
            let leaf = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "media".into());
            let dest = dir.join(format!("{}-{}", &id.as_str()[..8], leaf));
            // An existing copy with the source's bytes is reused; one with other bytes is refused, never
            // replaced. A new copy is atomic, verified and exclusive (temp + no-replace rename, source and
            // copy digests compared): an interrupted export never leaves a truncated media file.
            if dest.symlink_metadata().is_ok() {
                if file_digest(&dest)? != file_digest(file)? {
                    return Err(FcpxmlError::BadInput(format!(
                        "{} already exists with different bytes than {}; refusing to replace it",
                        dest.display(),
                        file.display()
                    )));
                }
            } else {
                let options = rightkit_fs::AtomicWriteOptions {
                    replace: rightkit_fs::ReplaceMode::NoReplace,
                    ..Default::default()
                };
                rightkit_fs::copy_verified(file, &dest, options, None).map_err(|e| {
                    io(format!("copying {} to {}", file.display(), dest.display()))(
                        std::io::Error::other(e),
                    )
                })?;
            }
            (dest, true)
        } else {
            (file.clone(), false)
        };
        asset_of.insert(id.clone(), format!("r{}", i + 2));
        media.push(MediaRef {
            source: id.clone(),
            url: file_url(&path)?,
            path,
            collected,
        });
    }

    // Sequence format.
    let (fw, fh) = opts.frame_size.unwrap_or_else(|| {
        tl.sources
            .iter()
            .find_map(|s| s.video.as_ref().map(|v| (v.width, v.height)))
            .unwrap_or((1920, 1080))
    });
    let frame_duration = frames_time(fps, 1);

    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n\n");
    let _ = writeln!(x, "<fcpxml version=\"{FCPXML_VERSION}\">");
    x.push_str("  <resources>\n");
    let _ = writeln!(
        x,
        "    <format id=\"r1\" name=\"CutRight{fw}x{fh}\" frameDuration=\"{frame_duration}\" width=\"{fw}\" height=\"{fh}\" colorSpace=\"1-1-1 (Rec. 709)\"/>"
    );
    for m in &media {
        let info = tl.source(&m.source).expect("validated source");
        let id = &asset_of[&m.source];
        let leaf = m
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Video asset durations are whole frames of the project rate (rounded down so the
        // asset never claims more than the file has); audio keeps exact time.
        let duration = if info.video.is_some() {
            let f = fps.ticks_to_frame(info.duration);
            let f = if fps.frame_to_ticks(f) > info.duration {
                f - 1
            } else {
                f
            };
            frames_time(fps, f.max(1))
        } else {
            ticks_time(info.duration)
        };
        let mut attrs = format!(
            "id=\"{id}\" name=\"{}\" start=\"0s\" duration=\"{duration}\"",
            xml_escape(&leaf)
        );
        if info.video.is_some() {
            attrs.push_str(" hasVideo=\"1\" format=\"r1\" videoSources=\"1\"");
        }
        if info.has_audio {
            attrs.push_str(
                " hasAudio=\"1\" audioSources=\"1\" audioChannels=\"2\" audioRate=\"48000\"",
            );
        }
        let _ = writeln!(x, "    <asset {attrs}>");
        let _ = writeln!(
            x,
            "      <media-rep kind=\"original-media\" src=\"{}\"/>",
            xml_escape(&m.url)
        );
        x.push_str("    </asset>\n");
    }
    x.push_str("  </resources>\n  <library>\n");
    let _ = writeln!(x, "    <event name=\"{}\">", xml_escape(&name));
    let _ = writeln!(x, "      <project name=\"{}\">", xml_escape(&name));
    let _ = writeln!(
        x,
        "        <sequence format=\"r1\" duration=\"{}\" tcStart=\"0s\" tcFormat=\"NDF\" audioLayout=\"stereo\" audioRate=\"48k\">",
        frames_time(fps, total_frames)
    );
    x.push_str("          <spine>\n");

    // Spine items (video clips, then a gap if audio outruns the video) and the
    // connected audio attached to whichever item each audio clip starts under.
    struct Item {
        spec: SpineItem,
        open: String,
        close: &'static str,
        audio: Vec<String>,
    }
    let mut items: Vec<Item> = Vec::new();
    let mut video_clips = 0;
    if let Some(track) = populated.first() {
        for (clip, place) in track.clips.iter().zip(tl.placements(track)) {
            let src_start = fps.snap(clip.source_in);
            let id = &asset_of[&clip.source];
            let info = tl.source(&clip.source).expect("validated source");
            let audio_role = if info.has_audio {
                " audioRole=\"dialogue\""
            } else {
                ""
            };
            items.push(Item {
                spec: SpineItem {
                    start_frame: place.start_frame,
                    frames: place.frames,
                    source_start: src_start,
                },
                open: format!(
                    "<asset-clip ref=\"{id}\" name=\"{}\" offset=\"{}\" start=\"{}\" duration=\"{}\" format=\"r1\"{audio_role}",
                    xml_escape(&clip.id),
                    frames_time(fps, place.start_frame),
                    ticks_time(src_start),
                    frames_time(fps, place.frames),
                ),
                close: "</asset-clip>",
                audio: Vec::new(),
            });
            video_clips += 1;
        }
    }
    let video_end = items
        .last()
        .map_or(0, |i| i.spec.start_frame + i.spec.frames);
    if video_end < total_frames {
        let frames = total_frames - video_end;
        items.push(Item {
            spec: SpineItem {
                start_frame: video_end,
                frames,
                source_start: Ticks::ZERO,
            },
            open: format!(
                "<gap name=\"Gap\" offset=\"{}\" start=\"0s\" duration=\"{}\"",
                frames_time(fps, video_end),
                frames_time(fps, frames)
            ),
            close: "</gap>",
            audio: Vec::new(),
        });
        if video_end > 0 {
            unsupported.push(Unsupported {
                kind: "hold",
                id: "tail".into(),
                reason: format!(
                    "the last video frame holds for {frames} frames in render; the export has a gap there"
                ),
            });
        }
    }

    let mut audio_clips = 0;
    for (lane_idx, track) in tl.audio_tracks.iter().enumerate() {
        let lane = -(lane_idx as i32 + 1);
        for clip in &track.clips {
            let start_frame = fps.ticks_to_frame(clip.start);
            let host = items
                .iter()
                .rposition(|i| i.spec.start_frame <= start_frame)
                .unwrap_or(0);
            let it = &mut items[host];
            let into_host = fps.frame_to_ticks(start_frame - it.spec.start_frame);
            let offset = Ticks(it.spec.source_start.0 + into_host.0);
            let (role, gain) = (
                match track.role {
                    AudioRole::Narration => "dialogue",
                    AudioRole::Music => "music",
                },
                clip.gain.0,
            );
            let asset = &asset_of[&clip.source];
            let mut s = format!(
                "<asset-clip ref=\"{asset}\" lane=\"{lane}\" name=\"{}\" offset=\"{}\" start=\"{}\" duration=\"{}\" audioRole=\"{role}\"",
                xml_escape(&clip.id),
                ticks_time(offset),
                ticks_time(clip.trim_in),
                ticks_time(clip.duration()),
            );
            if gain != 0 {
                let _ = write!(
                    s,
                    ">\n                <adjust-volume amount=\"{}\"/>\n              </asset-clip>",
                    clip.gain.ffmpeg_db()
                );
            } else {
                s.push_str("/>");
            }
            it.audio.push(s);
            audio_clips += 1;
            if track.role == AudioRole::Narration && clip.under != UnderAudio::Keep {
                unsupported.push(Unsupported {
                    kind: "ducking",
                    id: clip.id.clone(),
                    reason: "audio under this narration is ducked/muted in render; adjust it in the editor".into(),
                });
            }
        }
    }
    for item in &items {
        let _ = write!(x, "            {}", item.open);
        if item.audio.is_empty() {
            x.push_str("/>\n");
        } else {
            x.push_str(">\n");
            for a in &item.audio {
                let _ = writeln!(x, "              {a}");
            }
            let _ = writeln!(x, "            {}", item.close);
        }
    }
    x.push_str("          </spine>\n        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n");

    // Captions: Caption titles become cues; other titles are reported.
    let mut cues = Vec::new();
    for track in &tl.overlay_tracks {
        for t in &track.clips {
            match &t.template {
                TitleTemplate::Caption { text } => cues.push(CaptionCue {
                    start_ms: fps.frame_to_ticks(t.start_frame).to_units(1000),
                    end_ms: fps.frame_to_ticks(t.start_frame + t.frames).to_units(1000),
                    text: text.clone(),
                }),
                other => unsupported.push(Unsupported {
                    kind: "title",
                    id: t.id.clone(),
                    reason: format!(
                        "{} title is not exported (baked title clips are not part of the v1 subset)",
                        other.kind()
                    ),
                }),
            }
        }
    }
    cues.extend(opts.extra_cues.iter().cloned());
    cues.sort_by_key(|c| (c.start_ms, c.end_ms));

    let fcpxml_path = out_dir.join(format!("{name}.fcpxml"));
    write_atomic(&fcpxml_path, x.as_bytes())?;
    let srt = if cues.is_empty() {
        None
    } else {
        let p = out_dir.join(format!("{name}.srt"));
        write_atomic(&p, srt_text(&cues).as_bytes())?;
        Some(p)
    };
    Ok(Report {
        fcpxml: fcpxml_path,
        srt,
        version: FCPXML_VERSION,
        media,
        video_clips,
        audio_clips,
        caption_cues: cues.len(),
        total_frames,
        duration: frames_time(fps, total_frames),
        unsupported,
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), FcpxmlError> {
    rightkit_fs::atomic_write(path, bytes).map_err(|e| FcpxmlError::Io {
        context: format!("writing {}", path.display()),
        source: std::io::Error::other(e),
    })
}

fn file_digest(path: &Path) -> Result<String, FcpxmlError> {
    rightkit_fs::digest::blake3_file(path).map_err(io(format!("reading {}", path.display())))
}
