//! Android capture notes and helpers (`cfg(target_os = "android")`).
//!
//! Capture on Android is the same cpal path as every other OS: cpal 0.18 hosts AAudio
//! (through the `ndk` crate) when built for `*-linux-android`, so `list_input_devices`,
//! `resolve_device` and `CaptureSession` need no Android fork. What differs is policy:
//!
//! * The app (Kotlin) owns the `RECORD_AUDIO` permission and must have called
//!   `RightKit.init` (which sets the `ndk-context` that cpal's AAudio host reads) before
//!   any capture call. Without the permission AAudio opens but delivers silence or
//!   fails; [`super::capture_open_error`] adds a hint for the failures it recognises.
//! * An AAudio stream that loses its device (headset unplugged, Bluetooth SCO dropped)
//!   is dead and must be reopened, so [`super::CaptureErrorKind::DeviceChanged`] is
//!   terminal on Android (see `CaptureErrorKind::is_terminal`).
//! * A Bluetooth SCO headset offers a narrow-band input (typically 8 or 16 kHz mono)
//!   and a route switch to or from it changes rate and channels. [`sco_route_changed`]
//!   tells the caller to reopen the warm stream even when the saved platform id is
//!   unchanged.
//!
//! Nothing here has run on a device (docs/design/android-minimum.md, row 15).

use super::device::CaptureTransport;
use super::route::CaptureRouteSignature;

/// The manifest permission the host app must hold and request at run time.
pub const RECORD_AUDIO_PERMISSION: &str = "android.permission.RECORD_AUDIO";

/// Highest native rate treated as a Bluetooth SCO (hands-free) narrow-band input.
pub const SCO_MAX_NATIVE_RATE: u32 = 16_000;

/// True when the route is a Bluetooth input at a narrow-band (SCO) rate. A2DP has no
/// input, so any Bluetooth capture route on Android is SCO or BLE audio.
pub fn is_sco_route(route: &CaptureRouteSignature) -> bool {
    route.transport == CaptureTransport::Bluetooth
        && route.native_rate > 0
        && route.native_rate <= SCO_MAX_NATIVE_RATE
}

/// True when moving between `previous` and `current` crosses the SCO boundary (to or
/// from a Bluetooth narrow-band input). The caller reopens the stream; cpal does not
/// renegotiate a running AAudio stream's rate.
pub fn sco_route_changed(
    previous: &Option<CaptureRouteSignature>,
    current: &Option<CaptureRouteSignature>,
) -> bool {
    let was = previous.as_ref().is_some_and(is_sco_route);
    let is = current.as_ref().is_some_and(is_sco_route);
    was != is
}
