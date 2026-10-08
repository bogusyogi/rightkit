//! Reads a sealed bundle back for the director and the carousel. It answers two
//! questions: which source frame shows a step's postcondition, and when each step
//! ran. Both are answered on the bundle's clock map, never by elapsed process time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rightkit_contracts::types::record_session::BundleStatus;
use serde_json::Value;

use crate::bundle::{read_json, read_lines, CLOCK_FILE, EVENTS_FILE, MANIFEST_FILE};
use crate::error::{Code, RecordError, Result};

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedPostcondition {
    description: String,
    observed_host_ns: u64,
}

pub struct BundleReader {
    dir: PathBuf,
    status: Option<BundleStatus>,
    offset_ns: i64,
    frames: Vec<SourceFrame>,
    postconditions: BTreeMap<String, Vec<ObservedPostcondition>>,
    steps: BTreeMap<String, StepSpan>,
}

impl BundleReader {
    /// Opens a bundle without re-hashing it. Call `bundle_verify` first when the
    /// bundle has not been checked.
    pub fn open(dir: &Path) -> Result<BundleReader> {
        let manifest = read_json(&dir.join(MANIFEST_FILE))
            .map_err(|e| RecordError::new(Code::NotBundle, format!("manifest unreadable: {e}")))?;
        let status = manifest
            .get("status")
            .and_then(|s| serde_json::from_value(s.clone()).ok());

        let clock = read_json(&dir.join(CLOCK_FILE)).map_err(|e| {
            RecordError::new(Code::NotBundle, format!("clock.json unreadable: {e}"))
        })?;
        let offset_ns = clock
            .pointer("/mapping/hostToSamplePtsOffsetNs")
            .and_then(Value::as_i64)
            .ok_or_else(|| RecordError::new(Code::NotBundle, "clock.json has no clock map"))?;
        let mut frames = Vec::new();
        for frame in clock
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
                postconditions.entry(step_id.to_string()).or_default().push(
                    ObservedPostcondition {
                        description,
                        observed_host_ns,
                    },
                );
            }
        }

        Ok(BundleReader {
            dir: dir.to_path_buf(),
            status,
            offset_ns,
            frames,
            postconditions,
            steps,
        })
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

    /// The source frame that first shows the state a step's postcondition
    /// observed. The postcondition's host time is mapped onto the sample PTS
    /// timeline, then the first frame at or after that PTS is returned. When a
    /// step has several observations of the same postcondition, the earliest is
    /// used.
    pub fn frame_at(&self, step_id: &str, postcondition: &str) -> Result<&SourceFrame> {
        let observed = self
            .postconditions
            .get(step_id)
            .into_iter()
            .flatten()
            .filter(|p| p.description == postcondition)
            .map(|p| p.observed_host_ns)
            .min()
            .ok_or_else(|| {
                RecordError::new(
                    Code::NotFound,
                    format!("no observed postcondition '{postcondition}' for step '{step_id}'"),
                )
            })?;
        let target_pts = (observed as i64).saturating_add(self.offset_ns).max(0) as u64;
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
