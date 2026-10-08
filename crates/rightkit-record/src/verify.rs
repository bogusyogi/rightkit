//! `bundle.verify`: checks a sealed bundle against its own manifest. Nothing
//! here trusts a file because it is present. Every listed file is re-hashed, every
//! event is re-validated, and every redacted field must be stored in the form the
//! policy names.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rightkit_contracts::types::record_session::BundleStatus;
use rightkit_contracts::{validate, Contract};
use serde_json::Value;

use crate::bundle::{
    read_json, CAPTURE_FILE, CLOCK_FILE, EVENTS_FILE, FRAMES_FILE, MANIFEST_FILE, SCENARIO_FILE,
    STATE_FILE, TRACKS_DIR,
};
use crate::error::{Code, RecordError, Result};
use crate::hash::{file_hash, is_blake3_text, HMAC_PREFIX, PREFIX};
use crate::redact::{FieldMode, RedactionPolicy, FIELDS, KEYED_SALT_PREFIX, LEGACY_SALT_PREFIX};

#[derive(Debug, Clone, Default)]
pub struct VerifyOptions {
    /// Plaintext values that must not appear in any persisted text file.
    pub forbidden_plaintext: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub dir: PathBuf,
    pub status: Option<BundleStatus>,
    pub problems: Vec<String>,
    /// Findings that do not fail the bundle. A bundle written before keyed
    /// hashing is readable but weak: its digests are label-salted, so a guess can
    /// be confirmed by anyone.
    pub warnings: Vec<String>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

const TEXT_FILES: [&str; 5] = [
    SCENARIO_FILE,
    EVENTS_FILE,
    CLOCK_FILE,
    CAPTURE_FILE,
    MANIFEST_FILE,
];

pub fn bundle_verify(dir: &Path, options: &VerifyOptions) -> Result<VerifyReport> {
    if !dir.is_dir() {
        return Err(RecordError::new(
            Code::NotFound,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let mut report = VerifyReport {
        dir: dir.to_path_buf(),
        status: None,
        problems: Vec::new(),
        warnings: Vec::new(),
    };
    let manifest = match read_json(&dir.join(MANIFEST_FILE)) {
        Ok(manifest) => manifest,
        Err(error) => {
            report
                .problems
                .push(format!("manifest unreadable: {error}"));
            return Ok(report);
        }
    };
    if let Err(error) = validate(Contract::RecordSession, &manifest) {
        report
            .problems
            .push(format!("manifest does not validate: {error}"));
    }
    report.status = manifest
        .get("status")
        .and_then(|s| serde_json::from_value(s.clone()).ok());

    let files = manifest
        .get("files")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tracks = manifest
        .get("tracks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Every listed file must match its hash and size.
    let mut listed: BTreeSet<String> = BTreeSet::new();
    for (name, entry) in &files {
        listed.insert(name.clone());
        check_entry(dir, name, entry, &mut report.problems);
    }
    for track in &tracks {
        if let Some(file) = track.get("file").and_then(Value::as_str) {
            listed.insert(file.to_string());
            check_entry(dir, file, track, &mut report.problems);
        }
    }

    // Nothing unlisted may sit in the bundle: no working files, no strays.
    check_unlisted(dir, &listed, &mut report.problems);

    // Immutable after stop: every file and directory is read-only.
    check_read_only(dir, &listed, &mut report.problems);

    if matches!(report.status, Some(BundleStatus::Complete)) {
        let frames = manifest
            .pointer("/frames/count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if frames == 0 {
            report.problems.push("complete bundle has no frames".into());
        }
    }

    let scenario = match read_json(&dir.join(SCENARIO_FILE)) {
        Ok(value) => Some(value),
        Err(error) => {
            report
                .problems
                .push(format!("scenario.json unreadable: {error}"));
            None
        }
    };
    let policy = scenario
        .as_ref()
        .and_then(|s| s.get("redaction"))
        .and_then(RedactionPolicy::from_json);
    if scenario.is_some() && policy.is_none() {
        report
            .problems
            .push("scenario.json carries no valid redaction policy".into());
    }
    if let Some(scenario) = &scenario {
        check_scenario(&manifest, scenario, &files, &mut report.problems);
    }

    let key_id = scenario
        .as_ref()
        .and_then(|s| s.pointer("/redaction/keyId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match read_lines_strict(&dir.join(EVENTS_FILE)) {
        Ok(events) => {
            if let Some(policy) = &policy {
                let hashes = check_events(&manifest, &events, policy, &mut report.problems);
                check_hashes(policy, &hashes, key_id.as_deref(), &mut report);
            }
        }
        Err(problem) => report.problems.push(problem),
    }

    check_clock_and_capture(dir, &manifest, &mut report.problems);

    for name in TEXT_FILES {
        let Ok(text) = fs::read_to_string(dir.join(name)) else {
            continue;
        };
        for secret in options.forbidden_plaintext.iter().filter(|s| !s.is_empty()) {
            if text.contains(secret.as_str()) {
                report
                    .problems
                    .push(format!("forbidden plaintext appears in {name}"));
            }
        }
    }

    Ok(report)
}

fn check_entry(dir: &Path, name: &str, entry: &Value, problems: &mut Vec<String>) {
    let path = dir.join(name);
    let expected_hash = entry.get("blake3").and_then(Value::as_str).unwrap_or("");
    let expected_bytes = entry.get("bytes").and_then(Value::as_u64);
    match file_hash(&path) {
        Ok((hash, bytes)) => {
            if hash != expected_hash {
                problems.push(format!("{name}: hash does not match the manifest"));
            }
            if Some(bytes) != expected_bytes {
                problems.push(format!("{name}: size does not match the manifest"));
            }
        }
        Err(error) => problems.push(format!("{name}: unreadable: {error}")),
    }
}

fn check_unlisted(dir: &Path, listed: &BTreeSet<String>, problems: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            problems.push(format!("bundle unreadable: {error}"));
            return;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == MANIFEST_FILE {
            continue;
        }
        if name == TRACKS_DIR && entry.path().is_dir() {
            let Ok(track_entries) = fs::read_dir(entry.path()) else {
                problems.push("tracks directory unreadable".into());
                continue;
            };
            for track in track_entries.flatten() {
                let rel = format!("{TRACKS_DIR}/{}", track.file_name().to_string_lossy());
                if !listed.contains(&rel) {
                    problems.push(format!("{rel}: unlisted file"));
                }
            }
            continue;
        }
        if !listed.contains(&name) {
            problems.push(format!(
                "{name}: unlisted file (working state left in a sealed bundle)"
            ));
        }
    }
    // Working files are never allowed in a sealed bundle.
    for working in [STATE_FILE, FRAMES_FILE] {
        if dir.join(working).exists() {
            problems.push(format!("{working} present in a sealed bundle"));
        }
    }
}

fn check_read_only(dir: &Path, listed: &BTreeSet<String>, problems: &mut Vec<String>) {
    let mut paths: Vec<PathBuf> = listed.iter().map(|name| dir.join(name)).collect();
    paths.push(dir.join(MANIFEST_FILE));
    for path in paths {
        if let Ok(meta) = fs::metadata(&path) {
            if !meta.permissions().readonly() {
                problems.push(format!("{}: writable after stop", path.display()));
            }
        }
    }
    if let Ok(meta) = fs::metadata(dir) {
        if !meta.permissions().readonly() {
            problems.push("bundle directory is writable after stop".into());
        }
    }
}

fn check_scenario(
    manifest: &Value,
    scenario: &Value,
    files: &serde_json::Map<String, Value>,
    problems: &mut Vec<String>,
) {
    if manifest.pointer("/scenario/scenarioId") != scenario.get("scenarioId") {
        problems.push("manifest scenarioId does not match scenario.json".into());
    }
    let steps = scenario
        .get("steps")
        .and_then(Value::as_array)
        .map_or(0, Vec::len) as u64;
    if manifest.pointer("/scenario/steps").and_then(Value::as_u64) != Some(steps) {
        problems.push("manifest step count does not match scenario.json".into());
    }
    let scenario_hash = files
        .get(SCENARIO_FILE)
        .and_then(|e| e.get("blake3"))
        .and_then(Value::as_str);
    if manifest.pointer("/scenario/blake3").and_then(Value::as_str) != scenario_hash {
        problems.push("manifest scenario hash does not match scenario.json".into());
    }
}

fn read_lines_strict(path: &Path) -> std::result::Result<Vec<Value>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("events.ndjson unreadable: {e}"))?;
    let mut events = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|e| format!("events.ndjson line {}: {e}", line_no + 1))?;
        events.push(value);
    }
    Ok(events)
}

/// The `(event, saltId, digest)` the stored hash fields carry, in event order.
type SaltIds = Vec<(usize, String, String)>;

/// Hashes must be keyed by a per-installation key whose id the scenario records.
/// A legacy label-salted hash is accepted and reported as weak. Plaintext is
/// refused by `check_events`, and a `hash` field never carries a `value`.
fn check_hashes(
    policy: &RedactionPolicy,
    salts: &SaltIds,
    key_id: Option<&str>,
    report: &mut VerifyReport,
) {
    let mut legacy = 0usize;
    for (position, salt, digest) in salts {
        let algorithm_prefix = if salt.starts_with(KEYED_SALT_PREFIX) {
            Some(HMAC_PREFIX)
        } else if salt.starts_with(LEGACY_SALT_PREFIX) {
            Some(PREFIX)
        } else {
            None
        };
        if let Some(expected) = algorithm_prefix {
            if !digest.starts_with(expected) {
                report.problems.push(format!(
                    "event {position}: hash digest prefix does not match its saltId algorithm \
                     (saltId '{salt}' requires '{expected}')"
                ));
            }
        }
        if let Some(id) = salt.strip_prefix(KEYED_SALT_PREFIX) {
            match key_id {
                None => report.problems.push(format!(
                    "event {position}: keyed hash but scenario.json records no key id"
                )),
                Some(recorded) if recorded != id => report.problems.push(format!(
                    "event {position}: hash key id {id} is not the recorded key id {recorded}"
                )),
                Some(_) => {}
            }
        } else if salt.starts_with(LEGACY_SALT_PREFIX) {
            legacy += 1;
        } else {
            report
                .problems
                .push(format!("event {position}: unknown hash salt '{salt}'"));
        }
    }
    if legacy > 0 {
        report.warnings.push(format!(
            "weak redaction: {legacy} hashed field(s) are label-salted (written before keyed hashing); \
             anyone can confirm a guessed value"
        ));
    }
    if policy.needs_key() && key_id.is_none() && salts.is_empty() {
        report.warnings.push(
            "weak redaction: the policy hashes fields but scenario.json records no key id".into(),
        );
    }
}

fn check_events(
    manifest: &Value,
    events: &[Value],
    policy: &RedactionPolicy,
    problems: &mut Vec<String>,
) -> SaltIds {
    let mut salts: SaltIds = Vec::new();
    let declared = manifest.pointer("/events/count").and_then(Value::as_u64);
    if declared != Some(events.len() as u64) {
        problems.push("event count does not match the manifest".into());
    }
    let bundle_id = manifest
        .get("bundleId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let first_frame = manifest.pointer("/clock/firstAcceptedFrame").cloned();
    let mut frame_count = 0u64;
    let mut dropped = 0u64;
    let mut last_pts: Option<u64> = None;
    let mut first_ready: Option<Value> = None;

    for (position, event) in events.iter().enumerate() {
        if let Err(error) = validate(Contract::RecordEvent, event) {
            problems.push(format!("event {position} does not validate: {error}"));
            continue;
        }
        if event.get("seq").and_then(Value::as_u64) != Some(position as u64) {
            problems.push(format!("event {position}: seq is not contiguous"));
        }
        if event.get("sessionId").and_then(Value::as_str) != Some(bundle_id) {
            problems.push(format!("event {position}: sessionId is not the bundle id"));
        }
        for field in FIELDS {
            let Some(stored) = field_value(event, field)
                .and_then(|v| v.get("mode"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if stored == "hash" {
                match field_value(event, field)
                    .and_then(|v| v.get("saltId"))
                    .and_then(Value::as_str)
                {
                    Some(salt) => {
                        let digest = field_value(event, field)
                            .and_then(|v| v.get("digest"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        salts.push((position, salt.to_string(), digest.to_string()));
                    }
                    None => problems.push(format!(
                        "event {position}: hash field {field} has no saltId"
                    )),
                }
            }
            match (policy.mode(field), stored) {
                (FieldMode::Omit, "full" | "hash") => problems.push(format!(
                    "event {position}: omit field {field} has a destination ({stored})"
                )),
                (FieldMode::Hash, "full") => problems.push(format!(
                    "event {position}: hash field {field} is stored in plaintext"
                )),
                _ => {}
            }
        }
        match event.get("kind").and_then(Value::as_str) {
            Some("frame_ready") => {
                frame_count += 1;
                let pts = event.pointer("/frame/samplePtsNs").and_then(Value::as_u64);
                match pts {
                    Some(pts) if last_pts.is_some_and(|last| pts <= last) => {
                        problems.push(format!("event {position}: frame PTS did not increase"));
                    }
                    Some(pts) => last_pts = Some(pts),
                    None => problems.push(format!("event {position}: frame event lacks PTS")),
                }
                if first_ready.is_none() {
                    first_ready = Some(event.clone());
                }
            }
            Some("frame_dropped") => {
                dropped += event
                    .pointer("/frameDropped/count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            _ => {}
        }
    }

    if manifest.pointer("/frames/count").and_then(Value::as_u64) != Some(frame_count) {
        problems.push("frame count does not match the frame events".into());
    }
    if manifest
        .pointer("/frames/droppedCount")
        .and_then(Value::as_u64)
        != Some(dropped)
    {
        problems.push("dropped count does not match the dropped-frame events".into());
    }
    if let (Some(first), Some(ready)) = (first_frame.as_ref(), first_ready.as_ref()) {
        let index = ready.pointer("/frame/index").cloned();
        let pts = ready.pointer("/frame/samplePtsNs").cloned();
        if first.get("frameIndex").cloned() != index || first.get("samplePtsNs").cloned() != pts {
            problems.push("first frame event does not match the readiness marker".into());
        }
    }
    salts
}

fn field_value<'a>(event: &'a Value, field: &str) -> Option<&'a Value> {
    match field {
        "target.identity" => event.pointer("/target/identity"),
        "target.label" => event.pointer("/target/label"),
        "text" => event.get("text"),
        _ => None,
    }
}

fn check_clock_and_capture(dir: &Path, manifest: &Value, problems: &mut Vec<String>) {
    match read_json(&dir.join(CLOCK_FILE)) {
        Ok(mut clock) => {
            let frames = clock
                .get("frames")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(object) = clock.as_object_mut() {
                object.remove("frames");
            }
            match manifest.get("clock") {
                Some(declared) if *declared == clock => {}
                Some(_) => problems.push("clock.json does not match the manifest clock".into()),
                None => {}
            }
            if manifest.pointer("/frames/count").and_then(Value::as_u64)
                != Some(frames.len() as u64)
            {
                problems.push("clock.json frame table does not match the frame count".into());
            }
            let mut last = None;
            for frame in &frames {
                let index = frame.get("index").and_then(Value::as_u64);
                let raw = frame.get("rawBlake3").and_then(Value::as_str).unwrap_or("");
                if !is_blake3_text(raw) {
                    problems.push("clock.json frame table holds a malformed raw hash".into());
                    break;
                }
                if index.is_none() || last.is_some_and(|l| index <= Some(l)) {
                    problems.push("clock.json frame indices do not increase".into());
                    break;
                }
                last = index;
            }
        }
        Err(error) => problems.push(format!("clock.json unreadable: {error}")),
    }
    match read_json(&dir.join(CAPTURE_FILE)) {
        Ok(capture) => {
            if manifest.get("capture") != Some(&capture) {
                problems.push("capture.json does not match the manifest capture".into());
            }
        }
        Err(error) => problems.push(format!("capture.json unreadable: {error}")),
    }
}
