//! The typed QA report: one entry per check, each with a status, a severity and evidence.

use serde::{Deserialize, Serialize};

use crate::render_qa::spec::QaSpec;

pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Version of the check code that judges a render (`run::judge` and its measurements).
/// Bump it when a check changes meaning. `receipt::verify_receipt` refuses a report
/// judged by another version, so an old pass cannot survive a change to the checks.
///
/// Version 2: the frozen-picture noise floor moved from -60 dB to -50 dB. A hardware
/// H.264 encode keeps refining a still picture for about two seconds, and at -60 dB that
/// refinement hid the first half of a four-second freeze.
/// Version 4 adopts shared QA detectors & delivery checks (delivery check version 1).
/// Bump again when `crate::delivery::DELIVERY_CHECK_VERSION` changes semantics.
pub const CHECK_VERSION: u32 = 4;

pub use crate::qa::{Check as QaCheck, CheckStatus, OverallStatus, Severity};

/// The render under test, by content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderIdentity {
    /// File name only (the report is portable).
    pub file_name: String,
    /// Lowercase hex BLAKE3 of the file.
    pub blake3: String,
    pub size_bytes: u64,
}

/// Facts read from the render, kept so packaging needs no second probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaFacts {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub fps: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub has_video: bool,
    pub has_audio: bool,
}

pub type QaReport = crate::qa::VerdictRecord<QaSpec, RenderIdentity, MediaFacts>;
