//! A device-identity source for a platform that has no device-identity item.

use super::{DeviceIdError, DeviceIdSource};

/// Every read fails with [`DeviceIdError`]. For a host that builds a
/// [`LicenseClient`](super::LicenseClient) on a target where no device-id item exists (the client
/// is then built [`LicenseSetup::Disabled`](super::LicenseSetup::Disabled)); it never reports an
/// identity, so nothing can be activated or leased through it.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableDeviceId;

impl DeviceIdSource for UnavailableDeviceId {
    fn device_id(&self) -> Result<String, DeviceIdError> {
        Err(DeviceIdError)
    }

    fn device_id_hash(&self) -> Result<String, DeviceIdError> {
        Err(DeviceIdError)
    }
}
