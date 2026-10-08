use serde::{Deserialize, Serialize};

use super::Rect;

/// `record-session` v1. The bundle manifest, written as the bundle's hashes.json.
/// It hashes every other file in the bundle and never hashes itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecordSession {
    pub contract: String,
    pub contract_version: String,
    pub bundle_id: String,
    pub created_at: String,
    pub status: BundleStatus,
    #[serde(default)]
    pub reason: Option<String>,
    pub recorder: Recorder,
    pub scenario: ScenarioRef,
    pub target: Target,
    pub capture: Capture,
    pub geometry_epochs: Vec<GeometryEpoch>,
    pub clock: Clock,
    pub frames: Frames,
    pub events: EventsRef,
    pub files: Files,
    #[serde(default)]
    pub tracks: Vec<Track>,
    pub dependencies: Vec<Dependency>,
    pub redaction: Redaction,
    pub donor_directory_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleStatus {
    Complete,
    PartialQuarantined,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Recorder {
    pub engine: String,
    pub version: String,
    pub os: RecorderOs,
    pub os_build: String,
    pub helper: Helper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecorderOs {
    Macos,
    Windows,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Helper {
    pub identity: String,
    pub signed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScenarioRef {
    pub scenario_id: String,
    pub blake3: String,
    pub steps: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Target {
    pub app_id: String,
    pub surfaces: Vec<Surface>,
}

/// A native surface that belongs to the target app. R12 records only these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Surface {
    pub surface_id: String,
    pub kind: SurfaceKind,
    #[serde(default)]
    pub parent_surface_id: Option<String>,
    pub native_id: String,
    pub supported: bool,
    #[serde(default)]
    pub unsupported_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    MainWindow,
    NativeSheet,
    ContextMenu,
    SecondaryWindow,
    DragDrop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Capture {
    pub fps: u32,
    pub scale: f64,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub scope: CaptureScope,
    pub display_opt_in: bool,
    pub visibility_state: VisibilityState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScope {
    AppSurfaces,
    Display,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityState {
    OffScreenVisible,
    OnScreenVisible,
    AppHidden,
    Occluded,
    Minimized,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GeometryEpoch {
    pub epoch: u32,
    pub start_ms: u64,
    #[serde(default)]
    pub end_ms: Option<u64>,
    pub bounds_pt: Rect,
    pub scale: f64,
    #[serde(default)]
    pub dpi: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Clock {
    pub host_clock: HostClock,
    pub host_ns_at_start: u64,
    pub first_accepted_frame: FirstFrame,
    pub mapping: ClockMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostClock {
    MachAbsoluteTime,
    Qpc,
}

/// The first frame the encoder accepted. Events bind to frames through it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FirstFrame {
    pub frame_index: u64,
    pub host_ns: u64,
    pub sample_pts_ns: u64,
}

/// Declared host clock to sample pts to encoded pts mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClockMapping {
    pub host_to_sample_pts_offset_ns: i64,
    pub sample_pts_to_encoded_pts_offset_ns: i64,
    pub timescale: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Frames {
    pub count: u64,
    pub dropped_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EventsRef {
    pub count: u64,
    pub contract: String,
    pub contract_version: String,
}

/// The hashed files. Each optional video key is mutually exclusive with the
/// other; the schema enforces exactly one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Files {
    #[serde(rename = "scenario.json")]
    pub scenario_json: FileEntry,
    #[serde(rename = "events.ndjson")]
    pub events_ndjson: FileEntry,
    #[serde(rename = "clock.json")]
    pub clock_json: FileEntry,
    #[serde(rename = "capture.json")]
    pub capture_json: FileEntry,
    #[serde(rename = "clean.mp4", default)]
    pub clean_mp4: Option<FileEntry>,
    #[serde(rename = "clean.mov", default)]
    pub clean_mov: Option<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub blake3: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Track {
    pub track_id: String,
    pub kind: TrackKind,
    pub file: String,
    pub blake3: String,
    pub bytes: u64,
    pub offset_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    AppAudio,
    Microphone,
    WebcamVideo,
}

/// A cursor asset carries a hotspot, and recipe or style dependencies carry a
/// version. Every entry is content-addressed so the bundle renders the same on
/// the other OS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Dependency {
    pub kind: DependencyKind,
    pub id: String,
    pub version: String,
    pub blake3: String,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub hotspot: Option<Hotspot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    CursorAsset,
    Recipe,
    Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hotspot {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Redaction {
    pub policy_id: String,
    pub policy_version: String,
    pub pixel_masking: PixelMasking,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PixelMasking {
    pub mode: MaskingMode,
    pub region_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskingMode {
    DeclaredRegions,
    CaptureSafeState,
}
