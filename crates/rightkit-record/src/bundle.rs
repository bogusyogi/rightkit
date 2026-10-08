//! The session bundle: the working files of a live take, the appends that
//! grow them, and the finalizer that seals a take into an immutable, hashed
//! bundle. Stale takes are recovered through the same finalizer.
//!
//! The core owns every file. The manifest (hashes.json) is the record-session
//! document. It is written only after it validates, and a complete status is
//! only written when it validates.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use rightkit_contracts::types::record_session::{BundleStatus, TrackKind};
use rightkit_contracts::{validate, Contract};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::backend::{track_file, track_id, TrackOutput};
use crate::hash::file_hash;
use crate::media::{probe_mp4, RecoveredMedia};

pub const SCENARIO_FILE: &str = "scenario.json";
pub const EVENTS_FILE: &str = "events.ndjson";
pub const CLOCK_FILE: &str = "clock.json";
pub const CAPTURE_FILE: &str = "capture.json";
pub const MEDIA_FILE: &str = "clean.mp4";
pub const MANIFEST_FILE: &str = "hashes.json";
pub const STATE_FILE: &str = "state.json";
pub const FRAMES_FILE: &str = "frames.ndjson";
pub const TRACKS_DIR: &str = "tracks";

const MAX_REASON_BYTES: usize = 400;

/// Lifecycle phase. The first three are live; the last three are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Recording,
    Stopping,
    Complete,
    Quarantined,
    Invalid,
}

impl Phase {
    pub fn is_active(self) -> bool {
        matches!(self, Phase::Starting | Phase::Recording | Phase::Stopping)
    }

    pub fn from_status(status: BundleStatus) -> Phase {
        match status {
            BundleStatus::Complete => Phase::Complete,
            BundleStatus::PartialQuarantined => Phase::Quarantined,
            BundleStatus::Invalid => Phase::Invalid,
        }
    }
}

/// state.json: everything recovery needs to finish a take that died mid-way.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub phase: Phase,
    pub bundle_id: String,
    pub created_at: String,
    pub heartbeat_ms: u64,
    pub recorder: Value,
    pub target: Value,
    pub dependencies: Value,
    pub pixel_masking: Value,
    pub geometry_epochs: Vec<Value>,
    #[serde(default)]
    pub clock: Option<Value>,
}

/// Appends to a live take. Each append is fsynced before it returns.
pub struct Writer {
    dir: PathBuf,
    events: File,
    frames: File,
    pub state: State,
    pub events_written: u64,
}

impl Writer {
    pub fn create(dir: &Path, state: State, scenario: &Value) -> io::Result<Writer> {
        write_json(&dir.join(SCENARIO_FILE), scenario)?;
        let events = open_new(&dir.join(EVENTS_FILE))?;
        let frames = open_new(&dir.join(FRAMES_FILE))?;
        let mut writer = Writer {
            dir: dir.to_path_buf(),
            events,
            frames,
            state,
            events_written: 0,
        };
        writer.save_state()?;
        Ok(writer)
    }

    pub fn save_state(&mut self) -> io::Result<()> {
        let value = serde_json::to_value(&self.state).map_err(to_io)?;
        write_json(&self.dir.join(STATE_FILE), &value)
    }

    pub fn write_capture(&self, capture: &Value) -> io::Result<()> {
        write_json(&self.dir.join(CAPTURE_FILE), capture)
    }

    pub fn append_event(&mut self, line: &Value) -> io::Result<()> {
        append_line(&mut self.events, line)?;
        self.events_written += 1;
        Ok(())
    }

    pub fn append_frame(&mut self, line: &Value) -> io::Result<()> {
        append_line(&mut self.frames, line)
    }
}

fn open_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().create_new(true).append(true).open(path)
}

fn append_line(file: &mut File, line: &Value) -> io::Result<()> {
    let mut text = serde_json::to_string(line).map_err(to_io)?;
    text.push('\n');
    file.write_all(text.as_bytes())?;
    file.sync_data()
}

pub(crate) fn to_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// Writes JSON through a temporary file and a rename, so a crash never leaves
/// a half-written document under the final name.
pub(crate) fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(to_io)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut file = File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

pub(crate) fn read_json(path: &Path) -> io::Result<Value> {
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(to_io)
}

/// Reads NDJSON, stopping at the first line that does not parse. The flag is
/// true when that torn tail was dropped. A crash mid-append leaves one, and
/// nothing after it is trusted.
pub(crate) fn read_lines(path: &Path) -> io::Result<(Vec<Value>, bool)> {
    let reader = BufReader::new(File::open(path)?);
    let mut lines = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(value) => lines.push(value),
            Err(_) => return Ok((lines, true)),
        }
    }
    Ok((lines, false))
}

fn rewrite_lines(path: &Path, lines: &[Value]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut file = File::create(&tmp)?;
        for line in lines {
            let mut text = serde_json::to_string(line).map_err(to_io)?;
            text.push('\n');
            file.write_all(text.as_bytes())?;
        }
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

pub struct Finalized {
    pub status: BundleStatus,
    pub reason: Option<String>,
    /// The first schema problem in the final manifest, if any.
    pub manifest_problem: Option<String>,
    /// What a non-complete take's media still plays, when it plays.
    pub media: Option<RecoveredMedia>,
}

/// Seals a take. Writes clock.json (the clock map and frame table), then the
/// manifest, removes the working files and makes the bundle read-only.
///
/// A `complete` request is downgraded to `invalid` when the manifest does not
/// validate, so a complete status is never written over an unvalidated bundle.
pub fn finalize(
    dir: &Path,
    state: &State,
    status: BundleStatus,
    reason: Option<String>,
    tracks: &[TrackOutput],
) -> io::Result<Finalized> {
    let (frames, frames_torn) = read_lines(&dir.join(FRAMES_FILE))?;
    let (events, events_torn) = read_lines(&dir.join(EVENTS_FILE))?;
    if frames_torn {
        rewrite_lines(&dir.join(FRAMES_FILE), &frames)?;
    }
    if events_torn {
        rewrite_lines(&dir.join(EVENTS_FILE), &events)?;
    }
    let dropped: u64 = events
        .iter()
        .filter_map(|event| event.pointer("/frameDropped/count").and_then(Value::as_u64))
        .sum();

    let mut clock_doc = state.clock.clone().unwrap_or_else(|| json!({}));
    clock_doc["frames"] = Value::Array(frames.clone());
    write_json(&dir.join(CLOCK_FILE), &clock_doc)?;
    if !dir.join(CAPTURE_FILE).exists() {
        write_json(&dir.join(CAPTURE_FILE), &json!({}))?;
    }

    let scenario = read_json(&dir.join(SCENARIO_FILE))?;
    let capture = read_json(&dir.join(CAPTURE_FILE))?;

    let mut files = Map::new();
    for name in [
        SCENARIO_FILE,
        EVENTS_FILE,
        CLOCK_FILE,
        CAPTURE_FILE,
        MEDIA_FILE,
    ] {
        let path = dir.join(name);
        if path.exists() {
            files.insert(name.to_string(), entry(&path)?);
        }
    }
    let mut track_values = Vec::new();
    for track in tracks {
        let path = dir.join(track_file(track.kind));
        if path.exists() {
            track_values.push(track_json(&path, track)?);
        }
    }

    let parts = Parts {
        state,
        scenario: &scenario,
        capture: &capture,
        files: &files,
        tracks: &track_values,
        frames: frames.len() as u64,
        dropped,
        events: events.len() as u64,
    };
    let mut status = status;
    let mut reason = non_complete_reason(status, reason);

    // A take that did not end cleanly keeps whatever media the helper left. The
    // helper writes a fragmented MP4, so the file plays up to its last whole
    // fragment. Check that, and record how much it holds. A file that does not
    // play makes the take explicitly invalid.
    let mut media = None;
    let mut media_note = None;
    if status != BundleStatus::Complete && dir.join(MEDIA_FILE).exists() {
        match probe_mp4(&dir.join(MEDIA_FILE)) {
            Ok(probe) => {
                let recovered = RecoveredMedia {
                    samples: probe.samples,
                    duration_ms: probe.duration_ms,
                    accepted_frames: frames.len() as u64,
                };
                media_note = Some(format!(
                    "recovered_media frames={}/{} duration_ms={}",
                    recovered.samples, recovered.accepted_frames, recovered.duration_ms
                ));
                media = Some(recovered);
            }
            Err(problem) => {
                status = BundleStatus::Invalid;
                let note = format!("media_unplayable: {problem}");
                reason = Some(append_note(reason.as_deref(), &note));
            }
        }
    }
    let manifest_reason = match (&reason, &media_note) {
        (Some(base), Some(note)) => Some(append_note(Some(base), note)),
        _ => reason.clone(),
    };
    let mut manifest = manifest(&parts, status, manifest_reason.as_deref());
    let mut problem = check(&manifest);
    if status == BundleStatus::Complete && problem.is_some() {
        status = BundleStatus::Invalid;
        reason = Some(truncate(&format!(
            "manifest_invalid: {}",
            problem.clone().unwrap_or_default()
        )));
        manifest = manifest_for(&parts, status, reason.as_deref());
        problem = check(&manifest);
        media = None;
    }
    write_json(&dir.join(MANIFEST_FILE), &manifest)?;
    remove_if_exists(&dir.join(STATE_FILE))?;
    remove_if_exists(&dir.join(FRAMES_FILE))?;
    seal(dir)?;
    Ok(Finalized {
        status,
        reason: if status == BundleStatus::Complete {
            None
        } else {
            reason
        },
        manifest_problem: problem,
        media,
    })
}

/// `base; note`, within the reason budget. The note is kept whole; the base gives way.
fn append_note(base: Option<&str>, note: &str) -> String {
    let room = MAX_REASON_BYTES.saturating_sub(note.len() + 2);
    match base {
        Some(base) => format!("{}; {note}", truncate_to(base, room)),
        None => truncate(note),
    }
}

fn truncate_to(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn manifest_for(parts: &Parts, status: BundleStatus, reason: Option<&str>) -> Value {
    manifest(parts, status, reason)
}

fn non_complete_reason(status: BundleStatus, reason: Option<String>) -> Option<String> {
    if status == BundleStatus::Complete {
        return None;
    }
    Some(truncate(&reason.unwrap_or_else(|| "unspecified".into())))
}

fn truncate(text: &str) -> String {
    if text.len() <= MAX_REASON_BYTES {
        return text.to_string();
    }
    let mut end = MAX_REASON_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn check(manifest: &Value) -> Option<String> {
    validate(Contract::RecordSession, manifest)
        .err()
        .map(|e| truncate(&e.to_string()))
}

struct Parts<'a> {
    state: &'a State,
    scenario: &'a Value,
    capture: &'a Value,
    files: &'a Map<String, Value>,
    tracks: &'a [Value],
    frames: u64,
    dropped: u64,
    events: u64,
}

fn manifest(parts: &Parts, status: BundleStatus, reason: Option<&str>) -> Value {
    let scenario_entry = parts
        .files
        .get(SCENARIO_FILE)
        .and_then(|e| e.get("blake3"))
        .cloned()
        .unwrap_or(Value::Null);
    let steps = parts
        .scenario
        .get("steps")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut doc = json!({
        "contract": "record-session",
        "contractVersion": version(Contract::RecordSession),
        "bundleId": parts.state.bundle_id,
        "createdAt": parts.state.created_at,
        "status": status,
        "recorder": parts.state.recorder,
        "scenario": {
            "scenarioId": parts.scenario.get("scenarioId").cloned().unwrap_or(Value::Null),
            "blake3": scenario_entry,
            "steps": steps,
        },
        "target": parts.state.target,
        "capture": parts.capture,
        "geometryEpochs": parts.state.geometry_epochs,
        "frames": { "count": parts.frames, "droppedCount": parts.dropped },
        "events": {
            "count": parts.events,
            "contract": "record-event",
            "contractVersion": version(Contract::RecordEvent),
        },
        "files": parts.files,
        "dependencies": parts.state.dependencies,
        "redaction": {
            "policyId": parts.scenario["redaction"]["policyId"],
            "policyVersion": parts.scenario["redaction"]["policyVersion"],
            "pixelMasking": parts.state.pixel_masking,
        },
        "donorDirectoryRequired": false,
    });
    if let Some(clock) = &parts.state.clock {
        doc["clock"] = clock.clone();
    }
    if !parts.tracks.is_empty() {
        doc["tracks"] = Value::Array(parts.tracks.to_vec());
    }
    if let Some(reason) = reason {
        doc["reason"] = json!(reason);
    }
    strip_nulls(&mut doc);
    doc
}

/// Removes `null` members: the contract schema wants absent keys, not nulls.
pub(crate) fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

pub(crate) fn version(contract: Contract) -> String {
    contract.supported_version().to_string()
}

fn entry(path: &Path) -> io::Result<Value> {
    let (blake3, bytes) = file_hash(path)?;
    Ok(json!({ "blake3": blake3, "bytes": bytes }))
}

fn track_json(path: &Path, track: &TrackOutput) -> io::Result<Value> {
    let mut value = entry(path)?;
    value["trackId"] = json!(track_id(track.kind));
    value["kind"] = serde_json::to_value(track.kind).map_err(to_io)?;
    value["file"] = json!(track_file(track.kind));
    value["offsetMs"] = json!(track.offset_ms);
    Ok(value)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Makes every file and directory of the bundle read-only.
fn seal(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            seal(&path)?;
        } else {
            set_read_only(&path)?;
        }
    }
    set_read_only(dir)
}

/// Seals one path on every OS. The per-file read-only attribute is set everywhere,
/// since on Windows it is the only thing that refuses a write to an existing file.
/// On Unix the write bits are also cleared from the mode.
fn set_read_only(path: &Path) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        permissions.set_mode(mode & !0o222);
    }
    fs::set_permissions(path, permissions)
}

pub(crate) fn read_state(dir: &Path) -> Option<State> {
    let value = read_json(&dir.join(STATE_FILE)).ok()?;
    serde_json::from_value(value).ok()
}

/// A take found by the startup sweep and sealed as quarantined or invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub dir: PathBuf,
    pub bundle_id: String,
    pub status: BundleStatus,
    pub reason: Option<String>,
    /// What the stale take's media still plays, when it plays.
    pub media: Option<RecoveredMedia>,
}

/// Seals every live take under `roots` whose heartbeat is older than
/// `stale_after_ms`. Bundles already sealed are skipped. A take that cannot be
/// read is left for the next sweep rather than failing the sweep.
pub fn recover_stale(
    roots: &[PathBuf],
    now_ms: u64,
    stale_after_ms: u64,
) -> io::Result<Vec<Recovered>> {
    let mut recovered = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in fs::read_dir(root)? {
            let dir = entry?.path();
            if !dir.is_dir() || dir.join(MANIFEST_FILE).exists() {
                continue;
            }
            let Some(state) = read_state(&dir) else {
                continue;
            };
            let stale = now_ms.saturating_sub(state.heartbeat_ms) > stale_after_ms;
            if !state.phase.is_active() || !stale {
                continue;
            }
            let (status, reason) = if state.clock.is_some() {
                (BundleStatus::PartialQuarantined, "stale_session_recovered")
            } else {
                (BundleStatus::Invalid, "stale_session_before_ready")
            };
            let tracks = scan_tracks(&dir);
            let Ok(done) = finalize(&dir, &state, status, Some(reason.to_string()), &tracks) else {
                continue;
            };
            recovered.push(Recovered {
                dir,
                bundle_id: state.bundle_id.clone(),
                status: done.status,
                reason: done.reason,
                media: done.media,
            });
        }
    }
    Ok(recovered)
}

/// Track files that exist on disk. Used when the helper's report is lost.
fn scan_tracks(dir: &Path) -> Vec<TrackOutput> {
    [
        TrackKind::AppAudio,
        TrackKind::Microphone,
        TrackKind::WebcamVideo,
    ]
    .into_iter()
    .filter(|kind| dir.join(track_file(*kind)).exists())
    .map(|kind| TrackOutput { kind, offset_ms: 0 })
    .collect()
}
