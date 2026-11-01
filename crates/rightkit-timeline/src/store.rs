//! Revision-backed timeline store: apply, undo, redo, canonical JSON persistence.
//!
//! The persisted document is the base fps plus the revision log and a cursor. The
//! current timeline is derived by replaying the log, so reopening restores both
//! state and history; every revision carries the BLAKE3 of the canonical timeline
//! it produced, checked on load.

use serde::{Deserialize, Serialize};

use crate::action::{apply_action, apply_inverse, Action, InverseAction};
use crate::error::TimelineError;
use crate::model::Timeline;
use crate::time::Fps;

pub const SCHEMA_VERSION: u32 = 1;
const DOCUMENT_KIND: &str = "cutright.timeline";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    /// 1-based position in the log.
    pub seq: u64,
    pub action: Action,
    /// Replayed in order by undo.
    pub inverse: Vec<InverseAction>,
    /// BLAKE3 (hex) of the canonical timeline JSON after this revision.
    pub result_blake3: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    kind: String,
    fps: Fps,
    /// Number of revisions currently applied; the rest are redo history.
    cursor: u64,
    revisions: Vec<Revision>,
}

/// Result of [`TimelineStore::apply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// A revision was recorded with this `seq`.
    Recorded(u64),
    /// The action changed nothing (idempotent retry); no revision recorded.
    NoChange,
}

#[derive(Debug, Clone)]
pub struct TimelineStore {
    fps: Fps,
    timeline: Timeline,
    revisions: Vec<Revision>,
    cursor: usize,
}

fn timeline_hash(tl: &Timeline) -> String {
    let bytes = serde_json::to_vec(tl).expect("timeline serializes");
    rightkit_fs::digest::blake3_bytes(&bytes)
}

impl TimelineStore {
    pub fn new(fps: Fps) -> Self {
        Self {
            fps,
            timeline: Timeline::new(fps),
            revisions: Vec::new(),
            cursor: 0,
        }
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn revisions(&self) -> &[Revision] {
        &self.revisions
    }

    /// Number of revisions currently applied.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.revisions.len()
    }

    /// BLAKE3 (hex) of the current timeline's canonical compact JSON.
    pub fn head_hash(&self) -> String {
        timeline_hash(&self.timeline)
    }

    /// Applies an action atomically. Applying discards any redo history.
    pub fn apply(&mut self, action: Action) -> Result<ApplyOutcome, TimelineError> {
        let (next, inverse) = apply_action(&self.timeline, &action)?;
        if next == self.timeline {
            return Ok(ApplyOutcome::NoChange);
        }
        self.revisions.truncate(self.cursor);
        let seq = self.revisions.len() as u64 + 1;
        self.revisions.push(Revision {
            seq,
            action,
            inverse,
            result_blake3: timeline_hash(&next),
        });
        self.cursor += 1;
        self.timeline = next;
        Ok(ApplyOutcome::Recorded(seq))
    }

    pub fn undo(&mut self) -> Result<(), TimelineError> {
        if self.cursor == 0 {
            return Err(TimelineError::NothingToUndo);
        }
        let rev = &self.revisions[self.cursor - 1];
        self.timeline = apply_inverse(&self.timeline, &rev.inverse)?;
        self.cursor -= 1;
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), TimelineError> {
        if self.cursor >= self.revisions.len() {
            return Err(TimelineError::NothingToRedo);
        }
        let rev = &self.revisions[self.cursor];
        let (next, inverse) = apply_action(&self.timeline, &rev.action)?;
        if inverse != rev.inverse || timeline_hash(&next) != rev.result_blake3 {
            return Err(TimelineError::Integrity {
                seq: rev.seq,
                detail: "redo diverged from the recorded revision".into(),
            });
        }
        self.timeline = next;
        self.cursor += 1;
        Ok(())
    }

    /// Canonical document bytes: pretty JSON, struct-declared key order, trailing newline.
    pub fn to_canonical_json(&self) -> Vec<u8> {
        let doc = Document {
            schema_version: SCHEMA_VERSION,
            kind: DOCUMENT_KIND.into(),
            fps: self.fps,
            cursor: self.cursor as u64,
            revisions: self.revisions.clone(),
        };
        let mut out = serde_json::to_vec_pretty(&doc).expect("document serializes");
        out.push(b'\n');
        out
    }

    /// Reopens a saved document: replays and verifies every revision (state and
    /// inverse), then rewinds to the saved cursor.
    pub fn from_canonical_json(bytes: &[u8]) -> Result<Self, TimelineError> {
        let doc: Document =
            serde_json::from_slice(bytes).map_err(|e| TimelineError::Json(e.to_string()))?;
        if doc.schema_version != SCHEMA_VERSION {
            return Err(TimelineError::SchemaVersion(doc.schema_version));
        }
        if doc.kind != DOCUMENT_KIND {
            return Err(TimelineError::Json(format!(
                "unexpected kind {:?}",
                doc.kind
            )));
        }
        Fps::new(doc.fps.num, doc.fps.den)?;
        if doc.cursor as usize > doc.revisions.len() {
            return Err(TimelineError::Json("cursor beyond revision log".into()));
        }
        let mut tl = Timeline::new(doc.fps);
        let mut snapshots = vec![tl.clone()];
        for (i, rev) in doc.revisions.iter().enumerate() {
            let integrity = |detail: &str| TimelineError::Integrity {
                seq: rev.seq,
                detail: detail.into(),
            };
            if rev.seq != i as u64 + 1 {
                return Err(integrity("sequence gap"));
            }
            let (next, inverse) = apply_action(&tl, &rev.action)?;
            if inverse != rev.inverse {
                return Err(integrity("recorded inverse does not match replay"));
            }
            if timeline_hash(&next) != rev.result_blake3 {
                return Err(integrity("timeline hash mismatch"));
            }
            tl = next;
            snapshots.push(tl.clone());
        }
        // Verify the recorded inverses actually restore history, then rewind.
        for (i, rev) in doc.revisions.iter().enumerate().rev() {
            let back = apply_inverse(&snapshots[i + 1], &rev.inverse)?;
            if back != snapshots[i] {
                return Err(TimelineError::Integrity {
                    seq: rev.seq,
                    detail: "inverse does not restore the previous state".into(),
                });
            }
        }
        let cursor = doc.cursor as usize;
        Ok(Self {
            fps: doc.fps,
            timeline: snapshots[cursor].clone(),
            revisions: doc.revisions,
            cursor,
        })
    }
}
