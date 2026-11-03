//! Robust ingest of raw talking-head recordings (iPhone/camera MOV and MP4).
//!
//! `ingest` deep-probes each file through the typed ffprobe boundary, decides whether
//! it needs a normalised intermediate, and registers exactly one timeline source per
//! file:
//!
//! - **No normalisation needed** (the common case, including rotated and HDR clips):
//!   the original is registered as-is ([`crate::source::add_source`]). Rotation is
//!   reported as DISPLAY orientation (see [`crate::probe`]) and ffmpeg's default
//!   autorotate makes every render honour it. HDR (PQ/HLG) is flagged for later
//!   tone-mapping; pixels are never altered here.
//! - **Variable frame rate**: a constant-frame-rate intermediate at the project fps is
//!   produced (video retimed with `fps`, audio kept in sync via `aresample`, both
//!   starting at 0, PCM audio so word timings map exactly). It is content-addressed
//!   under `media/derived/<blake3>.mov` with a `media/derived/<original>-<transform>.json`
//!   manifest recording the transform and the original's BLAKE3. The registered source
//!   is the intermediate; [`source_provenance`] resolves it back to the original.
//!   Rotation is baked into the intermediate by autorotate. 10-bit and colour tags are
//!   preserved (no tone-mapping).
//! - **Unsupported codec / no video**: a typed per-file error; nothing is registered.
//!
//! Originals are never modified. Unless `reference` is set they are copied once into
//! `media/` (no proxy) or `media/originals/` (proxy) so the project owns them.
//! Everything is idempotent per original BLAKE3: a re-ingest re-registers nothing and
//! re-encodes nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rightkit_ffmpeg::{FfmpegArg, FfmpegCommand};
use rightkit_timeline::{Fps, SourceId, Ticks};
use serde::{Deserialize, Serialize};

use crate::error::ProjectError;
use crate::fsutil::atomic_write;
use crate::probe::{
    rate_pair, read_document, run_probe_json, stream_rotation, summarize, video_index,
};
use crate::project::{Access, Project, SourceMode};
use crate::source::{add_source, copy_verified, hash_file, media_file_name, register_in_place};
use crate::tools::MediaTools;
use crate::worker::{run_ffmpeg, RunOptions};
use rightkit_ffmpeg as rk;
use rightkit_ffmpeg::encoders::IntermediateFormat;
use rightkit_ffmpeg::probe::{PacketSample, ProbeJson, ProbeStream};

/// Directory (project-relative) holding content-addressed intermediates and manifests.
pub const DERIVED_DIR: &str = "media/derived";
/// Directory (project-relative) holding owned copies of originals that were proxied.
pub const ORIGINALS_DIR: &str = "media/originals";
pub const MANIFEST_KIND: &str = "cutright.derived";
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Video codecs the editing pipeline accepts.
pub const SUPPORTED_VIDEO_CODECS: &[&str] = &["h264", "hevc", "prores", "mpeg4"];

/// Packets sampled for timestamp-jitter analysis.
const SAMPLE_PACKETS: u32 = 600;
/// Deadline for the packet sample; the same bound `probe.rs` gives its ffprobe runs.
const PACKET_PROBE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A frame delta this far (relative) from the median counts as an outlier.
const JITTER_TOLERANCE: f64 = 0.25;
/// Outliers needed before timestamps are called variable.
const JITTER_MIN_OUTLIERS: usize = 2;
/// `r_frame_rate` and `avg_frame_rate` differing by more than this (relative) means VFR.
const RATE_MISMATCH_TOLERANCE: f64 = 0.01;

// ---------------------------------------------------------------- errors

/// Ingest failure for one file. [`IngestError::code`] is the stable machine code.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unsupported video codec {codec:?} in {file} (supported: h264, hevc, prores, mpeg4)")]
    UnsupportedCodec { codec: String, file: String },
    #[error("{file} has no video stream")]
    NoVideo { file: String },
    #[error(transparent)]
    Project(#[from] ProjectError),
}

impl IngestError {
    pub fn code(&self) -> &'static str {
        match self {
            IngestError::UnsupportedCodec { .. } => "unsupported_codec",
            IngestError::NoVideo { .. } => "no_video_stream",
            IngestError::Project(e) => e.code(),
        }
    }
}

// ---------------------------------------------------------------- deep probe

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrKind {
    /// SMPTE ST 2084 (PQ / HDR10 / Dolby Vision base layer).
    Pq,
    /// ARIB STD-B67 (HLG, iPhone HDR video).
    Hlg,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimestampStats {
    pub samples: usize,
    pub median_delta_s: f64,
    pub min_delta_s: f64,
    pub max_delta_s: f64,
    pub outliers: usize,
}

/// Everything ingest decides on, from one ffprobe pass plus a packet-timestamp sample.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeepProbe {
    pub container: String,
    pub video_codec: String,
    pub video_profile: Option<String>,
    pub pix_fmt: Option<String>,
    pub bit_depth: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    /// Display orientation (rotation applied): what the timeline sees.
    pub display_width: u32,
    pub display_height: u32,
    /// Display rotation in degrees (ffprobe convention, counter-clockwise), 0/90/180/270.
    pub rotation: i32,
    pub r_frame_rate: Option<(u32, u32)>,
    pub avg_frame_rate: Option<(u32, u32)>,
    pub timestamp_stats: Option<TimestampStats>,
    pub vfr: bool,
    pub vfr_reasons: Vec<&'static str>,
    pub hdr: Option<HdrKind>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub has_audio: bool,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<u32>,
    pub audio_sample_rate: Option<u32>,
    pub duration: Ticks,
    /// UTC RFC 3339 from container metadata, when present.
    pub creation_time: Option<String>,
    pub timecode: Option<String>,
}

/// The stream `-select_streams v:0` names: the first video stream, cover art included.
fn first_video_stream(typed: &ProbeJson) -> Option<&ProbeStream> {
    typed.streams.iter().find(|s| s.is_video())
}

/// Packet times in seconds on the first video stream's time base: `pts`, or `dts` for a packet
/// that has none. Empty when that stream has no usable time base.
fn packet_seconds(typed: &ProbeJson, packets: &[PacketSample]) -> Vec<f64> {
    let Some(time_base) = first_video_stream(typed).and_then(|s| s.time_base) else {
        return Vec::new();
    };
    packets
        .iter()
        .filter_map(|p| {
            p.pts_seconds(time_base)
                .or_else(|| p.dts_seconds(time_base))
        })
        .collect()
}

fn rate_value(r: (u32, u32)) -> f64 {
    r.0 as f64 / r.1 as f64
}

fn known(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|v| !v.is_empty() && *v != "unknown" && *v != "reserved")
        .map(str::to_string)
}

fn bit_depth_of(raw: Option<u32>, pix_fmt: Option<&str>) -> u32 {
    if let Some(n) = raw.filter(|n| *n > 0) {
        return n;
    }
    match pix_fmt.unwrap_or("") {
        p if p.contains("16") => 16,
        p if p.contains("12") => 12,
        p if p.contains("10") || p.starts_with("p010") => 10,
        _ => 8,
    }
}

/// Container `creation_time` normalised to UTC RFC 3339 (seconds), else `None`.
fn normalise_time(text: &str) -> Option<String> {
    use chrono::{DateTime, Utc};
    let text = text.trim();
    let parsed = DateTime::parse_from_rfc3339(text)
        .or_else(|_| DateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%z"))
        .or_else(|_| DateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f%z"))
        .map(|d| d.with_timezone(&Utc))
        .ok()?;
    Some(parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Frame-timestamp regularity of a presentation-time sample: deltas far from the median
/// are outliers. `None` when there are too few samples to judge.
pub fn analyze_timestamps(pts: &[f64]) -> Option<TimestampStats> {
    let mut sorted: Vec<f64> = pts.iter().copied().filter(|p| p.is_finite()).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let deltas: Vec<f64> = sorted.windows(2).map(|w| w[1] - w[0]).collect();
    if deltas.len() < 3 {
        return None;
    }
    let mut by_size = deltas.clone();
    by_size.sort_by(|a, b| a.total_cmp(b));
    let median = by_size[by_size.len() / 2];
    if median <= 0.0 {
        return None;
    }
    let outliers = deltas
        .iter()
        .filter(|d| (**d - median).abs() > JITTER_TOLERANCE * median)
        .count();
    Some(TimestampStats {
        samples: deltas.len(),
        median_delta_s: median,
        min_delta_s: by_size[0],
        max_delta_s: *by_size.last().expect("non-empty"),
        outliers,
    })
}

/// Parses ffprobe `-show_format -show_streams` JSON plus optional packet-sample JSON.
pub fn parse_deep(
    main: &[u8],
    packets: Option<&[PacketSample]>,
) -> Result<DeepProbe, ProjectError> {
    deep_from_document(&read_document(main)?, packets)
}

/// The deep probe of an already-read ffprobe document plus optional packet samples.
fn deep_from_document(
    typed: &ProbeJson,
    packets: Option<&[PacketSample]>,
) -> Result<DeepProbe, ProjectError> {
    let summary = summarize(typed)?;
    let video_at = video_index(typed);
    let video = video_at.map(|i| &typed.streams[i]);
    let audio = typed.first_audio();
    let rotation = video.map_or(0, stream_rotation);
    let (coded_width, coded_height) = video
        .and_then(|v| Some((v.width?, v.height?)))
        .unwrap_or((0, 0));
    let display = summary.video.clone();
    let r_frame_rate = video.and_then(|v| v.r_frame_rate).and_then(rate_pair);
    let avg_frame_rate = video.and_then(|v| v.avg_frame_rate).and_then(rate_pair);

    let pts = packets.map_or_else(Vec::new, |p| packet_seconds(typed, p));
    let timestamp_stats = analyze_timestamps(&pts);

    let mut vfr_reasons = Vec::new();
    if let (Some(r), Some(a)) = (r_frame_rate, avg_frame_rate) {
        let (r, a) = (rate_value(r), rate_value(a));
        if (r - a).abs() / r > RATE_MISMATCH_TOLERANCE {
            vfr_reasons.push("frame_rate_mismatch");
        }
    }
    if timestamp_stats
        .as_ref()
        .is_some_and(|s| s.outliers >= JITTER_MIN_OUTLIERS)
    {
        vfr_reasons.push("timestamp_jitter");
    }

    let transfer = video.and_then(|v| known(&v.color_transfer));
    let hdr = match transfer.as_deref() {
        Some("smpte2084") => Some(HdrKind::Pq),
        Some("arib-std-b67") => Some(HdrKind::Hlg),
        _ => None,
    };
    let pix_fmt = video.and_then(|v| known(&v.pix_fmt));
    let tag = |key: &str| -> Option<String> {
        typed
            .format
            .tags
            .get(key)
            .cloned()
            .or_else(|| typed.streams.iter().find_map(|s| s.tags.get(key).cloned()))
    };
    let creation_time = tag("com.apple.quicktime.creationdate")
        .or_else(|| tag("creation_time"))
        .and_then(|t| normalise_time(&t));
    Ok(DeepProbe {
        container: typed.format.format_name.clone().unwrap_or_default(),
        video_codec: video.and_then(|v| v.codec_name.clone()).unwrap_or_default(),
        video_profile: video.and_then(|v| known(&v.profile)),
        bit_depth: bit_depth_of(
            video.and_then(|v| v.bits_per_raw_sample),
            pix_fmt.as_deref(),
        ),
        pix_fmt,
        coded_width,
        coded_height,
        display_width: display.as_ref().map_or(0, |d| d.width),
        display_height: display.as_ref().map_or(0, |d| d.height),
        rotation,
        r_frame_rate,
        avg_frame_rate,
        timestamp_stats,
        vfr: !vfr_reasons.is_empty(),
        vfr_reasons,
        hdr,
        color_primaries: video.and_then(|v| known(&v.color_primaries)),
        color_transfer: transfer,
        color_space: video.and_then(|v| known(&v.color_space)),
        has_audio: summary.has_audio,
        audio_codec: audio.and_then(|a| a.codec_name.clone()),
        audio_channels: audio.and_then(|a| a.channels),
        audio_sample_rate: audio.and_then(|a| a.sample_rate),
        duration: summary.duration,
        creation_time,
        timecode: tag("timecode"),
    })
}

/// Deep-probes `file`: streams/format plus the first packets' timestamps for VFR detection.
pub fn probe_deep(tools: &MediaTools, file: &Path) -> Result<DeepProbe, ProjectError> {
    let typed = run_probe_json(tools, file)?;
    // A failed packet sample only costs the jitter signal; the rate comparison remains.
    let packets = first_video_stream(&typed)
        .map(|s| s.index)
        .and_then(|index| {
            rk::probe::probe_packets(
                &rk::verify_ffprobe_handle(
                    &tools.ffprobe,
                    rk::DECODE_VARIANT,
                    crate::tools::VERIFY_TIMEOUT,
                )
                .ok()?,
                file,
                index,
                SAMPLE_PACKETS,
                Some(PACKET_PROBE_TIMEOUT),
                &rk::CancellationToken::new(),
            )
            .ok()
        });
    deep_from_document(&typed, packets.as_deref())
}

// ---------------------------------------------------------------- manifest

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestOriginal {
    pub blake3: SourceId,
    pub name: String,
    pub size: u64,
    pub mode: SourceMode,
    /// Project-relative (copy) or absolute (reference).
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestTransform {
    /// `cfr`: constant-frame-rate intermediate.
    pub kind: String,
    pub fps: Fps,
    pub reasons: Vec<String>,
    /// Display rotation baked into the pixels (0 when none).
    pub rotation_applied: i32,
    pub hdr: Option<HdrKind>,
    pub video_codec: String,
    pub pix_fmt: String,
    pub audio_codec: Option<String>,
    pub ffmpeg_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDerived {
    pub blake3: SourceId,
    /// Project-relative.
    pub path: String,
    pub size: u64,
}

/// Provenance of an intermediate: what it was made from and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedManifest {
    pub schema_version: u32,
    pub kind: String,
    pub original: ManifestOriginal,
    pub transform: ManifestTransform,
    pub derived: ManifestDerived,
    /// RFC 3339 UTC.
    pub created: String,
}

fn manifest_rel(original: &SourceId, transform: &str) -> String {
    format!("{DERIVED_DIR}/{original}-{transform}.json")
}

fn transform_key(fps: Fps) -> String {
    format!("cfr-{}-{}", fps.num, fps.den)
}

fn read_manifest(path: &Path) -> Option<DerivedManifest> {
    let m: DerivedManifest = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (m.schema_version == MANIFEST_SCHEMA_VERSION && m.kind == MANIFEST_KIND).then_some(m)
}

/// Finds the manifest whose intermediate is registered source `id` (None for originals).
pub fn source_provenance(project: &Project, id: &SourceId) -> Option<DerivedManifest> {
    let dir = project.dir.join(DERIVED_DIR);
    let mut found: Vec<DerivedManifest> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| read_manifest(&p))
        .filter(|m| &m.derived.blake3 == id)
        .collect();
    found.sort_by(|a, b| a.created.cmp(&b.created));
    found.into_iter().next()
}

// ---------------------------------------------------------------- options & report

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOrder {
    /// Order by container creation time (file mtime when absent); ties keep given order.
    CreationTime,
    /// Keep the caller's order.
    Given,
}

pub struct IngestOptions {
    pub order: IngestOrder,
    /// Register originals in place instead of copying them into the project.
    pub reference: bool,
    /// Probe and plan only; change nothing (project may be opened read-only).
    pub dry_run: bool,
    /// CFR target for VFR clips; the project fps when `None`.
    pub target_fps: Option<Fps>,
    pub cancel: Arc<AtomicBool>,
    /// Per-file ffmpeg time limit.
    pub timeout: Duration,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            order: IngestOrder::CreationTime,
            reference: false,
            dry_run: false,
            target_fps: None,
            cancel: Arc::new(AtomicBool::new(false)),
            timeout: Duration::from_secs(4 * 60 * 60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DerivedInfo {
    /// Project-relative manifest path.
    pub manifest: String,
    pub original_blake3: SourceId,
    /// `true` when an existing intermediate was reused (no encode).
    pub reused: bool,
}

/// Per-file result. `index` is the position in timeline order.
#[derive(Debug, Clone, Serialize)]
pub struct FileReport {
    pub index: usize,
    pub input: PathBuf,
    pub name: String,
    /// `registered`, `already_registered`, `would_register` or `error`.
    pub status: &'static str,
    pub needs_proxy: bool,
    /// Why a proxy is needed (`frame_rate_mismatch`, `timestamp_jitter`).
    pub reasons: Vec<String>,
    /// Non-blocking facts for later stages (`hdr_pq`, `hdr_hlg`, `rotated_90`, ...).
    pub notes: Vec<String>,
    pub original_blake3: Option<SourceId>,
    /// The registered timeline source (the intermediate when proxied).
    pub source_id: Option<SourceId>,
    pub derived: Option<DerivedInfo>,
    pub display_width: u32,
    pub display_height: u32,
    pub rotation: i32,
    pub fps: Option<(u32, u32)>,
    pub has_audio: bool,
    pub duration: Option<Ticks>,
    pub hdr: Option<HdrKind>,
    pub video_codec: String,
    pub created: Option<String>,
    /// `container_tag` or `file_mtime`.
    pub created_from: &'static str,
    pub error: Option<FileError>,
}

impl FileReport {
    fn new(index: usize, input: &Path) -> Self {
        Self {
            index,
            input: input.to_path_buf(),
            name: input
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            status: "error",
            needs_proxy: false,
            reasons: vec![],
            notes: vec![],
            original_blake3: None,
            source_id: None,
            derived: None,
            display_width: 0,
            display_height: 0,
            rotation: 0,
            fps: None,
            has_audio: false,
            duration: None,
            hdr: None,
            video_codec: String::new(),
            created: None,
            created_from: "file_mtime",
            error: None,
        }
    }

    fn fail(mut self, e: &IngestError) -> Self {
        self.status = "error";
        self.error = Some(FileError {
            code: e.code().to_string(),
            message: e.to_string(),
        });
        self
    }
}

// ---------------------------------------------------------------- ingest

fn mtime_rfc3339(path: &Path) -> Option<String> {
    let t = fs::metadata(path).ok()?.modified().ok()?;
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Opens the project at `dir` for writing (or reading for a dry run) and ingests.
pub fn ingest_dir(
    dir: &Path,
    tools: &MediaTools,
    files: &[PathBuf],
    opts: &IngestOptions,
) -> Result<Vec<FileReport>, ProjectError> {
    let access = if opts.dry_run {
        Access::Read
    } else {
        Access::Write
    };
    let mut project = Project::open(dir, access)?;
    Ok(ingest(&mut project, tools, files, opts))
}

/// Imports `files` into `project`, one source per file, in timeline order. Never fails
/// as a whole: each file's outcome (including a typed error) is in its report, and one
/// bad file does not stop the others. Reports come back in timeline order.
pub fn ingest(
    project: &mut Project,
    tools: &MediaTools,
    files: &[PathBuf],
    opts: &IngestOptions,
) -> Vec<FileReport> {
    struct Probed {
        file: PathBuf,
        deep: Result<DeepProbe, ProjectError>,
        created: Option<String>,
        created_from: &'static str,
    }
    let mut probed: Vec<Probed> = files
        .iter()
        .map(|f| {
            let deep = if f.is_file() {
                probe_deep(tools, f)
            } else {
                Err(ProjectError::SourceNotFound(f.display().to_string()))
            };
            let (created, created_from) =
                match deep.as_ref().ok().and_then(|d| d.creation_time.clone()) {
                    Some(t) => (Some(t), "container_tag"),
                    None => (mtime_rfc3339(f), "file_mtime"),
                };
            Probed {
                file: f.clone(),
                deep,
                created,
                created_from,
            }
        })
        .collect();
    if opts.order == IngestOrder::CreationTime {
        // RFC 3339 UTC with fixed width sorts chronologically as text; stable sort
        // keeps the given order for ties and for files with no time at all.
        probed.sort_by(|a, b| a.created.cmp(&b.created));
    }

    let mut reports = Vec::with_capacity(probed.len());
    for (index, p) in probed.into_iter().enumerate() {
        let mut report = FileReport::new(index, &p.file);
        report.created = p.created;
        report.created_from = p.created_from;
        if opts.cancel.load(Ordering::Relaxed) {
            reports.push(report.fail(&IngestError::Project(ProjectError::RenderCancelled)));
            continue;
        }
        let deep = match p.deep {
            Ok(d) => d,
            Err(e) => {
                reports.push(report.fail(&e.into()));
                continue;
            }
        };
        match ingest_one(project, tools, &p.file, &deep, opts, &mut report) {
            Ok(()) => reports.push(report),
            Err(e) => reports.push(report.fail(&e)),
        }
    }
    reports
}

fn describe(report: &mut FileReport, deep: &DeepProbe) {
    report.display_width = deep.display_width;
    report.display_height = deep.display_height;
    report.rotation = deep.rotation;
    report.has_audio = deep.has_audio;
    report.duration = Some(deep.duration);
    report.hdr = deep.hdr;
    report.video_codec = deep.video_codec.clone();
    report.fps = deep.avg_frame_rate.or(deep.r_frame_rate);
    report.needs_proxy = deep.vfr;
    report.reasons = deep.vfr_reasons.iter().map(|r| r.to_string()).collect();
    match deep.hdr {
        Some(HdrKind::Pq) => report.notes.push("hdr_pq".into()),
        Some(HdrKind::Hlg) => report.notes.push("hdr_hlg".into()),
        None => {}
    }
    if deep.rotation != 0 {
        report.notes.push(format!("rotated_{}", deep.rotation));
    }
    if deep.bit_depth > 8 {
        report.notes.push(format!("{}_bit", deep.bit_depth));
    }
}

fn ingest_one(
    project: &mut Project,
    tools: &MediaTools,
    file: &Path,
    deep: &DeepProbe,
    opts: &IngestOptions,
    report: &mut FileReport,
) -> Result<(), IngestError> {
    let name = file.display().to_string();
    if deep.video_codec.is_empty() || deep.display_width == 0 {
        return Err(IngestError::NoVideo { file: name });
    }
    if !SUPPORTED_VIDEO_CODECS.contains(&deep.video_codec.as_str()) {
        return Err(IngestError::UnsupportedCodec {
            codec: deep.video_codec.clone(),
            file: name,
        });
    }
    describe(report, deep);
    let file = std::path::absolute(file)
        .map_err(|e| ProjectError::io(format!("resolving {}", file.display()), e))?;
    let (original_id, original_size) = hash_file(&file).map_err(IngestError::from)?;
    report.original_blake3 = Some(original_id.clone());

    if !deep.vfr {
        let r = add_source(project, tools, &file, opts.reference, opts.dry_run)?;
        report.status = r.status;
        report.source_id = Some(r.id);
        return Ok(());
    }

    // ---- VFR: constant-frame-rate intermediate, registered instead of the original.
    let fps = opts.target_fps.unwrap_or(project.meta.fps);
    report.fps = Some((fps.num, fps.den));
    let key = transform_key(fps);
    let manifest_path = project.dir.join(manifest_rel(&original_id, &key));
    let reusable = read_manifest(&manifest_path).filter(|m| {
        m.original.blake3 == original_id
            && hash_file(&project.dir.join(&m.derived.path))
                .map(|(h, _)| h == m.derived.blake3)
                .unwrap_or(false)
    });
    if opts.dry_run {
        report.status = "would_register";
        report.source_id = reusable.map(|m| m.derived.blake3);
        return Ok(());
    }

    // Preserve the original: an owned, verified copy (or a recorded reference).
    let (mode, original_path, encode_input) = if opts.reference {
        (
            SourceMode::Reference,
            file.to_string_lossy().into_owned(),
            file.clone(),
        )
    } else {
        let rel = format!("{ORIGINALS_DIR}/{}", media_file_name(&original_id, &file));
        let dest = project.dir.join(&rel);
        fs::create_dir_all(project.dir.join(ORIGINALS_DIR))
            .map_err(|e| ProjectError::io("creating media/originals", e))?;
        let intact = dest.is_file()
            && hash_file(&dest)
                .map(|(h, _)| h == original_id)
                .unwrap_or(false);
        if !intact {
            copy_verified(&file, &dest, &original_id)?;
        }
        (SourceMode::Copy, rel, dest)
    };

    let (manifest, reused) = match reusable {
        Some(m) => (m, true),
        None => {
            let m = encode_cfr(
                project,
                tools,
                &encode_input,
                deep,
                fps,
                &key,
                ManifestOriginal {
                    blake3: original_id.clone(),
                    name: file
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    size: original_size,
                    mode,
                    path: original_path,
                },
                opts,
            )?;
            (m, false)
        }
    };
    let r = register_in_place(
        project,
        tools,
        &manifest.derived.path,
        Some(manifest.derived.blake3.as_str()),
    )?;
    report.status = r.status;
    report.source_id = Some(r.id);
    report.duration = Some(r.info.duration);
    if let Some(v) = &r.info.video {
        report.display_width = v.width;
        report.display_height = v.height;
    }
    report.has_audio = r.info.has_audio;
    report.rotation = 0;
    report.derived = Some(DerivedInfo {
        manifest: manifest_rel(&original_id, &key),
        original_blake3: original_id,
        reused,
    });
    Ok(())
}

fn path_ref(arg: &FfmpegArg, input: &Path, out: &Path) -> String {
    match arg {
        FfmpegArg::Text(t) => t.clone(),
        FfmpegArg::Path(p) if p == input => "<original>".into(),
        FfmpegArg::Path(p) if p == out => "<intermediate>".into(),
        FfmpegArg::Path(p) => p.display().to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_cfr(
    project: &Project,
    tools: &MediaTools,
    input: &Path,
    deep: &DeepProbe,
    fps: Fps,
    key: &str,
    original: ManifestOriginal,
    opts: &IngestOptions,
) -> Result<DerivedManifest, IngestError> {
    let dir = project.dir.join(DERIVED_DIR);
    fs::create_dir_all(&dir).map_err(|e| ProjectError::io("creating media/derived", e))?;
    rightkit_fs::sweep_stale_temps(&dir, Duration::ZERO)
        .map_err(|e| ProjectError::io("sweeping media/derived", e.into()))?;
    let partial =
        crate::fsutil::ffmpeg_temp_path(&dir.join(format!("{}-{key}.mov", original.blake3)))?;
    let rate = fps.ffmpeg_rational();
    let ten_bit = deep.bit_depth >= 10;
    let format = match (ten_bit, deep.pix_fmt.as_deref().unwrap_or("")) {
        (true, p) if p.contains("422") => IntermediateFormat::Yuv422p10,
        (true, _) => IntermediateFormat::Yuv420p10,
        (false, _) => IntermediateFormat::Yuv420p8,
    };
    let pix_fmt = format.pix_fmt();
    let encode = tools.intermediate(format)?;

    let mut cmd = FfmpegCommand::default();
    cmd.texts([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-progress",
        "pipe:1",
        "-nostats",
        "-i",
    ]);
    cmd.path(input);
    // First video stream plus all audio. Autorotate (ffmpeg default) bakes the display
    // rotation into the pixels and drops the displaymatrix.
    cmd.texts(["-map", "0:v:0", "-map", "0:a?"]);
    // Preserve the colour description so HDR stays identifiable (no tone-mapping here).
    // setparams tags the frames; encoder options alone are overridden by the filter graph.
    let mut vf = format!("fps={rate}:start_time=0");
    let tags: Vec<String> = [
        ("color_primaries", &deep.color_primaries),
        ("color_trc", &deep.color_transfer),
        ("colorspace", &deep.color_space),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={v}")))
    .collect();
    if !tags.is_empty() {
        vf.push_str(&format!(",setparams={}", tags.join(":")));
    }
    cmd.texts(["-vf", &vf, "-fps_mode", "cfr"]);
    cmd.texts(encode.args.iter().copied());
    cmd.texts(["-pix_fmt", pix_fmt]);
    if deep.has_audio {
        // PCM keeps speech untouched for transcription; async resampling pads or trims
        // the head so audio and video both start at 0 and stay locked.
        cmd.texts(["-af", "aresample=async=1:first_pts=0", "-c:a", "pcm_s16le"]);
    }
    // write_colr keeps the colour description (HDR transfer) in the QuickTime header.
    cmd.texts(["-movflags", "+write_colr", "-f", "mov"]);
    cmd.path(partial.clone());
    let ffmpeg_args = cmd
        .args()
        .iter()
        .map(|a| path_ref(a, input, &partial))
        .collect();

    let result = run_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        RunOptions {
            timeout: opts.timeout,
            cancel: Arc::clone(&opts.cancel),
            on_progress: &mut |_| {},
        },
    );
    if let Err(e) = result {
        let _ = fs::remove_file(&partial);
        return Err(e.into());
    }

    // Verify the intermediate is a faithful, in-sync stand-in before keeping it.
    let check = (|| -> Result<(), ProjectError> {
        let after = probe_deep(tools, &partial)?;
        let tolerance = (3.0 * rate_value((fps.num, fps.den)).recip()).max(0.1);
        let diff = (after.duration.0 - deep.duration.0).abs() as f64
            / rightkit_timeline::TICKS_PER_SECOND as f64;
        let bad = |m: String| ProjectError::RenderFailed { message: m };
        if after.has_audio != deep.has_audio {
            return Err(bad("intermediate lost or gained an audio stream".into()));
        }
        if (after.display_width, after.display_height) != (deep.display_width, deep.display_height)
        {
            return Err(bad(format!(
                "intermediate is {}x{}, expected display {}x{}",
                after.display_width, after.display_height, deep.display_width, deep.display_height
            )));
        }
        if diff > tolerance {
            return Err(bad(format!(
                "intermediate duration differs from the original by {diff:.3}s"
            )));
        }
        if after.vfr {
            return Err(bad("intermediate is still variable frame rate".into()));
        }
        Ok(())
    })();
    if let Err(e) = check {
        let _ = fs::remove_file(&partial);
        return Err(e.into());
    }

    let (derived_id, size) = hash_file(&partial)?;
    let rel = format!("{DERIVED_DIR}/{derived_id}.mov");
    let dest = project.dir.join(&rel);
    match rightkit_fs::publish_file_with(
        &partial,
        &dest,
        rightkit_fs::AtomicWriteOptions {
            replace: rightkit_fs::ReplaceMode::NoReplace,
            ..Default::default()
        },
    ) {
        Ok(()) => {}
        Err(rightkit_fs::FsError::AlreadyExists(_)) if dest.is_file() => {
            let _ = fs::remove_file(&partial);
        }
        Err(e) => {
            return Err(ProjectError::io(
                format!("renaming to {}", dest.display()),
                std::io::Error::other(e),
            )
            .into())
        }
    }
    let manifest = DerivedManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        kind: MANIFEST_KIND.into(),
        transform: ManifestTransform {
            kind: "cfr".into(),
            fps,
            reasons: deep.vfr_reasons.iter().map(|r| r.to_string()).collect(),
            rotation_applied: deep.rotation,
            hdr: deep.hdr,
            video_codec: encode.codec.into(),
            pix_fmt: pix_fmt.into(),
            audio_codec: deep.has_audio.then(|| "pcm_s16le".to_string()),
            ffmpeg_args,
        },
        original,
        derived: ManifestDerived {
            blake3: derived_id,
            path: rel,
            size,
        },
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
    bytes.push(b'\n');
    atomic_write(
        &project
            .dir
            .join(manifest_rel(&manifest.original.blake3, key)),
        &bytes,
    )?;
    Ok(manifest)
}
