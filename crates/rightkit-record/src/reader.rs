//! Reads a sealed bundle back for the director and the carousel. It answers
//! these questions: which source frame shows a step's postcondition (or a
//! target's postcondition), when each step ran, what the scenario declared, what
//! happened in order, how host time maps onto the clip's timeline, and which
//! cursor assets were recorded. Every timeline answer is read from the bundle's
//! clock map, never from elapsed process time.
//!
//! The reader never decodes `clean.mp4`. A consumer decodes it through its own
//! typed ffmpeg boundary and selects pictures by each frame's `sample_pts_ns`.
//! See the README, "Decoding clean.mp4".

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

use rightkit_contracts::types::record_event::RecordEvent;
use rightkit_contracts::types::record_session::BundleStatus;
use serde_json::Value;

use crate::bundle::{
    read_json, read_lines, CLOCK_FILE, EVENTS_FILE, MANIFEST_FILE, SCENARIO_FILE,
};
use crate::error::{Code, RecordError, Result};
use crate::plan::{valid_id, ScenarioSpec, ScenarioStep};
use crate::verify::{bundle_verify, VerifyOptions};

/// The cursor manifest, relative to the bundle directory. The recorder does not
/// write it yet; see the README, "Cursor assets".
const CURSOR_MANIFEST: &str = "cursors/manifest.json";
const CURSOR_SCHEMA: &str = "rightkit.record.cursor-assets.v1";

/// A source frame: the raw frame identity and its presentation time. Rendered
/// output (zoom, stage, captions) has a different identity and is never compared
/// with this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFrame {
    pub index: u64,
    pub sample_pts_ns: u64,
    pub host_ns: u64,
    pub raw_blake3: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepSpan {
    pub first_host_ns: u64,
    pub last_host_ns: u64,
}

/// The bundle's clock map: how host time becomes sample PTS, and how sample PTS
/// becomes encoded PTS. Read from `clock.json`, `mapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockMap {
    /// Added to a host time (nanoseconds) to give its sample PTS. Anchored to the
    /// first accepted frame: its `samplePtsNs` minus its `hostNs`.
    pub host_to_sample_pts_offset_ns: i64,
    /// Added to a sample PTS to give its encoded PTS. The recorder writes 0 today
    /// (`mapping.samplePtsToEncodedPtsOffsetNs`), so encoded PTS equals sample PTS.
    /// A bundle without the field reads as 0. Consumers call
    /// `sample_to_encoded_pts` rather than assuming equality, so a later encoder
    /// that shifts the stream needs no consumer change.
    pub sample_to_encoded_pts_offset_ns: i64,
}

impl ClockMap {
    /// The sample PTS of a host time, clamped at zero.
    pub fn host_to_sample_pts(&self, host_ns: u64) -> u64 {
        shift(host_ns, self.host_to_sample_pts_offset_ns)
    }

    /// The encoded PTS of a sample PTS, clamped at zero.
    pub fn sample_to_encoded_pts(&self, sample_pts_ns: u64) -> u64 {
        shift(sample_pts_ns, self.sample_to_encoded_pts_offset_ns)
    }
}

/// Adds a signed offset to a nanosecond timestamp, clamped to zero below and to
/// `i64::MAX` above the input range.
fn shift(ns: u64, offset: i64) -> u64 {
    let base = i64::try_from(ns).unwrap_or(i64::MAX);
    // The `max` makes the value non-negative, so the cast is lossless.
    base.saturating_add(offset).max(0) as u64
}

/// Whether a bundle carries cursor assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAssetsStatus {
    /// `cursors/manifest.json` exists. Its assets are listed by `cursor_assets`.
    Recorded,
    /// The bundle has no cursor manifest. `cursor_assets` returns an empty list.
    NotRecorded,
}

/// One cursor image a bundle recorded, with its hotspot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAsset {
    pub id: String,
    /// Relative to the bundle directory, always under `cursors/`.
    pub path: PathBuf,
    /// The hotspot in the asset's own pixels, from its top-left corner.
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedPostcondition {
    description: String,
    /// `target.targetId` of the postcondition event, when the caller set one.
    target_id: Option<String>,
    observed_host_ns: u64,
}

impl std::fmt::Debug for BundleReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BundleReader").finish_non_exhaustive()
    }
}

pub struct BundleReader {
    dir: PathBuf,
    status: Option<BundleStatus>,
    clock: ClockMap,
    frames: Vec<SourceFrame>,
    postconditions: BTreeMap<String, Vec<ObservedPostcondition>>,
    steps: BTreeMap<String, StepSpan>,
}

impl BundleReader {
    /// Opens a bundle without re-hashing it. A quarantined, partial or invalid
    /// bundle opens too, so the status is reported by `status()`. A consumer that
    /// must not import an incomplete bundle calls [`BundleReader::open_complete`].
    pub fn open(dir: &Path) -> Result<BundleReader> {
        let manifest = read_json(&dir.join(MANIFEST_FILE))
            .map_err(|e| RecordError::new(Code::NotBundle, format!("manifest unreadable: {e}")))?;
        let status = manifest
            .get("status")
            .and_then(|s| serde_json::from_value(s.clone()).ok());

        let clock_doc = read_json(&dir.join(CLOCK_FILE)).map_err(|e| {
            RecordError::new(Code::NotBundle, format!("clock.json unreadable: {e}"))
        })?;
        let offset_ns = clock_doc
            .pointer("/mapping/hostToSamplePtsOffsetNs")
            .and_then(Value::as_i64)
            .ok_or_else(|| RecordError::new(Code::NotBundle, "clock.json has no clock map"))?;
        let encoded_offset_ns = match clock_doc.pointer("/mapping/samplePtsToEncodedPtsOffsetNs") {
            None => 0,
            Some(value) => value.as_i64().ok_or_else(|| {
                RecordError::new(
                    Code::NotBundle,
                    "clock.json encoded-PTS offset is not an integer",
                )
            })?,
        };
        let mut frames = Vec::new();
        for frame in clock_doc
            .get("frames")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let field = |name: &str| frame.get(name).and_then(Value::as_u64);
            let (Some(index), Some(host_ns), Some(sample_pts_ns), Some(raw)) = (
                field("index"),
                field("hostNs"),
                field("samplePtsNs"),
                frame.get("rawBlake3").and_then(Value::as_str),
            ) else {
                return Err(RecordError::new(
                    Code::NotBundle,
                    "clock.json frame table is malformed",
                ));
            };
            frames.push(SourceFrame {
                index,
                sample_pts_ns,
                host_ns,
                raw_blake3: raw.to_string(),
            });
        }
        if frames
            .windows(2)
            .any(|pair| pair[1].sample_pts_ns <= pair[0].sample_pts_ns)
        {
            return Err(RecordError::new(
                Code::NotBundle,
                "frame table PTS does not increase",
            ));
        }

        let (events, _) = read_lines(&dir.join(EVENTS_FILE)).map_err(|e| {
            RecordError::new(Code::NotBundle, format!("events.ndjson unreadable: {e}"))
        })?;
        let mut postconditions: BTreeMap<String, Vec<ObservedPostcondition>> = BTreeMap::new();
        let mut steps: BTreeMap<String, StepSpan> = BTreeMap::new();
        for event in &events {
            let Some(step_id) = event.get("stepId").and_then(Value::as_str) else {
                continue;
            };
            let Some(host_ns) = event.get("hostNs").and_then(Value::as_u64) else {
                continue;
            };
            steps
                .entry(step_id.to_string())
                .and_modify(|span| {
                    span.first_host_ns = span.first_host_ns.min(host_ns);
                    span.last_host_ns = span.last_host_ns.max(host_ns);
                })
                .or_insert(StepSpan {
                    first_host_ns: host_ns,
                    last_host_ns: host_ns,
                });
            let observed = event.get("postcondition");
            let is_observed = observed
                .and_then(|p| p.get("observed"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if event.get("kind").and_then(Value::as_str) == Some("postcondition") && is_observed {
                let description = observed
                    .and_then(|p| p.get("description"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let observed_host_ns = observed
                    .and_then(|p| p.get("observedNs"))
                    .and_then(Value::as_u64)
                    .unwrap_or(host_ns);
                // Bundles written before target ids were recorded have no
                // `target.targetId`; they read with `None` and match no target.
                let target_id = event
                    .pointer("/target/targetId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                postconditions.entry(step_id.to_string()).or_default().push(
                    ObservedPostcondition {
                        description,
                        target_id,
                        observed_host_ns,
                    },
                );
            }
        }

        Ok(BundleReader {
            dir: dir.to_path_buf(),
            status,
            clock: ClockMap {
                host_to_sample_pts_offset_ns: offset_ns,
                sample_to_encoded_pts_offset_ns: encoded_offset_ns,
            },
            frames,
            postconditions,
            steps,
        })
    }

    /// Opens a bundle only when it is sealed `complete` and passes `bundle_verify`.
    /// A `partial_quarantined` or `invalid` bundle is refused with
    /// `Code::NotComplete`, and the message names the status and the manifest
    /// reason. A bundle that fails verification is refused with
    /// `Code::VerifyFailed`, and the message names the first problems. Warnings do
    /// not refuse a bundle.
    pub fn open_complete(dir: &Path, options: &VerifyOptions) -> Result<BundleReader> {
        let report = bundle_verify(dir, options)?;
        match report.status {
            Some(BundleStatus::Complete) => {}
            Some(status) => {
                let name = match status {
                    BundleStatus::Complete => "complete",
                    BundleStatus::PartialQuarantined => "partial_quarantined",
                    BundleStatus::Invalid => "invalid",
                };
                let reason = manifest_reason(dir)
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default();
                return Err(RecordError::new(
                    Code::NotComplete,
                    format!("bundle status is {name}{reason}; only a complete bundle opens here"),
                ));
            }
            None => {
                return Err(RecordError::new(
                    Code::NotComplete,
                    format!(
                        "bundle has no readable status: {}",
                        report.problems.join("; ")
                    ),
                ));
            }
        }
        if !report.ok() {
            return Err(RecordError::new(
                Code::VerifyFailed,
                format!(
                    "bundle fails verification: {}",
                    report.problems.join("; ")
                ),
            ));
        }
        BundleReader::open(dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn status(&self) -> Option<BundleStatus> {
        self.status
    }

    pub fn frames(&self) -> &[SourceFrame] {
        &self.frames
    }

    /// First and last host time of each step's events.
    pub fn step_index(&self) -> &BTreeMap<String, StepSpan> {
        &self.steps
    }

    /// The clock map: host time to sample PTS, and sample PTS to encoded PTS.
    pub fn clock_map(&self) -> ClockMap {
        self.clock
    }

    /// The scenario the take was recorded under: its id, and each step's id,
    /// label and optional `say`, in declaration order. Reads `scenario.json`.
    pub fn scenario(&self) -> Result<ScenarioSpec> {
        let malformed = |detail: &str| {
            RecordError::new(Code::NotBundle, format!("scenario.json {detail}"))
        };
        let value = read_json(&self.dir.join(SCENARIO_FILE))
            .map_err(|e| malformed(&format!("unreadable: {e}")))?;
        let scenario_id = value
            .get("scenarioId")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("has no scenarioId"))?;
        let steps = value
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("has no steps"))?;
        let steps = steps
            .iter()
            .enumerate()
            .map(|(position, step)| {
                let text = |name: &str| step.get(name).and_then(Value::as_str);
                let in_order = step.get("index").and_then(Value::as_u64) == Some(position as u64);
                let (Some(step_id), Some(label), true) = (text("stepId"), text("label"), in_order)
                else {
                    return Err(malformed(&format!("step {position} is malformed")));
                };
                let say = match step.get("say") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(say)) => Some(say.clone()),
                    Some(_) => return Err(malformed(&format!("step {position} say is not text"))),
                };
                Ok(ScenarioStep {
                    step_id: step_id.to_string(),
                    label: label.to_string(),
                    say,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ScenarioSpec {
            scenario_id: scenario_id.to_string(),
            steps,
        })
    }

    /// Streams `events.ndjson` one event per line, in file order, without reading
    /// the whole file. A line that does not parse as a `record-event` yields an
    /// error naming its line number, and the stream ends there: nothing past a bad
    /// line is trusted. An unreadable file yields one error.
    pub fn events(&self) -> impl Iterator<Item = Result<RecordEvent>> {
        EventStream::open(&self.dir.join(EVENTS_FILE))
    }

    /// The source frame that first shows the state a step's postcondition
    /// observed. The postcondition's host time is mapped onto the sample PTS
    /// timeline, then the first frame at or after that PTS is returned. When a
    /// step has several observations of the same postcondition, the earliest is
    /// used.
    pub fn frame_at(&self, step_id: &str, postcondition: &str) -> Result<&SourceFrame> {
        let observed = self
            .earliest_observation(step_id, |p| p.description == postcondition)
            .ok_or_else(|| {
                RecordError::new(
                    Code::NotFound,
                    format!("no observed postcondition '{postcondition}' for step '{step_id}'"),
                )
            })?;
        self.first_frame_from(step_id, observed)
    }

    /// The source frame that first shows a target's postcondition within a step.
    /// Postconditions are matched by the `target.targetId` of their event, not by
    /// description text. Resolution is the same as [`BundleReader::frame_at`]: the
    /// earliest observation of the target, mapped through the clock map to the
    /// first frame at or after it. A bundle recorded without target ids returns
    /// `Code::NotFound` for every target.
    pub fn frame_at_target(&self, step_id: &str, target_id: &str) -> Result<&SourceFrame> {
        let observed = self
            .earliest_observation(step_id, |p| p.target_id.as_deref() == Some(target_id))
            .ok_or_else(|| {
                RecordError::new(
                    Code::NotFound,
                    format!("no observed postcondition for target '{target_id}' in step '{step_id}'"),
                )
            })?;
        self.first_frame_from(step_id, observed)
    }

    /// The cursor assets the bundle recorded. A bundle with no cursor manifest
    /// returns an empty list; `cursor_assets_status` tells the two apart.
    pub fn cursor_assets(&self) -> Result<Vec<CursorAsset>> {
        if self.cursor_assets_status()? == CursorAssetsStatus::NotRecorded {
            return Ok(Vec::new());
        }
        let manifest = read_json(&self.dir.join(CURSOR_MANIFEST)).map_err(|e| {
            RecordError::new(
                Code::NotBundle,
                format!("cursors/manifest.json unreadable: {e}"),
            )
        })?;
        if manifest.get("schema").and_then(Value::as_str) != Some(CURSOR_SCHEMA) {
            return Err(RecordError::new(
                Code::NotBundle,
                "cursors/manifest.json has an unknown schema",
            ));
        }
        let entries = manifest
            .get("cursors")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                RecordError::new(Code::NotBundle, "cursors/manifest.json has no cursors list")
            })?;
        entries.iter().map(parse_cursor).collect()
    }

    /// Whether the bundle has a cursor manifest. Does not validate it.
    pub fn cursor_assets_status(&self) -> Result<CursorAssetsStatus> {
        Ok(if self.dir.join(CURSOR_MANIFEST).is_file() {
            CursorAssetsStatus::Recorded
        } else {
            CursorAssetsStatus::NotRecorded
        })
    }

    /// The earliest observed host time among a step's postconditions that match.
    fn earliest_observation(
        &self,
        step_id: &str,
        matches: impl Fn(&ObservedPostcondition) -> bool,
    ) -> Option<u64> {
        self.postconditions
            .get(step_id)
            .into_iter()
            .flatten()
            .filter(|p| matches(p))
            .map(|p| p.observed_host_ns)
            .min()
    }

    fn first_frame_from(&self, step_id: &str, observed_host_ns: u64) -> Result<&SourceFrame> {
        let target_pts = self.clock.host_to_sample_pts(observed_host_ns);
        let position = self
            .frames
            .partition_point(|f| f.sample_pts_ns < target_pts);
        self.frames.get(position).ok_or_else(|| {
            RecordError::new(
                Code::NotFound,
                format!("no frame at or after the postcondition of step '{step_id}'"),
            )
        })
    }
}

/// The manifest's `reason`, when the bundle carries one.
fn manifest_reason(dir: &Path) -> Option<String> {
    read_json(&dir.join(MANIFEST_FILE))
        .ok()?
        .get("reason")?
        .as_str()
        .map(str::to_string)
}

fn parse_cursor(entry: &Value) -> Result<CursorAsset> {
    let malformed = || RecordError::new(Code::NotBundle, "cursor manifest entry is malformed");
    let text = |name: &str| entry.get(name).and_then(Value::as_str);
    let number = |name: &str| {
        entry
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let (Some(id), Some(path), Some(hotspot_x), Some(hotspot_y), Some(width), Some(height)) = (
        text("id"),
        text("path"),
        number("hotspotX"),
        number("hotspotY"),
        number("width"),
        number("height"),
    ) else {
        return Err(malformed());
    };
    let path = PathBuf::from(path);
    // Under `cursors/`, one or more normal components deep.
    let inside_cursors = path.starts_with("cursors")
        && path.components().count() > 1
        && path.components().all(|c| matches!(c, Component::Normal(_)));
    if !valid_id(id) || !inside_cursors || width == 0 || height == 0 {
        return Err(malformed());
    }
    if hotspot_x >= width || hotspot_y >= height {
        return Err(malformed());
    }
    Ok(CursorAsset {
        id: id.to_string(),
        path,
        hotspot_x,
        hotspot_y,
        width,
        height,
    })
}

/// The lazy event stream behind [`BundleReader::events`].
struct EventStream {
    lines: Option<io::Lines<BufReader<File>>>,
    /// Set when the file could not be opened. Yielded once, then the stream ends.
    open_error: Option<RecordError>,
    line_no: usize,
}

impl EventStream {
    fn open(path: &Path) -> EventStream {
        match File::open(path) {
            Ok(file) => EventStream {
                lines: Some(BufReader::new(file).lines()),
                open_error: None,
                line_no: 0,
            },
            Err(error) => EventStream {
                lines: None,
                open_error: Some(RecordError::new(
                    Code::NotBundle,
                    format!("events.ndjson unreadable: {error}"),
                )),
                line_no: 0,
            },
        }
    }
}

impl Iterator for EventStream {
    type Item = Result<RecordEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(error) = self.open_error.take() {
            return Some(Err(error));
        }
        loop {
            let next = self.lines.as_mut()?.next()?;
            self.line_no += 1;
            let line = match next {
                Ok(line) => line,
                Err(error) => {
                    self.lines = None;
                    return Some(Err(RecordError::new(
                        Code::Io,
                        format!("events.ndjson line {} unreadable: {error}", self.line_no),
                    )));
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            return match serde_json::from_str::<RecordEvent>(&line) {
                Ok(event) => Some(Ok(event)),
                Err(error) => {
                    self.lines = None;
                    Some(Err(RecordError::new(
                        Code::NotBundle,
                        format!("events.ndjson line {}: {error}", self.line_no),
                    )))
                }
            };
        }
    }
}
