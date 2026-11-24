//! Serialisable microphone device list and saved-device refusal for setup commands.
//!
//! The host passes in nothing for listing; for saved-device resolution it passes its own
//! selection function (the device snapshot it builds and the resolver it owns), so this crate
//! needs no knowledge of how a saved microphone setting is encoded.

use crate::capture::{list_input_devices, DeviceInfo};

/// One input device as a renderer-facing record. `id` and `channels` are widened to `i64` so the
/// record crosses a JSON/IPC boundary without platform-width integers; `channels` defaults to `0`
/// when deserialised from a record that lacks it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MicDeviceView {
    /// Index into the device enumeration this record came from.
    pub id: i64,
    /// Human-readable device name.
    pub name: String,
    /// Whether this is the system default input device.
    pub default: bool,
    /// Stable platform device identifier, when the host exposes one.
    pub platform_id: Option<String>,
    /// Native channel count.
    #[serde(default)]
    pub channels: i64,
}

impl From<DeviceInfo> for MicDeviceView {
    fn from(device: DeviceInfo) -> Self {
        Self {
            id: device.id as i64,
            name: device.name,
            default: device.is_default,
            platform_id: device.platform_id,
            channels: device.channels as i64,
        }
    }
}

/// Enumerate input devices as [`MicDeviceView`]s. Blocking: it calls into the OS audio driver, so
/// a UI host runs it on a blocking worker. The error is the enumeration error's text.
pub fn list_input_device_views() -> Result<Vec<MicDeviceView>, String> {
    let devices = list_input_devices().map_err(|e| e.to_string())?;
    Ok(devices.into_iter().map(MicDeviceView::from).collect())
}

/// What a host's saved-microphone resolver decided. The host builds it from its own selection
/// result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedDeviceChoice {
    /// The chosen device id; `None` selects the default device.
    pub device_id: Option<usize>,
    /// `true` when a saved microphone was named but is not among the devices.
    pub fallback: bool,
    /// Label to show for the choice (the saved microphone's name when `fallback` is set).
    pub label: String,
}

/// Resolve a saved microphone against `devices`, refusing when it fell back to the default.
///
/// `snapshot` maps each [`DeviceInfo`] to the host's own device type; `resolve` receives the
/// saved setting and those snapshots and returns the host's decision. A `fallback` decision is an
/// error `"{unavailable_prefix}: {label}"`; otherwise the chosen id is returned. Pass
/// `"saved microphone is unavailable"` as the prefix for the established text.
pub fn resolve_saved_device<S>(
    saved: Option<&str>,
    devices: &[DeviceInfo],
    snapshot: impl Fn(&DeviceInfo) -> S,
    resolve: impl FnOnce(Option<&str>, &[S]) -> SavedDeviceChoice,
    unavailable_prefix: &str,
) -> Result<Option<usize>, String> {
    let snapshots = devices.iter().map(snapshot).collect::<Vec<_>>();
    let choice = resolve(saved, &snapshots);
    if choice.fallback {
        return Err(format!("{unavailable_prefix}: {}", choice.label));
    }
    Ok(choice.device_id)
}
