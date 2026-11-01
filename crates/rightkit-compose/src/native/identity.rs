//! Frame identity and request scheduling types.
//!
//! Semantics adapted from Clypra's `wgpu_compositor` frame request/deadline
//! modules (MIT, rev efd8ab33): production (`FrameRequest`: "what frame must
//! exist") is separated from presentation, deadlines combine a priority class
//! with a wall-clock `present_by`, and the most urgent deadline orders first
//! in a max-heap. All types are rewritten against [`SnapshotIdentity`] —
//! The project+revision+timeline-hash tuple replaces Clypra's
//! clip/session identity so that stale revision, cancellation and project
//! switches are expressed as identity mismatches, never as delivered frames.

use std::cmp::Ordering;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::native::snapshot::SnapshotIdentity;

/// Canonical identity of one composed output frame.
///
/// Two requests with equal keys are the same frame; the hash is also the
/// frame-cache key. Export uses `scale = 1.0` at delivery dimensions, so an
/// export frame and a full-quality preview frame of the same revision share
/// identity — that is the export-compatible frame identity contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameKey {
    pub identity: SnapshotIdentity,
    /// Output frame index under the plan's fps.
    pub frame: i64,
    /// Effective render dimensions (post quality-scale).
    pub width: u32,
    pub height: u32,
    /// Quality scale used for decode/composite (`1.0` = full).
    pub quality_scale: f32,
}

impl FrameKey {
    /// Content-addressed cache key: BLAKE3 of the canonical fields.
    pub fn cache_key(&self) -> String {
        let canonical = format!(
            "{}|{}|{}|f{}|{}x{}|q{}",
            self.identity.project,
            self.identity.revision,
            self.identity.timeline_blake3,
            self.frame,
            self.width,
            self.height,
            self.quality_scale
        );
        rightkit_fs::digest::blake3_bytes(canonical.as_bytes())
    }
}

/// Scheduling priority tier (Clypra `FramePriority` semantics).
/// Ordered `Realtime > Interactive > Background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FramePriority {
    /// Thumbnails / lookahead prefetch; first to drop under pressure.
    Background = 0,
    /// Scrub, seek, frame-step. Never dropped for being slow.
    Interactive = 1,
    /// Active playback. May be dropped when its deadline expires.
    Realtime = 2,
}

/// Quality tier for a frame request (Clypra `PreviewQuality` semantics).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum PreviewQuality {
    /// 0.25x — thumbnails / emergency.
    Quarter,
    /// 0.5x — scrub.
    Half,
    /// 1.0x — normal playback, paused preview, export.
    Full,
}

impl PreviewQuality {
    pub fn scale_factor(&self) -> f32 {
        match self {
            Self::Quarter => 0.25,
            Self::Half => 0.5,
            Self::Full => 1.0,
        }
    }
}

/// Presentation deadline: priority class plus wall-clock `present_by`
/// (Clypra `FrameDeadline` semantics). Orders so the *earliest* deadline is
/// greatest in a max-heap; ties break toward higher priority.
#[derive(Debug, Clone)]
pub struct FrameDeadline {
    pub present_by: Instant,
    pub priority: FramePriority,
}

impl FrameDeadline {
    /// Deadline one frame interval from now at `fps`.
    pub fn for_fps(fps: f64, priority: FramePriority) -> Self {
        let frame_us = (1_000_000.0 / fps) as u64;
        Self {
            present_by: Instant::now() + Duration::from_micros(frame_us),
            priority,
        }
    }

    /// "As soon as possible" — scrub and single-frame export.
    pub fn immediate(priority: FramePriority) -> Self {
        Self {
            present_by: Instant::now(),
            priority,
        }
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.present_by
    }

    pub fn time_remaining(&self) -> Option<Duration> {
        self.present_by.checked_duration_since(Instant::now())
    }
}

impl PartialEq for FrameDeadline {
    fn eq(&self, other: &Self) -> bool {
        self.present_by == other.present_by && self.priority == other.priority
    }
}

impl Eq for FrameDeadline {}

impl PartialOrd for FrameDeadline {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FrameDeadline {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed on time so the earliest deadline pops first from a max-heap.
        other
            .present_by
            .cmp(&self.present_by)
            .then(self.priority.cmp(&other.priority))
    }
}

/// A request for one composed frame. Contains no presentation target: frame
/// production and presentation are separate concerns.
#[derive(Debug, Clone)]
pub struct FrameRequest {
    pub key: FrameKey,
    pub deadline: FrameDeadline,
}
