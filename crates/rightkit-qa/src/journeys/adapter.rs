//! The app-side half of adapter protocol v1 (D4).
//!
//! An app's own Rust test adapter links this module: [`Selection::read`] loads what the runner
//! selected, a [`Reporter`] appends one JSON object per line to the results file, and
//! [`journey_from_outcomes`] maps scenario outcomes to a journey result 1:1.
//!
//! The helper refuses what the protocol forbids: a journey that was not selected, a second record
//! for the same id, a `passed` journey that has no passed unit or has a failed or skipped one, a
//! result file that already holds data, records before `start` or after `end`. It never turns one
//! status into another, never retries, and never writes a `missing` status (the runner alone
//! synthesizes that). `end` always states the true `selected` and `reported` counts, and returns
//! an error when they differ.
use super::{is_hex_lower, ArtifactDigest, Evidence, JourneyStatus, Units, PROTOCOL};
use crate::harness::Outcome;
use crate::util::{err, now_iso, sha256_hex, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const ENV_PROTOCOL: &str = "RIGHTKIT_JOURNEYS_PROTOCOL";
pub const ENV_SELECTION: &str = "RIGHTKIT_JOURNEYS_SELECTION";
pub const ENV_RESULTS: &str = "RIGHTKIT_JOURNEYS_RESULTS";
/// Set by the isolation guard; the adapter's `start.isolation.data_dir` must come from it.
pub const ENV_DATA_DIR: &str = "RIGHTKIT_QA_DATA_DIR";

/// One entry of `selection.json`'s `journeys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedJourney {
    pub id: String,
    pub entry: String,
}

/// `selection.json`, written by the runner and read-only to the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub protocol: u32,
    pub run_id: String,
    pub nonce: String,
    pub app: String,
    pub platform: String,
    pub tier: String,
    pub manifest_sha256: String,
    pub journeys: Vec<SelectedJourney>,
}

impl Selection {
    /// Read the selection named by `RIGHTKIT_JOURNEYS_SELECTION`, after checking
    /// `RIGHTKIT_JOURNEYS_PROTOCOL` is `1`.
    pub fn read() -> Result<Selection> {
        match std::env::var(ENV_PROTOCOL) {
            Ok(v) if v.trim() == PROTOCOL.to_string() => {}
            Ok(v) => {
                return err(format!(
                    "{ENV_PROTOCOL} is {v:?}, this adapter speaks protocol {PROTOCOL}"
                ))
            }
            Err(_) => {
                return err(format!(
                    "{ENV_PROTOCOL} is not set: not started by a journeys runner"
                ))
            }
        }
        let path = std::env::var_os(ENV_SELECTION)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| Error(format!("{ENV_SELECTION} is not set")))?;
        Selection::from_path(Path::new(&path))
    }

    pub fn from_path(path: &Path) -> Result<Selection> {
        let bytes = std::fs::read(path)
            .map_err(|e| Error(format!("cannot read selection {}: {e}", path.display())))?;
        Selection::parse(&bytes)
    }

    /// Parse and validate `selection.json` content. Zero selected journeys is an error, never a pass.
    pub fn parse(bytes: &[u8]) -> Result<Selection> {
        let s: Selection = serde_json::from_slice(bytes)?;
        if s.protocol != PROTOCOL {
            return err(format!(
                "selection protocol is {}, expected {PROTOCOL}",
                s.protocol
            ));
        }
        if s.run_id.trim().is_empty() {
            return err("selection run_id is empty");
        }
        if !is_hex_lower(&s.nonce, 32) {
            return err("selection nonce must be 32 lowercase hex characters");
        }
        if s.app.trim().is_empty() || s.platform.trim().is_empty() || s.tier.trim().is_empty() {
            return err("selection app, platform and tier are required");
        }
        if !is_hex_lower(&s.manifest_sha256, 64) {
            return err("selection manifest_sha256 must be 64 lowercase hex characters");
        }
        if s.journeys.is_empty() {
            return err("selection holds zero journeys; zero selected is an error, never a pass");
        }
        let mut seen = BTreeSet::new();
        for j in &s.journeys {
            if j.id.trim().is_empty() || j.entry.trim().is_empty() {
                return err("selection holds a journey with an empty id or entry");
            }
            if !seen.insert(j.id.as_str()) {
                return err(format!("selection lists journey {} twice", j.id));
            }
        }
        Ok(s)
    }

    pub fn ids(&self) -> Vec<String> {
        self.journeys.iter().map(|j| j.id.clone()).collect()
    }

    /// The entry the runner selected for `id`.
    pub fn entry(&self, id: &str) -> Option<&str> {
        self.journeys
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.entry.as_str())
    }
}

/// `start.isolation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Isolation {
    pub data_dir: String,
    pub guard: String,
    pub qa_mode: String,
}

impl Isolation {
    /// Isolation as the adapter can attest it: `RIGHTKIT_QA_DATA_DIR` is set, non-empty and
    /// absolute. This is the adapter's own statement, not the guard's verdict; the runner checks
    /// the directory against the guard and refuses a record that says anything but `passed`.
    pub fn from_env() -> Result<Isolation> {
        let dir = std::env::var(ENV_DATA_DIR).unwrap_or_default();
        if dir.trim().is_empty() || !Path::new(&dir).is_absolute() {
            return err(format!(
                "{ENV_DATA_DIR} is unset or not absolute: refusing to report an isolated run"
            ));
        }
        Ok(Isolation {
            data_dir: dir,
            guard: "passed".into(),
            qa_mode: "runtime".into(),
        })
    }
}

impl ArtifactDigest {
    /// Hash `path` now. Do it immediately before launching the binary.
    pub fn of(role: &str, path: &Path) -> Result<ArtifactDigest> {
        let bytes = std::fs::read(path)
            .map_err(|e| Error(format!("cannot hash artifact {}: {e}", path.display())))?;
        Ok(ArtifactDigest {
            role: role.to_string(),
            path: path.display().to_string(),
            sha256: sha256_hex(&bytes),
        })
    }
}

/// What the adapter reports in `start`.
#[derive(Debug, Clone)]
pub struct StartInfo {
    pub adapter: String,
    pub adapter_version: String,
    pub artifacts: Vec<ArtifactDigest>,
    pub isolation: Isolation,
}

/// `start` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRecord {
    pub protocol: u32,
    pub run_id: String,
    pub nonce: String,
    pub adapter: String,
    pub adapter_version: String,
    pub started_at: String,
    pub artifacts: Vec<ArtifactDigest>,
    pub isolation: Isolation,
}

/// `journey` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyRecord {
    pub run_id: String,
    pub nonce: String,
    pub id: String,
    pub status: JourneyStatus,
    pub started_at: String,
    pub duration_ms: u64,
    pub units: Units,
    pub evidence: Vec<Evidence>,
    pub reason: Option<String>,
}

/// `end` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndRecord {
    pub run_id: String,
    pub nonce: String,
    pub selected: u64,
    pub reported: u64,
    pub ended_at: String,
}

/// One line of `results.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    Start(StartRecord),
    Journey(JourneyRecord),
    End(EndRecord),
}

/// The adapter's result for one journey, before it is stamped with `run_id` and `nonce`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JourneyResult {
    pub id: String,
    pub status: JourneyStatus,
    pub started_at: String,
    pub duration_ms: u64,
    pub units: Units,
    pub evidence: Vec<Evidence>,
    pub reason: Option<String>,
}

impl JourneyResult {
    pub fn with_timing(mut self, started_at: impl Into<String>, duration_ms: u64) -> Self {
        self.started_at = started_at.into();
        self.duration_ms = duration_ms;
        self
    }
    pub fn with_evidence(mut self, evidence: Vec<Evidence>) -> Self {
        self.evidence = evidence;
        self
    }
}

/// How one scenario or test inside a journey ended. A failed scenario is a value here, so the
/// adapter can report it as `failed` instead of aborting the whole protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitOutcome {
    Passed,
    Failed(String),
    Skipped(String),
}

impl From<&Outcome> for UnitOutcome {
    fn from(o: &Outcome) -> Self {
        match o {
            Outcome::Passed => UnitOutcome::Passed,
            Outcome::Skipped(why) => UnitOutcome::Skipped(why.clone()),
        }
    }
}

/// Map a journey's scenario outcomes to its result, 1:1 and without softening:
/// any failed unit gives `failed`; otherwise any skipped unit gives `skipped`; no unit at all
/// (a group entry that matched nothing) gives `errored`; only all-passed gives `passed`.
pub fn journey_from_units(id: &str, units: &[UnitOutcome]) -> JourneyResult {
    let mut u = Units {
        total: units.len() as u64,
        ..Units::default()
    };
    let mut first_failed: Option<&str> = None;
    let mut first_skipped: Option<&str> = None;
    for o in units {
        match o {
            UnitOutcome::Passed => u.passed += 1,
            UnitOutcome::Failed(why) => {
                u.failed += 1;
                first_failed.get_or_insert(why.as_str());
            }
            UnitOutcome::Skipped(why) => {
                u.skipped += 1;
                first_skipped.get_or_insert(why.as_str());
            }
        }
    }
    let (status, reason) = if u.total == 0 {
        (
            JourneyStatus::Errored,
            Some("entry matched zero scenarios".to_string()),
        )
    } else if let Some(why) = first_failed {
        (JourneyStatus::Failed, Some(why.to_string()))
    } else if let Some(why) = first_skipped {
        (JourneyStatus::Skipped, Some(why.to_string()))
    } else {
        (JourneyStatus::Passed, None)
    };
    JourneyResult {
        id: id.to_string(),
        status,
        started_at: now_iso(),
        duration_ms: 0,
        units: u,
        evidence: vec![],
        reason: reason.map(|r| {
            if r.trim().is_empty() {
                "no reason given".to_string()
            } else {
                r
            }
        }),
    }
}

/// [`journey_from_units`] for a harness scenario that either returns an [`Outcome`] or panics.
pub fn journey_from_outcomes(id: &str, outcomes: &[Outcome]) -> JourneyResult {
    let units: Vec<UnitOutcome> = outcomes.iter().map(UnitOutcome::from).collect();
    journey_from_units(id, &units)
}

/// Appends protocol records to `results.jsonl`. One `Reporter` per run; call `start`, then
/// `journey` once per selected id, then `end`.
#[derive(Debug)]
pub struct Reporter {
    file: File,
    run_id: String,
    nonce: String,
    selected: Vec<String>,
    reported: BTreeSet<String>,
    started: bool,
    ended: bool,
}

impl Reporter {
    /// Open the file named by `RIGHTKIT_JOURNEYS_RESULTS`.
    pub fn from_env(selection: &Selection) -> Result<Reporter> {
        let path = std::env::var_os(ENV_RESULTS)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| Error(format!("{ENV_RESULTS} is not set")))?;
        Reporter::open(selection, &PathBuf::from(path))
    }

    /// Open an existing, empty results file for appending. The runner creates it empty; a file
    /// that already holds data is stale and refused, and a missing file is not created.
    pub fn open(selection: &Selection, path: &Path) -> Result<Reporter> {
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| Error(format!("cannot open results file {}: {e}", path.display())))?;
        let len = file.metadata()?.len();
        if len != 0 {
            return err(format!(
                "results file {} already holds {len} byte(s): stale file, refusing to append",
                path.display()
            ));
        }
        Ok(Reporter {
            file,
            run_id: selection.run_id.clone(),
            nonce: selection.nonce.clone(),
            selected: selection.ids(),
            reported: BTreeSet::new(),
            started: false,
            ended: false,
        })
    }

    fn write(&mut self, rec: &Record) -> Result<()> {
        let mut line = serde_json::to_string(rec)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        Ok(())
    }

    /// First line. Refused when isolation is not `passed` in `runtime` QA mode.
    pub fn start(&mut self, info: StartInfo) -> Result<()> {
        if self.started {
            return err("start already written");
        }
        if info.adapter.trim().is_empty() || info.adapter_version.trim().is_empty() {
            return err("start needs adapter and adapter_version");
        }
        if info.isolation.guard != "passed"
            || info.isolation.qa_mode != "runtime"
            || info.isolation.data_dir.trim().is_empty()
        {
            return err("refusing to report start: isolation must be guard \"passed\", qa_mode \"runtime\" with a data_dir");
        }
        for a in &info.artifacts {
            if !is_hex_lower(&a.sha256, 64) || a.role.trim().is_empty() {
                return err(format!(
                    "artifact {:?} needs a role and a 64-hex sha256",
                    a.role
                ));
            }
        }
        let rec = Record::Start(StartRecord {
            protocol: PROTOCOL,
            run_id: self.run_id.clone(),
            nonce: self.nonce.clone(),
            adapter: info.adapter,
            adapter_version: info.adapter_version,
            started_at: now_iso(),
            artifacts: info.artifacts,
            isolation: info.isolation,
        });
        self.write(&rec)?;
        self.started = true;
        Ok(())
    }

    /// One line per selected id. Refused (nothing written) when the result breaks the protocol.
    pub fn journey(&mut self, r: &JourneyResult) -> Result<()> {
        if !self.started {
            return err("journey before start");
        }
        if self.ended {
            return err("journey after end");
        }
        if !self.selected.contains(&r.id) {
            return err(format!("journey {} was not selected", r.id));
        }
        if self.reported.contains(&r.id) {
            return err(format!("journey {} already reported", r.id));
        }
        if r.status == JourneyStatus::Missing {
            return err(
                "status \"missing\" is synthesized by the runner and never reported by an adapter",
            );
        }
        if !r.units.consistent() {
            return err(format!("journey {}: units do not add up to total", r.id));
        }
        if r.status == JourneyStatus::Passed && !r.units.supports_pass() {
            return err(format!(
                "journey {}: \"passed\" needs units.total >= 1 with none failed or skipped",
                r.id
            ));
        }
        if r.status != JourneyStatus::Passed
            && r.reason.as_deref().is_none_or(|s| s.trim().is_empty())
        {
            return err(format!(
                "journey {}: a reason is required unless passed",
                r.id
            ));
        }
        for e in &r.evidence {
            if !is_hex_lower(&e.sha256, 64) || !super::is_safe_relative(&e.path) {
                return err(format!(
                    "journey {}: evidence {:?} needs a relative path and a 64-hex sha256",
                    r.id, e.path
                ));
            }
        }
        let rec = Record::Journey(JourneyRecord {
            run_id: self.run_id.clone(),
            nonce: self.nonce.clone(),
            id: r.id.clone(),
            status: r.status,
            started_at: r.started_at.clone(),
            duration_ms: r.duration_ms,
            units: r.units,
            evidence: r.evidence.clone(),
            reason: r.reason.clone(),
        });
        self.write(&rec)?;
        self.reported.insert(r.id.clone());
        Ok(())
    }

    /// Last line, with the true counts. Returns an error (after writing) when a selected journey
    /// was never reported, so the adapter cannot finish quietly with a hole.
    pub fn end(&mut self) -> Result<()> {
        if !self.started {
            return err("end before start");
        }
        if self.ended {
            return err("end already written");
        }
        let rec = Record::End(EndRecord {
            run_id: self.run_id.clone(),
            nonce: self.nonce.clone(),
            selected: self.selected.len() as u64,
            reported: self.reported.len() as u64,
            ended_at: now_iso(),
        });
        self.write(&rec)?;
        self.ended = true;
        let missing: Vec<&str> = self
            .selected
            .iter()
            .filter(|id| !self.reported.contains(*id))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            err(format!(
                "end written, but {} selected journey(s) were never reported: {}",
                missing.len(),
                missing.join(", ")
            ))
        }
    }
}
