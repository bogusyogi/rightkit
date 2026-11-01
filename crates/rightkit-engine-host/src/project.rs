//! Project-method support for engines built on [`rightkit_video_project`]: JSON and transcript
//! file helpers, the analysis folder layout, clip occurrence ranges, flat settings files, the project
//! font, the bundled media tools and the renders listing. Product command modules call these
//! instead of each carrying a copy.
//!
//! Requires the `project-cmd` feature.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use rightkit_app_settings::{FileFormat, NullMeans, OnCorrupt, Schema, Store, StoreOptions};
use rightkit_transcript::Transcript;
use rightkit_video_project::{Access, MediaTools, Project, ProjectError, SourceRecord};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::env::Env;

/// Settings schema id kept from the first product that stored these files; existing settings files
/// are written under it.
pub const SETTINGS_SCHEMA_ID: &str = "cutright-settings";

/// Bundled title font, relative to the engine's resource folder.
pub const TITLE_FONT_RELATIVE_PATH: &str = "runtime/fonts/JetBrainsMono-Regular.ttf";

/// The bundled editing tools (ffmpeg, ffprobe) resolved from the engine's resource folder.
pub fn media_tools(env: &Env) -> Result<MediaTools, ProjectError> {
    MediaTools::resolve(env.resource_dir().as_deref())
}

/// Title font from the engine bundle (`test-mode` builds also honour `RIGHTKIT_FONT`).
pub fn title_font(env: &Env) -> Option<PathBuf> {
    #[cfg(feature = "test-mode")]
    if let Some(p) = std::env::var_os("RIGHTKIT_FONT").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    // Only a font that exists: on Windows the resource dir is the exe's folder even for a
    // bare dev binary, and a missing path would fail every render while hashing it. With no
    // font, a render that needs one reports the missing bundle font instead.
    env.resource_dir()
        .map(|d| d.join(TITLE_FONT_RELATIVE_PATH))
        .filter(|p| p.is_file())
}

/// Root of the thumbnail cache under a project directory.
pub fn thumb_cache_root(project: &Path) -> PathBuf {
    project.join(".cache").join("thumbs")
}

pub fn bad(msg: impl Into<String>) -> ProjectError {
    ProjectError::BadInput(msg.into())
}

pub fn io_err(context: String) -> impl FnOnce(std::io::Error) -> ProjectError {
    move |e| ProjectError::io(context.clone(), e)
}

pub fn write_json(path: &Path, v: &impl serde::Serialize) -> Result<(), ProjectError> {
    let mut b = serde_json::to_vec_pretty(v)
        .map_err(|e| bad(format!("serialising {}: {e}", path.display())))?;
    b.push(b'\n');
    rightkit_fs::atomic_write_with(
        path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(&b),
    )
    .map_err(|e| {
        rightkit_video_project::ProjectError::io(format!("writing {}", path.display()), e.into())
    })
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ProjectError> {
    let b = std::fs::read(path).map_err(io_err(format!("reading {}", path.display())))?;
    serde_json::from_slice(&b)
        .map_err(|e| ProjectError::Corrupt(format!("{}: {e}", path.display())))
}

/// Reads a transcript strictly: unknown or missing fields are corruption, since
/// transcripts gate destructive word-edge cuts.
pub fn read_transcript(path: &Path) -> Result<Transcript, ProjectError> {
    let text =
        std::fs::read_to_string(path).map_err(io_err(format!("reading {}", path.display())))?;
    Transcript::from_json_strict(&text)
        .map_err(|e| ProjectError::Corrupt(format!("{}: {e}", path.display())))
}

pub fn analysis_dir(project: &Path) -> PathBuf {
    project.join("analysis")
}

pub fn transcript_path(project: &Path, source_id: &str) -> PathBuf {
    analysis_dir(project).join(format!("transcript-{source_id}.json"))
}

/// Whether a transcript artifact exists for `source_id` (content unchecked).
pub fn transcript_exists(p: &Project, source_id: &str) -> bool {
    transcript_path(&p.dir, source_id).is_file()
}

pub fn envelope_path(project: &Path, source_id: &str) -> PathBuf {
    analysis_dir(project).join(format!("envelope-{source_id}.json"))
}

pub fn timemap_path(project: &Path, source_id: &str) -> PathBuf {
    analysis_dir(project).join(format!("timemap-{source_id}.json"))
}

fn shorts_path(project: &Path, source_id: &str) -> PathBuf {
    analysis_dir(project).join(format!("shorts-{source_id}.json"))
}

/// Resolves `source` (a BLAKE3 source id or exact file name) to its record.
pub fn find_source<'a>(p: &'a Project, source: &str) -> Result<&'a SourceRecord, ProjectError> {
    p.meta
        .sources
        .iter()
        .find(|s| s.id.as_str() == source || s.name == source)
        .ok_or_else(|| bad(format!("no source {source:?} in this project")))
}

pub fn load_transcript(p: &Project, source_id: &str) -> Result<Transcript, ProjectError> {
    let mut transcript = load_transcript_dir(&p.dir, source_id)?;
    if let Some(source) = p
        .timeline()
        .sources
        .iter()
        .find(|s| s.id.as_str() == source_id)
    {
        let cap =
            (source.duration.0 as i128 * 1000 / rightkit_timeline::TICKS_PER_SECOND as i128) as u64;
        transcript
            .constrain_duration(cap)
            .map_err(|e| bad(e.to_string()))?;
    }
    Ok(transcript)
}

pub fn load_transcript_dir(dir: &Path, source_id: &str) -> Result<Transcript, ProjectError> {
    let path = transcript_path(dir, source_id);
    if !path.is_file() {
        return Err(bad(format!(
            "source {source_id} has no transcript; run transcribe first"
        )));
    }
    let t = read_transcript(&path)?;
    t.validate()
        .map_err(|e| ProjectError::Corrupt(format!("{}: {e}", path.display())))?;
    Ok(t)
}

pub fn load_envelope(p: &Project, source_id: &str) -> Option<Vec<f32>> {
    read_json(&envelope_path(&p.dir, source_id)).ok()
}

/// One placed source range on the output timeline: `source_ms ∈ [in_ms, out_ms)` maps to
/// `out_off + (ms - in_ms)`. Built by walking the actual revisioned clips, so trims,
/// reorders, repeated sources and manual edits all map correctly. `clip_id` identifies
/// the clip occurrence (a repeated source has one entry per placement).
pub struct ClipRange {
    pub clip_id: String,
    pub video: bool,
    pub in_ms: u64,
    pub out_ms: u64,
    pub out_off_ms: u64,
}

pub fn ms(t: rightkit_timeline::Ticks) -> u64 {
    t.to_units(1000).max(0) as u64
}

/// Every clip occurrence on the timeline (video placements in output order, then
/// audio clips), keyed by source id.
pub fn clip_ranges(tl: &rightkit_timeline::Timeline) -> BTreeMap<String, Vec<ClipRange>> {
    let mut ranges: BTreeMap<String, Vec<ClipRange>> = BTreeMap::new();
    for track in &tl.video_tracks {
        for (c, place) in track.clips.iter().zip(tl.placements(track)) {
            let (si, so) = (ms(c.source_in), ms(c.source_out));
            if so <= si {
                continue;
            }
            ranges
                .entry(c.source.to_string())
                .or_default()
                .push(ClipRange {
                    clip_id: c.id.clone(),
                    video: true,
                    in_ms: si,
                    out_ms: so,
                    out_off_ms: ms(tl.fps.frame_to_ticks(place.start_frame)),
                });
        }
    }
    for track in &tl.audio_tracks {
        for c in &track.clips {
            let (si, so) = (ms(c.trim_in), ms(c.trim_out));
            if so <= si {
                continue;
            }
            ranges
                .entry(c.source.to_string())
                .or_default()
                .push(ClipRange {
                    clip_id: c.id.clone(),
                    video: false,
                    in_ms: si,
                    out_ms: so,
                    out_off_ms: ms(c.start),
                });
        }
    }
    for v in ranges.values_mut() {
        v.sort_by_key(|r| r.out_off_ms);
    }
    ranges
}

/// Camera dialogue under B-roll. A B-roll placement removes the camera picture it
/// covers, and its programme audio is re-created as microphone clips with no link.
/// Each such clip at output `start` with trim `[trim_in, trim_out)` is camera time
/// `[trim_in - offset, trim_out - offset)` of the attached camera, so its words are
/// read back into camera time here and captioned as ordinary occurrences.
pub fn mic_covered_ranges(tl: &rightkit_timeline::Timeline) -> BTreeMap<String, Vec<ClipRange>> {
    let mut out: BTreeMap<String, Vec<ClipRange>> = BTreeMap::new();
    for track in &tl.audio_tracks {
        for c in &track.clips {
            if c.link.is_some() {
                continue;
            }
            let Some(att) = tl.mic_attachments.iter().find(|a| a.microphone == c.source) else {
                continue;
            };
            let cam_in = c.trim_in.0 - att.offset.0;
            let cam_out = c.trim_out.0 - att.offset.0;
            // Words before camera time zero do not exist; clamp the window and keep
            // the output offset aligned with the camera time it starts at.
            let in_ticks = cam_in.max(0);
            if cam_out <= in_ticks {
                continue;
            }
            let out_off = c.start.0 + (in_ticks - cam_in);
            out.entry(att.camera.to_string())
                .or_default()
                .push(ClipRange {
                    clip_id: c.id.clone(),
                    video: false,
                    in_ms: ms(rightkit_timeline::Ticks(in_ticks)),
                    out_ms: ms(rightkit_timeline::Ticks(cam_out)),
                    out_off_ms: ms(rightkit_timeline::Ticks(out_off)),
                });
        }
    }
    out
}

/// Placements where a source's dialogue is actually audible: video clips of sources
/// that carry audio (the compiler mixes their source audio) plus audio-track clips.
/// Linked A/V pairs (an audio clip following its video clip) resolve to the same
/// occurrence — deduped on the identical (in, out, out_off) window.
pub fn audible_ranges(tl: &rightkit_timeline::Timeline) -> BTreeMap<String, Vec<ClipRange>> {
    let mut ranges = clip_ranges(tl);
    for (camera, covered) in mic_covered_ranges(tl) {
        let rs = ranges.entry(camera).or_default();
        rs.extend(covered);
        rs.sort_by_key(|r| r.out_off_ms);
    }
    for (id, rs) in ranges.iter_mut() {
        let has_audio = tl
            .sources
            .iter()
            .find(|s| s.id.as_str() == id.as_str())
            .map(|s| s.has_audio)
            .unwrap_or(false);
        rs.retain(|r| !r.video || has_audio);
        rs.dedup_by(|a, b| {
            a.in_ms == b.in_ms && a.out_ms == b.out_ms && a.out_off_ms == b.out_off_ms
        });
    }
    ranges.retain(|_, rs| !rs.is_empty());
    ranges
}

pub fn settings_path(env: &Env, dir: Option<&Path>) -> Result<PathBuf, ProjectError> {
    match dir {
        Some(d) => Ok(d.join("settings.json")),
        None => env
            .app_root
            .path("settings.json")
            .map_err(|e| bad(e.to_string())),
    }
}

/// Opens a settings file through rightkit-app-settings in flat mode: the file stays
/// the plain top-level JSON object that older builds read. Every access path
/// (settings.get/set, framing approval, caption style, finishing) goes through here.
/// `writable` opens for patches; otherwise the file is only read and never
/// created, locked or rewritten.
pub fn open_settings(
    path: &Path,
    writable: bool,
) -> Result<Store<Map<String, Value>>, ProjectError> {
    let options = StoreOptions::new(Schema::new(SETTINGS_SCHEMA_ID, 1))
        .format(FileFormat::Flat)
        // A corrupt or non-object file is refused on open, read or write. Nothing on disk
        // is moved, quarantined, or read from `.bak`.
        .on_corrupt(OnCorrupt::Refuse)
        // Missing file reads as {} and is created only by the first real patch.
        .defaults_provider(|| json!({}))
        // Any top-level key may be set, as before.
        .allow_unknown_keys(|_: &str| true)
        // SetNull: a `null` for a key the file does not hold yet is stored as JSON null. A
        // `null` for a key it already holds removes that key (rightkit-app-settings store.rs).
        .null_means(NullMeans::SetNull)
        // Pretty JSON, sorted keys at every level, trailing newline.
        .pretty_sorted_newline(true)
        .validate_values(|values| {
            if let Some(v) = values.get("framing_approval") {
                if !v.is_null() && v != "review" && v != "auto" {
                    return Err("framing_approval must be review or auto".into());
                }
            }
            // Opt-in room-tone bed for the cut gaps. A non-bool is refused here rather than silently
            // read as off by the render paths.
            if let Some(v) = values.get("room_tone") {
                if !v.is_null() && !v.is_boolean() {
                    return Err("room_tone must be true, false or null".into());
                }
            }
            // Font shape only; existence and the BLAKE3 match are checked when a render uses it.
            if let Some(v) = values.get(PROJECT_FONT_KEY) {
                if !v.is_null() && !v.is_string() {
                    return Err("font must be a path string or null".into());
                }
            }
            if let Some(v) = values.get(PROJECT_FONT_HASH_KEY) {
                let hex_ok = match v {
                    Value::Null => true,
                    Value::String(h) => {
                        h.is_empty() || (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    }
                    _ => false,
                };
                if !hex_ok {
                    return Err(
                        "font_blake3 must be a 64-character BLAKE3 hex digest or null".into(),
                    );
                }
            }
            Ok(())
        });
    let store = if writable {
        Store::open_path(path, options)
    } else {
        Store::open_read_only(path, options)
    };
    store.map_err(|e| settings_error(path, e))
}

/// Current settings object of one file. A missing file is `{}`.
pub fn settings_object(path: &Path) -> Result<Map<String, Value>, ProjectError> {
    Ok(open_settings(path, false)?.get())
}

/// Finishing defaults the Settings screen saves app-wide. A project's own value wins;

pub const PROJECT_FONT_KEY: &str = "font";
pub const PROJECT_FONT_HASH_KEY: &str = "font_blake3";

/// The font the renders of `dir` burn captions and titles with.
///
/// No `font` setting (or an empty one) means the bundled title font. A set `font` must be
/// an existing `.ttf`/`.otf` file whose bytes match `font_blake3`; anything else is refused
/// by name, so a render never silently falls back to a different face.
pub fn project_font(env: &Env, dir: &Path) -> Result<Option<PathBuf>, ProjectError> {
    let settings = settings_object(&dir.join("settings.json"))?;
    let raw = match settings.get(PROJECT_FONT_KEY) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(bad("settings font must be a path string")),
    };
    let Some(raw) = raw else {
        return Ok(title_font(env));
    };
    let path = PathBuf::from(&raw);
    let is_font_file = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"));
    if !is_font_file {
        return Err(bad(format!(
            "project font {} must be a .ttf or .otf file",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(bad(format!("project font {} is missing", path.display())));
    }
    let expected = settings
        .get(PROJECT_FONT_HASH_KEY)
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            bad(format!(
                "project font {} has no BLAKE3 registered ({PROJECT_FONT_HASH_KEY})",
                path.display()
            ))
        })?;
    let bytes = std::fs::read(&path)
        .map_err(|e| bad(format!("reading project font {}: {e}", path.display())))?;
    let actual = rightkit_fs::digest::blake3_bytes(&bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(bad(format!(
            "project font {} does not match its registered BLAKE3 ({PROJECT_FONT_HASH_KEY})",
            path.display()
        )));
    }
    Ok(Some(path))
}

pub fn settings_error(path: &Path, e: rightkit_app_settings::Error) -> ProjectError {
    match e {
        rightkit_app_settings::Error::Io(source) => {
            ProjectError::io(format!("reading {}", path.display()), source)
        }
        rightkit_app_settings::Error::Conflict { expected, actual } => {
            ProjectError::RevisionConflict { expected, actual }
        }
        rightkit_app_settings::Error::Corrupt(_) => ProjectError::Corrupt(format!(
            "{}: settings file is not readable as a JSON object",
            path.display()
        )),
        other => bad(format!("{}: {other}", path.display())),
    }
}

pub fn renders_list(dir: &Path) -> Result<Value, ProjectError> {
    let p = Project::open(dir, Access::Read)?;
    let renders = p.dir.join("renders");
    let mut out = Vec::new();
    if renders.is_dir() {
        for entry in std::fs::read_dir(&renders)
            .map_err(io_err("reading renders".into()))?
            .flatten()
        {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".render.json") {
                continue; // surfaced on its mp4 entry as `record`
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "mp4" | "srt" | "vtt" | "fcpxml" | "otio" | "json") {
                let meta = entry.metadata().ok();
                // Render records ride alongside: `renders/<stem>.mp4.render.json`
                // holds the export hash, revision identity and source↔output map
                // (render record schema; see the host product).
                let record = (ext == "mp4").then(|| {
                    let mut rp = path.as_os_str().to_owned();
                    rp.push(".render.json");
                    PathBuf::from(rp)
                });
                let record = record
                    .and_then(|rp| rp.is_file().then(|| read_json::<Value>(&rp).ok()).flatten());
                // Automation's retention.import binds to these flattened fields:
                // export hash, rendered revision (timeline cursor), duration and the
                // source↔output time map — same data as `record`, stable shape.
                let flat = record.as_ref().map(|r| {
                    json!({
                        "hash": r["output_blake3"],
                        "revision": r["origin"]["timeline_cursor"],
                        "duration_ms": r["duration_ms"],
                        "time_map": r["time_map"],
                    })
                });
                out.push(json!({
                    "name": name,
                    "path": path.display().to_string(),
                    "bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    "hash": flat.as_ref().map(|f| f["hash"].clone()),
                    "revision": flat.as_ref().map(|f| f["revision"].clone()),
                    "duration_ms": flat.as_ref().map(|f| f["duration_ms"].clone()),
                    "time_map": flat.as_ref().map(|f| f["time_map"].clone()),
                    "record": record,
                }));
            }
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(json!({"renders": out}))
}
