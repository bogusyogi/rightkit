//! The seam between the platform-neutral core and the per-OS helpers. A helper
//! owns capture, encoding and the OS permission surface. The core owns every
//! file, the clock map, redaction and each lifecycle decision. The helper never
//! writes a bundle manifest.

use std::path::PathBuf;

use rightkit_contracts::types::record_event::DropReason;
use rightkit_contracts::types::record_session::{
    CaptureBorder, RecorderOs, TrackKind, VisibilityState,
};
use rightkit_contracts::types::Rect;

use crate::error::RecordError;
use crate::plan::PixelMasking;

/// The screen-recording permission of the helper identity.
///
/// `permission()` reports the state without prompting the user. A helper that
/// cannot read the state without prompting reports `Undetermined`, and the
/// prompt, if any, happens in `start`, which reports a refusal as
/// `Code::PermissionDenied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
    Undetermined,
}

/// The encoded video codecs a take may request. `prores` is a macOS follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn parse(value: &str) -> Option<Codec> {
        match value {
            "h264" => Some(Codec::H264),
            "hevc" => Some(Codec::Hevc),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperIdentity {
    pub identity: String,
    pub signed: bool,
}

/// A capturable window the helper can see.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub window_id: u64,
    pub pid: u32,
    pub app_id: String,
    pub title: String,
    pub bounds_pt: Rect,
    pub visibility: VisibilityState,
}

/// One frame as the helper reports it. The helper hashes the raw pixels off the
/// capture callback. The core never sees pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSample {
    pub index: u64,
    pub host_ns: u64,
    pub sample_pts_ns: u64,
    pub raw_hash: String,
}

/// Why a take cannot finish as a complete bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The helper process died or stopped answering.
    HelperLost,
    DiskFull,
    /// The target app exited mid-take.
    TargetExited,
    /// Screen-recording permission was withdrawn mid-take.
    PermissionRevoked,
    /// The target window was hidden mid-take, so it can deliver no frames.
    WindowHidden,
    /// The target window was minimized mid-take, so it can deliver no frames.
    WindowMinimized,
    /// The helper could not establish the masked pixel region before encoding.
    MaskGeometryUnknown,
    /// The helper broke the protocol (frame before readiness, PTS going back).
    Protocol(String),
    Io(String),
}

impl Failure {
    pub fn reason(&self) -> String {
        match self {
            Failure::HelperLost => "helper_lost".into(),
            Failure::DiskFull => "disk_full".into(),
            Failure::TargetExited => "target_exited".into(),
            Failure::PermissionRevoked => "permission_revoked".into(),
            Failure::WindowHidden => "window_hidden".into(),
            Failure::WindowMinimized => "window_minimized".into(),
            Failure::MaskGeometryUnknown => "mask_geometry_unknown".into(),
            Failure::Protocol(detail) => format!("protocol_violation: {detail}"),
            Failure::Io(detail) => format!("io_error: {detail}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HelperEvent {
    /// The first accepted frame: the readiness marker. No `Frame` is valid
    /// before it, and the clock map is anchored to it.
    Ready(FrameSample),
    Frame(FrameSample),
    Dropped {
        host_ns: u64,
        count: u32,
        interval_ms: f64,
        reason: DropReason,
    },
    Geometry {
        host_ns: u64,
        bounds_pt: Rect,
        scale: f64,
        dpi: u32,
    },
    Failed(Failure),
    /// The helper process ended without being asked to stop.
    Exited,
}

/// What the core asks a helper to start.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendPlan {
    pub out_dir: PathBuf,
    pub window_id: u64,
    pub pid: u32,
    pub fps: u32,
    pub scale: f64,
    pub audio: Vec<TrackKind>,
    pub visibility: VisibilityState,
    pub masking: PixelMasking,
    pub codec: Codec,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StartInfo {
    pub host_ns_at_start: u64,
    pub width: u32,
    pub height: u32,
    /// The scale the backend applied. A backend that cannot honour the
    /// requested scale refuses the start instead of reporting a different one.
    pub scale: f64,
    pub bounds_pt: Rect,
    pub dpi: u32,
    /// The codec the encoder was configured with, `h264` or `hevc`.
    pub codec: String,
    pub os_build: String,
    /// The visibility state the backend observed for the target at start. The
    /// core refuses the take when it differs from the declared state.
    pub visibility: VisibilityState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackOutput {
    pub kind: TrackKind,
    pub offset_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// False when the media could not be finalized cleanly.
    pub clean: bool,
    pub tracks: Vec<TrackOutput>,
}

/// Bundle-relative media path of a track. Fixed per kind so recovery can find
/// track files without the helper's report.
pub fn track_file(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::AppAudio => "tracks/app_audio.m4a",
        TrackKind::Microphone => "tracks/microphone.m4a",
        TrackKind::WebcamVideo => "tracks/webcam.mp4",
    }
}

pub fn track_id(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::AppAudio => "app_audio",
        TrackKind::Microphone => "microphone",
        TrackKind::WebcamVideo => "webcam",
    }
}

pub trait Backend {
    fn os(&self) -> RecorderOs;
    fn os_build(&self) -> String;
    fn helper(&self) -> HelperIdentity;
    /// Reports the screen-recording permission now. It must not prompt. A
    /// helper that cannot tell without prompting returns `Undetermined`.
    fn permission(&self) -> Permission;
    fn sources(&self) -> Result<Vec<Source>, RecordError>;
    /// Starts capture. Writes the media and track files under `plan.out_dir`.
    /// Honours `plan.scale` and `plan.codec`, or refuses with `Code::Unsupported`
    /// or `Code::CodecUnavailable`. Requests permission here when undetermined.
    fn start(&mut self, plan: &BackendPlan) -> Result<StartInfo, RecordError>;
    /// The OS capture border state of the take that `start` began. `None` when
    /// the backend reports none, which is the default. A `required` border means
    /// the frames contain it, and the bundle records that.
    fn border(&self) -> Option<CaptureBorder> {
        None
    }
    /// The current time on the clock this backend stamps frames and events with (`hostNs`: the
    /// value `StartInfo::host_ns_at_start` and `FrameSample::host_ns` are on). Lets a host stamp
    /// its own bundle events on the frames' clock. `None` when the backend cannot read it, which
    /// is the default: a backend whose clock lives in another process reports none.
    fn host_ns(&self) -> Option<u64> {
        None
    }
    /// Drains the helper events received since the last call. Never blocks.
    fn poll(&mut self) -> Vec<HelperEvent>;
    /// Finalizes the media and reports whether the finish was clean.
    fn stop(&mut self) -> Result<Finished, RecordError>;
    /// Ends the helper without a clean finish. Media already on disk is kept
    /// for quarantine.
    fn abort(&mut self);
}
