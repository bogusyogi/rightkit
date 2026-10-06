//! Live microphone capture: cpal stream -> lock-free ring -> adaptive mono downmix ->
//! rubato resample -> staged f32 reads.
//!
//! Donor: HeardRight `heardright_capture` (the most mature capture path in the suite:
//! stable platform-id device resolution, per-transport classification, adaptive
//! downmix that survives echo-cancellation reference channels, prewarmed ring,
//! pause/resume without teardown, typed terminal-vs-advisory stream errors).
//!
//! Threading: cpal owns an OS audio thread that calls the data callback, which only
//! converts samples to mono f32, pushes into the SPSC ring and bumps atomics (no
//! allocation, no logging). The consumer pops, resamples and reads.

use std::fmt;

mod device;
mod downmix;
mod metrics;
mod resample;
mod route;
mod session;
mod staging;

pub use device::{
    list_input_devices, resolve_device, CaptureFormFactor, CaptureTransport, DeviceInfo,
};
pub use downmix::MonoDownmix;
pub use metrics::{CaptureErrorKind, MetricsSnapshot};
pub use resample::StreamResampler;
pub use route::{
    capture_open_error, capture_route_changed, current_capture_route, open_capture_paused,
    resolve_capture_route, CaptureRouteSignature,
};
pub use session::{CaptureFirstBuffer, CaptureSession, OutDType, SessionConfig};

/// Capture failure with the OS/cpal detail text. Terminal stream errors are
/// additionally exposed as [`CaptureErrorKind`] through [`MetricsSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError(pub String);

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CaptureError {}

pub type Result<T> = std::result::Result<T, CaptureError>;

macro_rules! anyhow {
    ($($arg:tt)*) => {
        $crate::capture::CaptureError(format!($($arg)*))
    };
}
pub(crate) use anyhow;
