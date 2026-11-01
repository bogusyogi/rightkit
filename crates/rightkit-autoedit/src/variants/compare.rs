//! Word-locked A/B compare: align two variants of one source by transcript
//! word index, report differences, give a synced player a seek map, and build
//! a labelled stacked or side-by-side review render.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cuts::MappedWord;
use rightkit_ffmpeg::FfmpegCommand;
use rightkit_timeline::title::escape_filter_path;
use rightkit_timeline::{Encoder, TextFile};
use rightkit_transcript::Transcript;
use rightkit_video_project::worker::{spawn_ffmpeg, wait_with_output, StdioMode, StdioPolicy};
use serde::{Deserialize, Serialize};

use crate::variants::error::VariantError;
use crate::variants::variant::{transcript_blake3, Variant};

pub const COMPARE_SCHEMA: &str = "cutright.variant-compare.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    A,
    B,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantSummary {
    pub name: String,
    pub preset: crate::cuts::Preset,
    pub custom: bool,
    pub output_ms: i64,
    pub output_frames: i64,
    pub clips: usize,
    pub cuts: usize,
    pub kept_words: usize,
    pub removed_words: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordStatus {
    Both,
    OnlyA,
    OnlyB,
    Neither,
}

/// One transcript word with its output span in each variant (`None` = cut).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordRow {
    pub index: usize,
    pub text: String,
    pub source_start_ms: u64,
    pub a: Option<(i64, i64)>,
    pub b: Option<(i64, i64)>,
    pub status: WordStatus,
    /// `b.start - a.start` when both keep the word.
    pub delta_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareReport {
    pub schema: String,
    pub source_blake3: String,
    pub transcript_blake3: String,
    pub a: VariantSummary,
    pub b: VariantSummary,
    /// `b.output_ms - a.output_ms`.
    pub duration_delta_ms: i64,
    pub cuts_delta: i64,
    pub shared_words: usize,
    pub only_a: Vec<usize>,
    pub only_b: Vec<usize>,
    pub cut_in_both: usize,
    pub max_abs_delta_ms: i64,
    pub words: Vec<WordRow>,
}

/// Seek targets for one word: exact output start where the variant keeps it,
/// else the next kept word's start (or the end) so a player lands after the cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeekEntry {
    pub index: usize,
    pub a_ms: Option<i64>,
    pub b_ms: Option<i64>,
    pub a_snap_ms: i64,
    pub b_snap_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeekMap {
    pub a_duration_ms: i64,
    pub b_duration_ms: i64,
    pub entries: Vec<SeekEntry>,
}

impl SeekMap {
    pub fn entry(&self, word: usize) -> Option<&SeekEntry> {
        self.entries.get(word)
    }

    /// Output time in `side` for `word` (exact if kept, else snapped forward).
    pub fn seek(&self, word: usize, side: Side) -> Option<i64> {
        let e = self.entry(word)?;
        Some(match side {
            Side::A => e.a_ms.unwrap_or(e.a_snap_ms),
            Side::B => e.b_ms.unwrap_or(e.b_snap_ms),
        })
    }

    /// The other side's time for a position in `from`: the word playing at
    /// `ms` (or the next kept word) is located, then looked up in the other
    /// variant. Used to keep two players in step while scrubbing.
    pub fn sync(&self, from: Side, ms: i64) -> Option<i64> {
        let pick = |e: &SeekEntry| match from {
            Side::A => e.a_ms,
            Side::B => e.b_ms,
        };
        // Last word of `from` that starts at or before ms.
        let e = self
            .entries
            .iter()
            .rfind(|e| pick(e).is_some_and(|t| t <= ms))
            .or_else(|| self.entries.iter().find(|e| pick(e).is_some()))?;
        let from_t = pick(e)?;
        let other = self.seek(
            e.index,
            match from {
                Side::A => Side::B,
                Side::B => Side::A,
            },
        )?;
        Some((other + (ms - from_t).max(0)).min(match from {
            Side::A => self.b_duration_ms,
            Side::B => self.a_duration_ms,
        }))
    }

    /// Nearest word at or after `index` that both variants keep.
    pub fn next_shared(&self, index: usize) -> Option<usize> {
        self.entries
            .iter()
            .skip(index)
            .find(|e| e.a_ms.is_some() && e.b_ms.is_some())
            .map(|e| e.index)
    }
}

fn summary(v: &Variant, spans: &BTreeMap<usize, MappedWord>, words: usize) -> VariantSummary {
    let map = v.time_map();
    VariantSummary {
        name: v.name.clone(),
        preset: v.provenance.preset,
        custom: v.provenance.custom,
        output_ms: map.output_duration().to_units(1000),
        output_frames: map.output_frames(),
        clips: v.plan.kept.len(),
        cuts: v.plan.removed.len(),
        kept_words: spans.len(),
        removed_words: words - spans.len(),
    }
}

/// Aligns `a` and `b` by transcript word index. Both must come from the same
/// source and the given transcript.
pub fn compare_variants(
    a: &Variant,
    b: &Variant,
    transcript: &Transcript,
) -> Result<(CompareReport, SeekMap), VariantError> {
    let hash = transcript_blake3(transcript);
    for v in [a, b] {
        if v.provenance.transcript_blake3 != hash {
            return Err(VariantError::Mismatch(format!(
                "variant {:?} was cut from a different transcript",
                v.name
            )));
        }
    }
    if a.provenance.source_blake3 != b.provenance.source_blake3 {
        return Err(VariantError::Mismatch(
            "variants come from different sources".into(),
        ));
    }
    let (ma, mb) = (a.time_map(), b.time_map());
    let aw: BTreeMap<usize, MappedWord> = ma
        .map_words(transcript)
        .into_iter()
        .map(|w| (w.index, w))
        .collect();
    let bw: BTreeMap<usize, MappedWord> = mb
        .map_words(transcript)
        .into_iter()
        .map(|w| (w.index, w))
        .collect();
    let n = transcript.words.len();
    let (a_end, b_end) = (
        ma.output_duration().to_units(1000),
        mb.output_duration().to_units(1000),
    );

    let mut words = Vec::with_capacity(n);
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    let (mut shared, mut neither, mut max_abs) = (0, 0, 0i64);
    for (i, w) in transcript.words.iter().enumerate() {
        let (x, y) = (aw.get(&i), bw.get(&i));
        let status = match (x, y) {
            (Some(_), Some(_)) => WordStatus::Both,
            (Some(_), None) => WordStatus::OnlyA,
            (None, Some(_)) => WordStatus::OnlyB,
            (None, None) => WordStatus::Neither,
        };
        let delta = x.zip(y).map(|(p, q)| q.start_ms - p.start_ms);
        match status {
            WordStatus::Both => {
                shared += 1;
                max_abs = max_abs.max(delta.unwrap_or(0).abs());
            }
            WordStatus::OnlyA => only_a.push(i),
            WordStatus::OnlyB => only_b.push(i),
            WordStatus::Neither => neither += 1,
        }
        words.push(WordRow {
            index: i,
            text: w.text.clone(),
            source_start_ms: w.start_ms,
            a: x.map(|p| (p.start_ms, p.end_ms)),
            b: y.map(|p| (p.start_ms, p.end_ms)),
            status,
            delta_ms: delta,
        });
    }

    // Snap targets: walk backwards so each cut word points at the next kept one.
    let mut entries = vec![
        SeekEntry {
            index: 0,
            a_ms: None,
            b_ms: None,
            a_snap_ms: a_end,
            b_snap_ms: b_end
        };
        n
    ];
    let (mut next_a, mut next_b) = (a_end, b_end);
    for i in (0..n).rev() {
        if let Some(p) = aw.get(&i) {
            next_a = p.start_ms;
        }
        if let Some(p) = bw.get(&i) {
            next_b = p.start_ms;
        }
        entries[i] = SeekEntry {
            index: i,
            a_ms: aw.get(&i).map(|p| p.start_ms),
            b_ms: bw.get(&i).map(|p| p.start_ms),
            a_snap_ms: next_a,
            b_snap_ms: next_b,
        };
    }

    let sa = summary(a, &aw, n);
    let sb = summary(b, &bw, n);
    let report = CompareReport {
        schema: COMPARE_SCHEMA.into(),
        source_blake3: a.provenance.source_blake3.clone(),
        transcript_blake3: hash,
        duration_delta_ms: sb.output_ms - sa.output_ms,
        cuts_delta: sb.cuts as i64 - sa.cuts as i64,
        a: sa,
        b: sb,
        shared_words: shared,
        only_a,
        only_b,
        cut_in_both: neither,
        max_abs_delta_ms: max_abs,
        words,
    };
    Ok((
        report,
        SeekMap {
            a_duration_ms: a_end,
            b_duration_ms: b_end,
            entries,
        },
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareLayout {
    SideBySide,
    Stacked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareAudio {
    A,
    B,
    Mute,
}

/// Inputs for the review render. `a_video` / `b_video` are renders of the two
/// variants (their output timelines are what the seek map describes).
pub struct CompareRenderRequest<'a> {
    pub a_video: &'a Path,
    pub b_video: &'a Path,
    pub a_label: &'a str,
    pub b_label: &'a str,
    pub layout: CompareLayout,
    /// Transcript word both panels are locked on; both variants must keep it.
    pub anchor_word: usize,
    /// Context shown before the anchor word (clamped so both sides stay locked).
    pub lead_ms: i64,
    /// Longest review clip; defaults to what both videos have left.
    pub max_duration_ms: Option<i64>,
    pub panel_width: u32,
    pub panel_height: u32,
    pub fps: u32,
    pub audio: CompareAudio,
    pub encoder: Encoder,
    pub font: &'a Path,
    pub text_dir: &'a Path,
    pub output: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltCompare {
    pub command: FfmpegCommand,
    /// Write each (exact content) before running the command.
    pub text_files: Vec<TextFile>,
    pub start_a_ms: i64,
    pub start_b_ms: i64,
    pub duration_ms: i64,
    /// Where the anchor word begins in the review clip, identical in A and B.
    pub anchor_at_ms: i64,
}

fn secs(ms: i64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Builds the word-locked compare command: both videos are seeked so the
/// anchor word starts at the same instant in the clip, scaled into equal
/// panels, labelled with `drawtext`, and stacked.
pub fn build_compare_render(
    seek: &SeekMap,
    report: &CompareReport,
    req: &CompareRenderRequest<'_>,
) -> Result<BuiltCompare, VariantError> {
    let row = report
        .words
        .get(req.anchor_word)
        .ok_or(VariantError::NoSuchWord(req.anchor_word))?;
    let (Some(ta), Some(tb)) = (
        seek.entry(req.anchor_word).and_then(|e| e.a_ms),
        seek.entry(req.anchor_word).and_then(|e| e.b_ms),
    ) else {
        return Err(VariantError::WordNotShared {
            index: req.anchor_word,
            detail: format!(
                "cut in {}; next shared word is {:?}",
                match row.status {
                    WordStatus::OnlyA => &report.b.name,
                    WordStatus::OnlyB => &report.a.name,
                    _ => "both",
                },
                seek.next_shared(req.anchor_word)
            ),
        });
    };
    if req.panel_width == 0 || req.panel_height == 0 || req.fps == 0 || req.lead_ms < 0 {
        return Err(VariantError::Render("panel size, fps, lead".into()));
    }
    let lead = req.lead_ms.min(ta).min(tb);
    let (sa, sb) = (ta - lead, tb - lead);
    let mut dur = (seek.a_duration_ms - sa).min(seek.b_duration_ms - sb);
    if let Some(m) = req.max_duration_ms {
        dur = dur.min(m);
    }
    if dur <= lead {
        return Err(VariantError::Render("nothing left after the anchor".into()));
    }
    let word_ms = row
        .a
        .map(|(s, e)| e - s)
        .unwrap_or(0)
        .max(row.b.map(|(s, e)| e - s).unwrap_or(0))
        .max(1);

    let (w, h) = (req.panel_width, req.panel_height);
    let font_px = (h / 18).max(12);
    let font = escape_filter_path(req.font)?;
    let mut text_files = Vec::new();
    let mut panel = |input: usize, label: &str, tag: &str| -> Result<String, VariantError> {
        let lpath: PathBuf = req.text_dir.join(format!("compare-{tag}-label.txt"));
        let wpath: PathBuf = req.text_dir.join(format!("compare-{tag}-word.txt"));
        text_files.push(TextFile {
            path: lpath.clone(),
            content: label.to_string(),
        });
        text_files.push(TextFile {
            path: wpath.clone(),
            content: row.text.clone(),
        });
        let (lp, wp) = (escape_filter_path(&lpath)?, escape_filter_path(&wpath)?);
        Ok(format!(
            "[{input}:v]fps={fps},scale={w}:{h}:force_original_aspect_ratio=decrease,\
pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black,setsar=1,\
drawtext=fontfile={font}:textfile={lp}:expansion=none:fontsize={font_px}:fontcolor=white:\
box=1:boxcolor=black@0.6:boxborderw=8:x=16:y=16,\
drawtext=fontfile={font}:textfile={wp}:expansion=none:fontsize={font_px}:fontcolor=yellow:\
box=1:boxcolor=black@0.6:boxborderw=8:x=(w-text_w)/2:y=h-text_h-24:\
enable='between(t,{ls},{le})'[p{input}]",
            fps = req.fps,
            ls = secs(lead),
            le = secs(lead + word_ms),
        ))
    };
    let pa = panel(0, &format!("A: {}", req.a_label), "a")?;
    let pb = panel(1, &format!("B: {}", req.b_label), "b")?;
    let stack = match req.layout {
        CompareLayout::SideBySide => "hstack",
        CompareLayout::Stacked => "vstack",
    };
    let graph = format!("{pa};{pb};[p0][p1]{stack}=inputs=2[v]");

    let mut cmd = FfmpegCommand::default();
    for s in ["-hide_banner", "-y", "-ss"] {
        cmd.text(s);
    }
    cmd.text(secs(sa));
    cmd.text("-i");
    cmd.path(req.a_video);
    cmd.text("-ss");
    cmd.text(secs(sb));
    cmd.text("-i");
    cmd.path(req.b_video);
    cmd.text("-filter_complex");
    cmd.text(&graph);
    cmd.text("-map");
    cmd.text("[v]");
    match req.audio {
        CompareAudio::A => {
            cmd.text("-map");
            cmd.text("0:a:0?");
        }
        CompareAudio::B => {
            cmd.text("-map");
            cmd.text("1:a:0?");
        }
        CompareAudio::Mute => {
            cmd.text("-an");
        }
    }
    cmd.text("-t");
    cmd.text(secs(dur));
    let enc: &[&str] = match req.encoder {
        Encoder::MediaFoundation => &["-c:v", "h264_mf", "-b:v", "8M"],
        Encoder::Nvenc => &["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "20"],
        Encoder::VideoToolbox => &["-c:v", "h264_videotoolbox", "-b:v", "8M"],
    };
    for s in enc {
        cmd.text(*s);
    }
    for s in [
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "160k",
        "-movflags",
        "+faststart",
    ] {
        cmd.text(s);
    }
    cmd.path(req.output);
    Ok(BuiltCompare {
        command: cmd,
        text_files,
        start_a_ms: sa,
        start_b_ms: sb,
        duration_ms: dur,
        anchor_at_ms: lead,
    })
}

/// Writes the label files and runs `ffmpeg`; returns its stderr on failure.
pub fn execute_compare(ffmpeg: &Path, built: &BuiltCompare) -> Result<(), VariantError> {
    for f in &built.text_files {
        rightkit_fs::atomic_write_with(
            &f.path,
            rightkit_fs::AtomicOptions {
                create_parents: true,
                ..Default::default()
            },
            |file| file.write_all(f.content.as_bytes()),
        )
        .map_err(|source| VariantError::Io {
            path: f.path.display().to_string(),
            source: source.into(),
        })?;
    }
    let out = spawn_ffmpeg(
        ffmpeg,
        &built.command,
        StdioPolicy {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
        },
    )
    .and_then(wait_with_output)
    .map_err(|source| VariantError::Io {
        path: ffmpeg.display().to_string(),
        source,
    })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(VariantError::Render(
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}
