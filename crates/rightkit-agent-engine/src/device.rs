//! Device reference. Each task names its target device; offline devices become `waiting-device`.

use serde::{Deserialize, Serialize};

/// Vocabulary version for [`DevicePlatform`].
pub const DEVICE_PLATFORM_SCHEMA_VERSION: u32 = 1;
/// Version of the [`DeviceRef`] record.
pub const DEVICE_REF_SCHEMA_VERSION: u32 = 1;

/// Portable reference to a paired device. Local filesystem paths are never
/// part of it: The contract says local paths are not portable references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub schema_version: u32,
    /// Stable identity established by pairing. Routing and leases key on this,
    /// never on `display_name`.
    pub device_id: String,
    pub platform: DevicePlatform,
    /// Label for display only.
    pub display_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DevicePlatform {
    Mac,
    Windows,
    Linux,
    Ios,
    Android,
}
