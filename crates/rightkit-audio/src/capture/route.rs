//! Capture route identity and open policy. Ported from HeardRight
//! `heardright-runtime/src/worker/capture_recovery.rs` (route signature, route-change
//! test, access-denied hint) minus the worker-state coupling: the mic-selection
//! policy here is the generic part (saved platform id, fail closed when it is gone).

use super::{
    device::{list_input_devices, CaptureFormFactor, CaptureTransport, DeviceInfo},
    session::{CaptureSession, OutDType, SessionConfig},
    Result,
};
use super::anyhow;

/// Everything that, when changed, requires reopening a warm paused stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureRouteSignature {
    pub device_id: usize,
    pub platform_id: Option<String>,
    pub uses_system_default: bool,
    pub name: String,
    pub native_rate: u32,
    pub channels: u16,
    pub sample_format: String,
    pub transport: CaptureTransport,
    pub form_factor: CaptureFormFactor,
}

/// Resolve the route for `saved_platform_id` (None = system default). A saved id that
/// is no longer present fails closed instead of silently switching microphones.
pub fn resolve_capture_route(
    saved_platform_id: Option<&str>,
    devices: &[DeviceInfo],
) -> Result<Option<CaptureRouteSignature>> {
    let (selected, uses_default) = match saved_platform_id {
        Some(id) => {
            let d = devices
                .iter()
                .find(|d| d.platform_id.as_deref() == Some(id))
                .ok_or_else(|| anyhow!("saved microphone is unavailable"))?;
            (Some(d), false)
        }
        None => (devices.iter().find(|d| d.is_default), true),
    };
    Ok(selected.map(|d| CaptureRouteSignature {
        device_id: d.id,
        platform_id: d.platform_id.clone(),
        uses_system_default: uses_default,
        name: d.name.clone(),
        native_rate: d.native_rate,
        channels: d.channels,
        sample_format: d.sample_format.clone(),
        transport: d.transport,
        form_factor: d.form_factor,
    }))
}

pub fn current_capture_route(saved_platform_id: Option<&str>) -> Result<Option<CaptureRouteSignature>> {
    let devices = list_input_devices().map_err(|e| anyhow!("capture route query: {e}"))?;
    resolve_capture_route(saved_platform_id, &devices)
}

/// A new route exists and differs from the one the warm stream was opened on.
pub fn capture_route_changed(
    previous: &Option<CaptureRouteSignature>,
    current: &Option<CaptureRouteSignature>,
) -> bool {
    current.is_some() && current != previous
}

/// Wrap an OS open error with a stable hint when it is a consent denial, so field
/// logs distinguish a denied microphone from a missing/broken device.
pub fn capture_open_error(detail: String) -> String {
    if detail.contains("Access is denied")
        || detail.contains("0x80070005")
        || detail.contains("-2147024891")
    {
        format!("capture open: {detail} (microphone permission may still be denied in OS settings)")
    } else {
        format!("capture open: {detail}")
    }
}

/// Open a capture session on the saved microphone, paused (no play/pause/play on a
/// cold start). The signature lets later starts reuse the stream while the route is
/// unchanged.
pub fn open_capture_paused(
    saved_platform_id: Option<&str>,
    target_rate: u32,
) -> Result<(CaptureSession, Option<CaptureRouteSignature>)> {
    let devices = list_input_devices().map_err(|e| anyhow!("capture devices: {e}"))?;
    let route = resolve_capture_route(saved_platform_id, &devices)?;
    let cfg = SessionConfig {
        device_id: None,
        device_platform_id: route
            .as_ref()
            .filter(|s| !s.uses_system_default)
            .and_then(|s| s.platform_id.clone()),
        target_rate,
        target_channels: 1,
        target_dtype: OutDType::Float32,
        block_ms: 20,
    };
    let session = CaptureSession::open_paused(cfg)
        .map_err(|e| super::CaptureError(capture_open_error(e.to_string())))?;
    Ok((session, route))
}
