//! Device enumeration and default-device resolution.
//!
//! Identification model: we expose an `id` that is the *index* into the
//! current `host.input_devices()` enumeration. This is intentionally simple —
//! callers should re-run `list_devices()` before each `start()` if the device
//! set may have changed (hot-swap).

use super::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{DeviceDescription, DeviceType, InterfaceType};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureTransport {
    BuiltIn,
    Bluetooth,
    Usb,
    Wired,
    Virtual,
    Aggregate,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureFormFactor {
    Headset,
    Microphone,
    Other,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: usize,
    pub name: String,
    pub native_rate: u32,
    pub channels: u16,
    pub sample_format: String,
    pub is_default: bool,
    pub transport: CaptureTransport,
    pub form_factor: CaptureFormFactor,
    pub platform_id: Option<String>,
}

fn classify_description(description: &DeviceDescription) -> (CaptureTransport, CaptureFormFactor) {
    let transport = match description.interface_type() {
        InterfaceType::BuiltIn => CaptureTransport::BuiltIn,
        InterfaceType::Bluetooth => CaptureTransport::Bluetooth,
        InterfaceType::Usb => CaptureTransport::Usb,
        InterfaceType::Line => CaptureTransport::Wired,
        InterfaceType::Virtual => CaptureTransport::Virtual,
        InterfaceType::Aggregate => CaptureTransport::Aggregate,
        _ => CaptureTransport::Unknown,
    };
    let form_factor = match description.device_type() {
        DeviceType::Headset | DeviceType::Headphones | DeviceType::HearingAid => {
            CaptureFormFactor::Headset
        }
        DeviceType::Microphone => CaptureFormFactor::Microphone,
        DeviceType::Unknown => CaptureFormFactor::Unknown,
        _ => CaptureFormFactor::Other,
    };
    (transport, form_factor)
}

#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals)]
fn macos_transport(platform_id: &str) -> CaptureTransport {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::{null, NonNull};

    use objc2_core_audio::{
        kAudioDevicePropertyTransportType, kAudioDeviceTransportTypeAggregate,
        kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE,
        kAudioDeviceTransportTypeBuiltIn, kAudioDeviceTransportTypeContinuityCaptureWired,
        kAudioDeviceTransportTypeFireWire, kAudioDeviceTransportTypePCI,
        kAudioDeviceTransportTypeThunderbolt, kAudioDeviceTransportTypeUSB,
        kAudioDeviceTransportTypeVirtual, kAudioHardwarePropertyTranslateUIDToDevice,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
        AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    };
    use objc2_core_foundation::CFString;

    let uid = CFString::from_str(platform_id);
    let uid_ref: *const CFString = &*uid;
    let mut device_id: AudioObjectID = 0;
    let mut device_size = size_of::<AudioObjectID>() as u32;
    let translate = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslateUIDToDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&translate),
            size_of::<*const CFString>() as u32,
            &uid_ref as *const *const CFString as *const c_void,
            NonNull::from(&mut device_size),
            NonNull::from(&mut device_id).cast(),
        )
    };
    if status != 0 || device_id == 0 {
        return CaptureTransport::Unknown;
    }

    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyTransportType,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut transport = 0u32;
    let mut transport_size = size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut transport_size),
            NonNull::from(&mut transport).cast(),
        )
    };
    if status != 0 {
        return CaptureTransport::Unknown;
    }
    match transport {
        kAudioDeviceTransportTypeBluetooth | kAudioDeviceTransportTypeBluetoothLE => {
            CaptureTransport::Bluetooth
        }
        kAudioDeviceTransportTypeBuiltIn | kAudioDeviceTransportTypePCI => {
            CaptureTransport::BuiltIn
        }
        kAudioDeviceTransportTypeUSB => CaptureTransport::Usb,
        kAudioDeviceTransportTypeFireWire
        | kAudioDeviceTransportTypeThunderbolt
        | kAudioDeviceTransportTypeContinuityCaptureWired => CaptureTransport::Wired,
        kAudioDeviceTransportTypeVirtual => CaptureTransport::Virtual,
        kAudioDeviceTransportTypeAggregate => CaptureTransport::Aggregate,
        _ => CaptureTransport::Unknown,
    }
}

pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    // CPAL Device implements logical equality using platform device identity.
    // Never infer default status from names: duplicate USB/headset names are
    // common & previously marked every collision as default.
    let default_device = host.default_input_device();
    let default_platform_id = default_device
        .as_ref()
        .and_then(|device| device.id().ok())
        .map(|id| id.id().to_string());

    let mut out = Vec::new();
    let devices = host
        .input_devices()
        .map_err(|e| anyhow!("cpal input_devices() failed: {e}"))?;

    for (idx, device) in devices.enumerate() {
        let description = device.description().ok();
        let name = description
            .as_ref()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|| format!("device-{idx}"));
        let platform_id = device.id().ok().map(|id| id.id().to_string());
        let (transport, form_factor) = description
            .as_ref()
            .map(classify_description)
            .unwrap_or((CaptureTransport::Unknown, CaptureFormFactor::Unknown));
        #[cfg(target_os = "macos")]
        let transport = platform_id
            .as_deref()
            .map(macos_transport)
            .unwrap_or(transport);
        let (native_rate, channels, sample_format) = match device.default_input_config() {
            Ok(cfg) => (
                cfg.sample_rate(),
                cfg.channels(),
                format!("{:?}", cfg.sample_format()),
            ),
            Err(_) => (0u32, 0u16, "Unknown".to_string()),
        };
        let handles_equal = default_device
            .as_ref()
            .is_some_and(|default| default == &device);
        let is_default = default_device_matches(
            default_platform_id.as_deref(),
            platform_id.as_deref(),
            handles_equal,
        );
        out.push(DeviceInfo {
            id: idx,
            name,
            native_rate,
            channels,
            sample_format,
            is_default,
            transport,
            form_factor,
            platform_id,
        });
    }
    Ok(out)
}

fn default_device_matches(
    default_platform_id: Option<&str>,
    candidate_platform_id: Option<&str>,
    handles_equal: bool,
) -> bool {
    match (default_platform_id, candidate_platform_id) {
        (Some(default_id), Some(candidate_id)) => default_id == candidate_id,
        _ => handles_equal,
    }
}

/// Resolve a cpal Device by stable platform id when available, otherwise by
/// legacy enumeration id (None = default). Re-enumerates each call so we
/// always pick up hot-swap state.
///
/// A supplied platform id is authoritative: if it is no longer present, this
/// returns an error instead of silently switching to another microphone.
pub fn resolve_device(id: Option<usize>, platform_id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(platform_id) = platform_id {
        let devices = host
            .input_devices()
            .map_err(|e| anyhow!("cpal input_devices() failed: {e}"))?;
        return devices
            .filter_map(|device| {
                let candidate_id = device.id().ok().map(|id| id.id().to_string());
                platform_id_matches(candidate_id.as_deref(), platform_id).then_some(device)
            })
            .next()
            .ok_or_else(|| anyhow!("input device platform id '{platform_id}' is unavailable"));
    }

    match id {
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device available")),
        Some(idx) => {
            let mut devices = host
                .input_devices()
                .map_err(|e| anyhow!("cpal input_devices() failed: {e}"))?;
            devices
                .nth(idx)
                .ok_or_else(|| anyhow!("input device index {idx} out of range"))
        }
    }
}

fn platform_id_matches(candidate_platform_id: Option<&str>, requested_platform_id: &str) -> bool {
    candidate_platform_id == Some(requested_platform_id)
}
